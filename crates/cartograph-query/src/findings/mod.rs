//! Findings: what is wrong or risky in the architecture, each with severity, a short title,
//! evidence (the edges and paths that prove it) and location ids.
//!
//! Computed on the fly from the graph and the config (never stored, so they can never be
//! stale against `cartograph.toml`). `violation` marks the findings that break a rule of
//! `cartograph.toml` (layers, cycles in `no_cycles` scopes, `max_fan_out`): `check` fails on
//! those and only those.

mod chains;
mod coupling;
mod cycles;
pub mod kind;
mod layers;
mod metrics;
mod orphans;

pub(crate) use cycles::WITHIN_PACKAGE;
pub use kind::{FindingKind, Severity};

use crate::config::Config;
use crate::graph::{EdgeRef, FileIx, RepoGraph};
use crate::kind::FileKind;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

/// Most evidence lines and edges a finding keeps (the rest is counted).
pub const MAX_EVIDENCE: usize = 12;
/// Most location ids a finding keeps (the rest is counted).
pub const MAX_NODES: usize = 60;
/// Most edges a finding keeps (the rest is counted in `edges_more`).
pub const MAX_EDGES: usize = MAX_EVIDENCE * 4;

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// Stable fingerprint: kind plus its main location (`cycle:file:src/a.ts`).
    pub id: String,
    pub kind: FindingKind,
    pub severity: Severity,
    /// Breaks a rule of `cartograph.toml`.
    pub violation: bool,
    /// One short line.
    pub title: String,
    /// Location ids (files and dirs).
    pub nodes: Vec<String>,
    /// Locations left out of `nodes`.
    #[serde(skip_serializing_if = "is_zero")]
    pub nodes_more: usize,
    /// File → file edges that prove it (at most [`MAX_EDGES`]).
    pub edges: Vec<EdgeRef>,
    /// Edges left out of `edges`: when not zero, `edges` is a sample and the nodes are the
    /// better place to mark the finding.
    #[serde(skip_serializing_if = "is_zero")]
    pub edges_more: usize,
    /// Human-readable proof, one line each (`src/a.ts:12 → src/b.ts`).
    pub evidence: Vec<String>,
    /// Evidence lines left out.
    #[serde(skip_serializing_if = "is_zero")]
    pub evidence_more: usize,
    /// The numbers behind it (fan-in, LOC, instability…).
    pub metrics: BTreeMap<&'static str, f64>,
    /// Locations and file pairs past the caps, so `nodes_more` and `edges_more` count each
    /// one once however often a detector offers it.
    #[serde(skip)]
    dropped_nodes: HashSet<String>,
    #[serde(skip)]
    dropped_edges: HashSet<(FileIx, FileIx)>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl Finding {
    pub(crate) fn new(kind: FindingKind, severity: Severity, key: &str, title: String) -> Finding {
        Finding {
            id: format!("{}:{key}", kind.as_str()),
            kind,
            severity,
            violation: false,
            title,
            nodes: Vec::new(),
            nodes_more: 0,
            edges: Vec::new(),
            edges_more: 0,
            evidence: Vec::new(),
            evidence_more: 0,
            metrics: BTreeMap::new(),
            dropped_nodes: HashSet::new(),
            dropped_edges: HashSet::new(),
        }
    }

    pub(crate) fn node(&mut self, id: String) {
        if self.nodes.contains(&id) {
            return;
        }
        if self.nodes.len() < MAX_NODES {
            self.nodes.push(id);
        } else if self.dropped_nodes.insert(id) {
            self.nodes_more += 1;
        }
    }

    pub(crate) fn line(&mut self, text: String) {
        if self.evidence.len() < MAX_EVIDENCE {
            self.evidence.push(text);
        } else {
            self.evidence_more += 1;
        }
    }

    pub(crate) fn edge(&mut self, g: &RepoGraph, src: FileIx, dst: FileIx) {
        let e = g.edge_ref(src, dst);
        if self.edges.iter().any(|x| x.src == e.src && x.dst == e.dst) {
            return;
        }
        if self.edges.len() < MAX_EDGES {
            self.edges.push(e);
        } else if self.dropped_edges.insert((src, dst)) {
            self.edges_more += 1;
        }
    }

    pub(crate) fn metric(&mut self, key: &'static str, v: f64) {
        self.metrics.insert(key, v);
    }

    /// Whether `id` is one of its locations or an end of one of its edges.
    pub fn touches(&self, id: &str) -> bool {
        self.nodes.iter().any(|n| n == id) || self.edges.iter().any(|e| e.src == id || e.dst == id)
    }
}

/// Context shared by the detectors.
pub(crate) struct Ctx<'a> {
    pub g: &'a RepoGraph,
    pub cfg: &'a Config,
    /// Files that take part: not tests, not ignored by the config.
    pub active: Vec<bool>,
    /// Dependencies between active files (layer rules, `max_fan_out`, orphans).
    pub adj: crate::algo::Adj,
    /// [`Ctx::adj`] without type-only dependencies: what loads at run time. Cycles, folder
    /// cycles and Stable Dependencies look at this (dependency-cruiser's
    /// `tsPreCompilationDeps: false`): a type import cannot make a load-order cycle.
    pub rt_adj: crate::algo::Adj,
    /// Run-time dependencies between active code files (no scripts, config or tests): what
    /// size and centrality measures (god modules, hubs) look at, so a pile of tools,
    /// scripts or type imports does not make a file look central.
    pub code_adj: crate::algo::Adj,
    /// Every dependency between active code files, type-only ones included (deep chains).
    pub chain_adj: crate::algo::Adj,
    /// What layer rules judge: [`Ctx::adj`], plus bridges between active files with
    /// `[rules] layers_count_bridges`.
    pub layer_adj: crate::algo::Adj,
}

/// Files that take part in the findings: not tests, not ignored by the config. Tests depend
/// on code, never the other way round: they would only add noise (cycles through test
/// helpers, fan-in from test suites).
fn active_files(g: &RepoGraph, cfg: &Config) -> Vec<bool> {
    g.files
        .iter()
        .map(|f| f.kind != FileKind::Test && !cfg.ignore.matches(&f.path))
        .collect()
}

/// Dependencies between active files, without a module root's declarations of its own
/// submodules (see [`declares`]); without type-only ones when `runtime`.
fn active_deps(g: &RepoGraph, active: &[bool], runtime: bool) -> crate::algo::Adj {
    g.out
        .iter()
        .enumerate()
        .map(|(i, deps)| {
            if !active[i] {
                return Vec::new();
            }
            deps.iter()
                .filter(|d| !(runtime && d.type_only))
                .map(|d| d.file)
                .filter(|&d| active[d as usize] && !declares(g, i as FileIx, d))
                .collect()
        })
        .collect()
}

/// The dependencies the rules of `cartograph.toml` look at (layers, `max_fan_out`), as the
/// detectors see them ([`Ctx::adj`]): what `diff` and `check --base` compare to tell
/// whether a violation got worse.
pub(crate) fn rule_deps(g: &RepoGraph, cfg: &Config) -> crate::algo::Adj {
    let active = active_files(g, cfg);
    layer_deps(g, cfg, &active, active_deps(g, &active, false))
}

/// `adj` plus the bridges between active files when `[rules] layers_count_bridges` is on.
fn layer_deps(
    g: &RepoGraph,
    cfg: &Config,
    active: &[bool],
    mut adj: crate::algo::Adj,
) -> crate::algo::Adj {
    if cfg.layers_count_bridges {
        for (i, bs) in g.bridge_out.iter().enumerate() {
            if !active[i] {
                continue;
            }
            for b in bs {
                if active[b.file as usize] && !adj[i].contains(&b.file) {
                    adj[i].push(b.file);
                }
            }
            adj[i].sort_unstable();
        }
    }
    adj
}

impl<'a> Ctx<'a> {
    pub fn new(g: &'a RepoGraph, cfg: &'a Config) -> Ctx<'a> {
        let active = active_files(g, cfg);
        let deps_of = |runtime: bool| active_deps(g, &active, runtime);
        let code_only = |adj: &crate::algo::Adj| -> crate::algo::Adj {
            adj.iter()
                .enumerate()
                .map(|(i, deps)| {
                    if g.files[i].kind != FileKind::Code {
                        return Vec::new();
                    }
                    deps.iter()
                        .copied()
                        .filter(|&d| g.files[d as usize].kind == FileKind::Code)
                        .collect()
                })
                .collect()
        };
        let adj = deps_of(false);
        let rt_adj = deps_of(true);
        let code_adj = code_only(&rt_adj);
        let chain_adj = code_only(&adj);
        let layer_adj = layer_deps(g, cfg, &active, adj.clone());
        Ctx {
            g,
            cfg,
            active,
            adj,
            rt_adj,
            code_adj,
            chain_adj,
            layer_adj,
        }
    }
}

/// Whether `a → b` is a module root declaring or re-exporting one of its own submodules
/// (Rust `lib.rs`/`main.rs`/`mod.rs`/`foo.rs` → `foo/…`, Python `__init__.py` → files of its
/// package). That is structure, not a dependency: Rust and Python modules use their parent
/// freely, and counting both directions would make every crate and package one big cycle.
/// TS/JS barrels (`index.ts`) are not exempt: cycles through them break at run time.
pub(crate) fn declares(g: &RepoGraph, a: FileIx, b: FileIx) -> bool {
    let fa = &g.files[a as usize];
    let pb = g.path(b);
    let scope = match (fa.lang.as_deref(), fa.name.as_str()) {
        (Some("rust"), "lib.rs" | "main.rs" | "mod.rs") | (Some("python"), "__init__.py") => {
            match fa.path.rsplit_once('/') {
                Some((dir, _)) => format!("{dir}/"),
                None => String::new(),
            }
        }
        (Some("rust"), _) => match fa.path.strip_suffix(".rs") {
            Some(stem) => format!("{stem}/"),
            None => return false,
        },
        _ => return false,
    };
    pb.starts_with(&scope)
}

/// Every finding of the repo, sorted: violations first, then by severity, kind and id.
pub fn analyze(g: &RepoGraph, cfg: &Config, git: &crate::history::GitTable) -> Vec<Finding> {
    let cx = Ctx::new(g, cfg);
    let mut out = Vec::new();
    let on = |k| cfg.enabled(k);
    if on(FindingKind::LayerViolation) {
        out.extend(layers::layer_violations(&cx));
    }
    if on(FindingKind::FanOutLimit) {
        out.extend(layers::fan_out_limits(&cx));
    }
    if on(FindingKind::Cycle) {
        out.extend(cycles::file_cycles(&cx));
    }
    if on(FindingKind::FolderCycle) {
        out.extend(cycles::folder_cycles(&cx));
    }
    if on(FindingKind::GodModule) {
        out.extend(metrics::god_modules(&cx));
    }
    if on(FindingKind::Hub) {
        out.extend(metrics::hubs(&cx));
    }
    if on(FindingKind::Sdp) {
        out.extend(metrics::sdp(&cx));
    }
    if on(FindingKind::Orphan) {
        out.extend(orphans::orphans(&cx));
    }
    if on(FindingKind::DeepChain) {
        out.extend(chains::deep_chains(&cx));
    }
    if on(FindingKind::HiddenCoupling) {
        out.extend(coupling::hidden_coupling(&cx, git));
    }
    // Ids are unique: a repeated one gets `#2`, `#3`…
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for f in &mut out {
        let n = seen.entry(f.id.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            f.id = format!("{}#{n}", f.id);
        }
    }
    out.sort_by(|a, b| {
        (!a.violation, a.severity, a.kind, &a.id).cmp(&(!b.violation, b.severity, b.kind, &b.id))
    });
    out
}

/// Counts per kind and severity.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    pub total: usize,
    pub violations: usize,
    pub by_severity: BTreeMap<&'static str, usize>,
    pub by_kind: BTreeMap<&'static str, usize>,
}

pub fn summarize<'a>(findings: impl IntoIterator<Item = &'a Finding>) -> Summary {
    let mut s = Summary::default();
    for f in findings {
        s.total += 1;
        if f.violation {
            s.violations += 1;
        }
        *s.by_severity.entry(f.severity.as_str()).or_default() += 1;
        *s.by_kind.entry(f.kind.as_str()).or_default() += 1;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartograph_core::{Confidence, Edge, EdgeKind, Graph, Node, NodeKind, node_id};

    fn node(kind: NodeKind, path: &str) -> Node {
        Node {
            id: node_id(kind, path),
            kind,
            path: path.to_string(),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            lang: (kind == NodeKind::File).then(|| "typescript".to_string()),
            loc: 1,
            bytes: 1,
            hash: None,
            meta: serde_json::Value::Null,
        }
    }

    /// `hub.ts` imports `f0.ts`…`f{n-1}.ts`.
    fn star(n: usize) -> RepoGraph {
        let mut g = Graph::default();
        g.nodes.push(node(NodeKind::Repo, ""));
        g.nodes.push(node(NodeKind::File, "hub.ts"));
        for i in 0..n {
            let p = format!("f{i}.ts");
            g.nodes.push(node(NodeKind::File, &p));
            g.edges.push(Edge::new(
                node_id(NodeKind::File, "hub.ts"),
                node_id(NodeKind::File, &p),
                EdgeKind::Import,
                Confidence::Resolved,
            ));
        }
        RepoGraph::build(&g)
    }

    #[test]
    fn edges_and_nodes_past_the_cap_are_counted_once() {
        let n = MAX_EDGES + 10;
        let g = star(n);
        let hub = g.file_by_path("hub.ts").unwrap();
        let leaves: Vec<FileIx> = (0..n)
            .map(|i| g.file_by_path(&format!("f{i}.ts")).unwrap())
            .collect();
        let mut f = Finding::new(FindingKind::Cycle, Severity::Info, "x", String::new());
        // Every pair twice, as a detector that meets an edge on two paths does.
        for _ in 0..2 {
            for &l in &leaves {
                f.edge(&g, hub, l);
            }
        }
        assert_eq!(f.edges.len(), MAX_EDGES);
        assert_eq!(f.edges.len() + f.edges_more, n);
        let ids: Vec<String> = (0..MAX_NODES + 5).map(|i| format!("file:n{i}")).collect();
        for _ in 0..2 {
            for id in &ids {
                f.node(id.clone());
            }
        }
        assert_eq!(f.nodes.len(), MAX_NODES);
        assert_eq!(f.nodes.len() + f.nodes_more, MAX_NODES + 5);
        // The bookkeeping does not reach the JSON.
        let json = serde_json::to_value(&f).unwrap();
        assert_eq!(json["edges_more"], 10);
        assert_eq!(json["nodes_more"], 5);
        let keys: Vec<&String> = json.as_object().unwrap().keys().collect();
        assert!(!keys.iter().any(|k| k.starts_with("dropped")), "{keys:?}");
    }

    /// `ui/a.ts` imports `core/b.ts`, and `core/b.ts` reaches `ui/a.ts` over a bridge.
    fn bridged() -> RepoGraph {
        let mut g = Graph::default();
        g.nodes.push(node(NodeKind::Repo, ""));
        for p in ["ui", "core"] {
            g.nodes.push(node(NodeKind::Dir, p));
        }
        for p in ["ui/a.ts", "core/b.ts"] {
            g.nodes.push(node(NodeKind::File, p));
        }
        let f = |p: &str| node_id(NodeKind::File, p);
        g.edges.push(Edge::new(
            f("ui/a.ts"),
            f("core/b.ts"),
            EdgeKind::Import,
            Confidence::Exact,
        ));
        g.edges.push(Edge {
            weight: 2,
            ..Edge::new(
                f("core/b.ts"),
                f("ui/a.ts"),
                EdgeKind::Bridge,
                Confidence::Resolved,
            )
        });
        RepoGraph::build(&g)
    }

    #[test]
    fn a_bridge_is_no_import_for_cycles_or_layers_unless_layers_count_it() {
        let g = bridged();
        let (a, b) = (
            g.file_by_path("ui/a.ts").unwrap(),
            g.file_by_path("core/b.ts").unwrap(),
        );
        assert_eq!(g.bridge(b, a).map(|d| d.weight), Some(2));
        assert!(g.dep(b, a).is_none(), "a bridge is not a Dep");
        assert_eq!(g.fan_in(a), 0);
        let rules = "[[layers]]
name = \"ui\"
paths = [\"ui\"]
[[layers]]
name = \"core\"
paths = [\"core\"]
forbid = [\"ui\"]
";
        let cfg = Config::parse(rules, "cartograph.toml").unwrap();
        let found = analyze(&g, &cfg, &crate::history::GitTable::default());
        assert!(
            !found
                .iter()
                .any(|f| matches!(f.kind, FindingKind::Cycle | FindingKind::LayerViolation)),
            "{found:#?}"
        );
        let counted = Config::parse(
            &format!(
                "{rules}[rules]
layers_count_bridges = true
"
            ),
            "cartograph.toml",
        )
        .unwrap();
        let found = analyze(&g, &counted, &crate::history::GitTable::default());
        let v: Vec<&Finding> = found
            .iter()
            .filter(|f| f.kind == FindingKind::LayerViolation)
            .collect();
        assert_eq!(v.len(), 1, "{found:#?}");
        assert!(v[0].id.ends_with("core->ui"), "{}", v[0].id);
        assert!(!found.iter().any(|f| f.kind == FindingKind::Cycle));
        assert_eq!(rule_deps(&g, &counted)[b as usize], vec![a]);
        assert!(rule_deps(&g, &cfg)[b as usize].is_empty());
    }
}
