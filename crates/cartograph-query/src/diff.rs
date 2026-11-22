//! `diff(base, head)`: what a change does to the architecture, from two indexed states of
//! the same repo (a git base and the working tree, in practice). Both sides must be built
//! with the same config (and the same git history, if any), so a difference is a difference
//! in the code, never in the rules.
//!
//! - **Findings** are matched by their stable id; cycles also by shared files, because a
//!   cycle's id names one of its files and may change when the cycle does. A head finding
//!   with no match is *new*, a base finding with no match is *resolved*.
//! - A rule violation that exists on both sides is *worse* when it rests on a file → file
//!   dependency the base does not have (one more import across a forbidden layer pair, a
//!   new edge in a cycle's loop, one more dependency of a file over `max_fan_out`), among
//!   the dependencies the rules count (no tests, no files the config ignores). New and
//!   worse violations are what the change *introduces*; `check --base` fails on those only.
//! - **Dependencies between modules**: a module is a layer of `cartograph.toml` when the
//!   file is in one, else the package (a manifest below the root) that holds it, else its
//!   top folder, where a folder holding at least half of the repo's files is split into its
//!   subfolders (so `src/` becomes `src/a/`, `src/b/`).
//! - **Unresolved imports** are matched by file, specifier and reason (not line).
//! - **Files** added and removed, by path.

use crate::Repo;
use crate::algo::Adj;
use crate::budget::{Answer, clip, fit, limits};
use crate::findings::{Finding, FindingKind, rule_deps};
use crate::graph::{FileIx, ROOT, RepoGraph};
use crate::list::{Brief, render_brief};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// How a head finding compares with the base.
#[derive(Debug, Clone)]
pub enum Change {
    /// No matching finding in the base.
    New,
    /// A violation in both, resting on dependencies the base does not have (head file ids).
    Worse(Vec<(FileIx, FileIx)>),
    /// In both, nothing new under it.
    Same,
}

/// Every finding of both sides, compared.
pub struct Compared<'a> {
    /// Head findings with their change, in the head's order (violations first).
    pub head: Vec<(&'a Finding, Change)>,
    /// Base findings with no match in the head.
    pub resolved: Vec<&'a Finding>,
}

impl Compared<'_> {
    /// Violations the change introduces (new or worse).
    pub fn introduced(&self) -> impl Iterator<Item = &(&Finding, Change)> {
        self.head
            .iter()
            .filter(|(f, c)| f.violation && !matches!(c, Change::Same))
    }
}

fn is_cycle(k: FindingKind) -> bool {
    matches!(k, FindingKind::Cycle | FindingKind::FolderCycle)
}

/// Compare the findings of `head` with those of `base`.
pub fn compare<'a>(base: &'a Repo, head: &'a Repo) -> Compared<'a> {
    let (bf, hf) = (base.findings(), head.findings());
    // The dependencies the rules count (no tests, no ignored files), as the detectors see
    // them: one the rules do not count never makes a violation worse.
    let base_adj = rule_deps(&base.graph, &base.config);
    let head_adj = rule_deps(&head.graph, &head.config);
    let base_deps = dep_set(&base.graph, &base_adj);
    let ids =
        |fs: &'a [Finding]| -> HashSet<&'a str> { fs.iter().map(|f| f.id.as_str()).collect() };
    let (base_ids, head_ids) = (ids(bf), ids(hf));
    // Cycle nodes of each side, by kind: a cycle matches one of the other side that shares
    // a node.
    let cycle_nodes = |fs: &'a [Finding]| -> HashSet<(FindingKind, &'a str)> {
        fs.iter()
            .filter(|f| is_cycle(f.kind))
            .flat_map(|f| f.nodes.iter().map(move |n| (f.kind, n.as_str())))
            .collect()
    };
    let (base_cyc, head_cyc) = (cycle_nodes(bf), cycle_nodes(hf));
    let matched = |f: &Finding, ids: &HashSet<&str>, cyc: &HashSet<(FindingKind, &str)>| {
        ids.contains(f.id.as_str())
            || (is_cycle(f.kind) && f.nodes.iter().any(|n| cyc.contains(&(f.kind, n.as_str()))))
    };
    let head_out = hf
        .iter()
        .map(|f| {
            let change = if !matched(f, &base_ids, &base_cyc) {
                Change::New
            } else if f.violation {
                let fresh: Vec<(FileIx, FileIx)> = edges_of(head, &head_adj, f)
                    .into_iter()
                    .filter(|&(a, b)| {
                        !base_deps.contains(&(head.graph.path(a), head.graph.path(b)))
                    })
                    .collect();
                if fresh.is_empty() {
                    Change::Same
                } else {
                    Change::Worse(fresh)
                }
            } else {
                Change::Same
            };
            (f, change)
        })
        .collect();
    let resolved = bf
        .iter()
        .filter(|f| !matched(f, &head_ids, &head_cyc))
        .collect();
    Compared {
        head: head_out,
        resolved,
    }
}

/// The file → file dependencies of `adj` (a graph's [`rule_deps`]), by path.
fn dep_set<'a>(g: &'a RepoGraph, adj: &Adj) -> HashSet<(&'a str, &'a str)> {
    adj.iter()
        .enumerate()
        .flat_map(|(a, deps)| deps.iter().map(move |&d| (g.path(a as FileIx), g.path(d))))
        .collect()
}

/// The dependencies a finding rests on, as head file ids, among `adj` (the head's
/// [`rule_deps`]). Findings keep a capped sample of edges; a layer violation past the cap
/// is worked out again from the config, a fan-out violation is every dependency of its file.
fn edges_of(repo: &Repo, adj: &Adj, f: &Finding) -> Vec<(FileIx, FileIx)> {
    let g = &repo.graph;
    let file = |id: &str| id.strip_prefix("file:").and_then(|p| g.file_by_path(p));
    match f.kind {
        FindingKind::FanOutLimit => {
            let Some(a) = f.id.strip_prefix("fan_out_limit:").and_then(file) else {
                return Vec::new();
            };
            adj[a as usize].iter().map(|&d| (a, d)).collect()
        }
        FindingKind::LayerViolation if f.edges_more > 0 => {
            let cfg = &repo.config;
            let pair =
                f.id.strip_prefix("layer_violation:")
                    .and_then(|p| p.split_once("->"));
            let layer = |name: &str| cfg.layers.iter().position(|l| l.name == name);
            let Some((Some(la), Some(lb))) = pair.map(|(a, b)| (layer(a), layer(b))) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            for (a, deps) in adj.iter().enumerate() {
                if cfg.layer_of(g.path(a as FileIx)) != Some(la) {
                    continue;
                }
                for &d in deps {
                    if cfg.layer_of(g.path(d)) == Some(lb) {
                        out.push((a as FileIx, d));
                    }
                }
            }
            out
        }
        _ => f
            .edges
            .iter()
            .filter_map(|e| Some((file(&e.src)?, file(&e.dst)?)))
            .collect(),
    }
}

/// A finding for a listing; a worse violation shows only its new dependencies.
pub fn brief(repo: &Repo, f: &Finding, change: &Change, max_evidence: usize) -> Brief {
    let mut b = Brief::of(f, max_evidence);
    if let Change::Worse(fresh) = change {
        let n = fresh.len();
        b.title = clip(
            &format!(
                "{} — {n} new {} since the base",
                f.title,
                if n == 1 { "dependency" } else { "dependencies" }
            ),
            200,
        );
        b.evidence = fresh
            .iter()
            .take(max_evidence)
            .map(|&(a, c)| clip(&repo.graph.edge_evidence(a, c), 200))
            .collect();
        b.evidence_more = n.saturating_sub(max_evidence);
    }
    b
}

/// A finding of the base that is gone.
#[derive(Debug, Clone, Serialize)]
pub struct Gone {
    pub id: String,
    pub violation: bool,
    pub title: String,
}

/// Imports from one module to another.
#[derive(Debug, Clone, Serialize)]
pub struct ModuleEdge {
    pub from: String,
    pub to: String,
    /// File → file dependencies in the base and in the head.
    pub base: usize,
    pub head: usize,
    /// One of them (`a.ts:3 → b.ts`), for a new pair.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Unresolved {
    pub file: String,
    pub line: u32,
    pub specifier: String,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffResult {
    pub base: String,
    pub head: String,
    /// `layers` (modules are the layers of cartograph.toml, folders for the rest) or
    /// `folders`.
    pub modules: &'static str,
    pub files_added: usize,
    pub files_removed: usize,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// Rule violations the change introduces (new, or worse with only the new evidence).
    pub violations: Vec<Brief>,
    pub violations_more: usize,
    /// Other new findings.
    pub new: Vec<Brief>,
    pub new_more: usize,
    /// New findings by kind (violations not included).
    pub new_by_kind: BTreeMap<String, usize>,
    pub resolved: Vec<Gone>,
    pub resolved_more: usize,
    pub resolved_by_kind: BTreeMap<String, usize>,
    /// Findings on both sides with nothing new.
    pub unchanged: usize,
    pub edges_new: Vec<ModuleEdge>,
    pub edges_new_more: usize,
    pub edges_removed: Vec<ModuleEdge>,
    pub edges_removed_more: usize,
    /// Module pairs on both sides whose number of dependencies changed.
    pub edges_changed: Vec<ModuleEdge>,
    pub edges_changed_more: usize,
    pub unresolved_new: Vec<Unresolved>,
    pub unresolved_new_more: usize,
    pub unresolved_removed: Vec<Unresolved>,
    pub unresolved_removed_more: usize,
}

impl DiffResult {
    /// Nothing changed that the diff tracks.
    pub fn is_empty(&self) -> bool {
        self.files_added + self.files_removed == 0
            && self.violations.is_empty()
            && self.new.is_empty()
            && self.resolved.is_empty()
            && self.edges_new.is_empty()
            && self.edges_removed.is_empty()
            && self.edges_changed.is_empty()
            && self.unresolved_new.is_empty()
            && self.unresolved_removed.is_empty()
    }
}

/// How files map to modules.
struct Modules {
    layers: bool,
    /// Folders of packages below the root, deepest first.
    packages: Vec<String>,
    /// Folders split into their subfolders (`""` = the root, always split).
    split: HashSet<String>,
}

/// A folder holding at least this share of the files is split into its subfolders.
const SPLIT_SHARE: f64 = 0.5;
/// Deepest folder level that is split.
const SPLIT_DEPTH: u32 = 3;

impl Modules {
    fn of(repo: &Repo) -> Modules {
        let g = &repo.graph;
        let total = g.files.len().max(1) as f64;
        let mut split = HashSet::from([String::new()]);
        let mut stack = vec![ROOT];
        while let Some(d) = stack.pop() {
            for &c in &g.dirs[d as usize].dirs {
                let node = &g.dirs[c as usize];
                if node.depth <= SPLIT_DEPTH && node.file_count as f64 >= SPLIT_SHARE * total {
                    split.insert(node.path.clone());
                    stack.push(c);
                }
            }
        }
        let mut packages: Vec<String> = g
            .package_dirs()
            .filter(|d| !d.is_empty())
            .map(str::to_string)
            .collect();
        packages.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        Modules {
            layers: !repo.config.layers.is_empty(),
            packages,
            split,
        }
    }

    /// The layer of the file, else the package (below the root) holding it, else its top
    /// folder (split as described in the module docs).
    fn module(&self, repo: &Repo, path: &str) -> String {
        if self.layers
            && let Some(l) = repo.config.layer_of(path)
        {
            return repo.config.layers[l].name.clone();
        }
        if let Some(p) = self.packages.iter().find(|p| {
            path.strip_prefix(p.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        }) {
            return format!("{p}/");
        }
        let mut acc = String::new();
        let mut rest = path;
        while let Some((seg, tail)) = rest.split_once('/') {
            let next = if acc.is_empty() {
                seg.to_string()
            } else {
                format!("{acc}/{seg}")
            };
            if !self.split.contains(&next) {
                return format!("{next}/");
            }
            acc = next;
            rest = tail;
        }
        if acc.is_empty() {
            "./".to_string()
        } else {
            format!("{acc}/")
        }
    }

    /// Dependencies per module pair (different modules only), with one example each.
    fn edges(&self, repo: &Repo) -> BTreeMap<(String, String), (usize, String)> {
        let g = &repo.graph;
        let mods: Vec<String> = g.files.iter().map(|f| self.module(repo, &f.path)).collect();
        let mut out: BTreeMap<(String, String), (usize, String)> = BTreeMap::new();
        for (a, deps) in g.out.iter().enumerate() {
            for d in deps {
                let (ma, mb) = (&mods[a], &mods[d.file as usize]);
                if ma == mb {
                    continue;
                }
                let e = out
                    .entry((ma.clone(), mb.clone()))
                    .or_insert_with(|| (0, g.edge_evidence(a as FileIx, d.file)));
                e.0 += 1;
            }
        }
        out
    }
}

fn unresolved_keys(repo: &Repo) -> BTreeMap<(&str, &str, &'static str), u32> {
    let mut out = BTreeMap::new();
    for u in &repo.unresolved {
        out.entry((u.file.as_str(), u.specifier.as_str(), u.reason.as_str()))
            .or_insert(u.line);
    }
    out
}

/// Items tried by [`fit`], largest first.
const STEPS: &[usize] = &[12, 10, 8, 6, 5, 4, 3, 2, 1];

/// `diff`: what changed from `base` to `head` (see the module docs). Labels name the sides
/// (`main (1a2b3c4d)`, `working tree`).
pub fn diff(base: &Repo, head: &Repo, base_label: &str, head_label: &str) -> Answer<DiffResult> {
    let compared = compare(base, head);
    let mut violations: Vec<(&Finding, &Change)> = Vec::new();
    let mut new: Vec<&Finding> = Vec::new();
    let mut new_by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut unchanged = 0;
    for (f, c) in &compared.head {
        if matches!(c, Change::New) && !f.violation {
            *new_by_kind.entry(f.kind.as_str().to_string()).or_default() += 1;
        }
        match c {
            _ if f.violation && !matches!(c, Change::Same) => violations.push((f, c)),
            Change::New => new.push(f),
            _ => unchanged += 1,
        }
    }
    let mut resolved_by_kind: BTreeMap<String, usize> = BTreeMap::new();
    for f in &compared.resolved {
        *resolved_by_kind
            .entry(f.kind.as_str().to_string())
            .or_default() += 1;
    }

    // Files.
    let paths =
        |r: &Repo| -> BTreeSet<String> { r.graph.files.iter().map(|f| f.path.clone()).collect() };
    let (bp, hp) = (paths(base), paths(head));
    let added: Vec<String> = hp.difference(&bp).cloned().collect();
    let removed: Vec<String> = bp.difference(&hp).cloned().collect();

    // Module edges, with the head's module split applied to both sides.
    let modules = Modules::of(head);
    let (be, he) = (modules.edges(base), modules.edges(head));
    let mut edges_new = Vec::new();
    let mut edges_changed = Vec::new();
    for ((a, b), (n, example)) in &he {
        match be.get(&(a.clone(), b.clone())) {
            None => edges_new.push(ModuleEdge {
                from: a.clone(),
                to: b.clone(),
                base: 0,
                head: *n,
                example: Some(example.clone()),
            }),
            Some((m, _)) if m != n => edges_changed.push(ModuleEdge {
                from: a.clone(),
                to: b.clone(),
                base: *m,
                head: *n,
                example: None,
            }),
            Some(_) => {}
        }
    }
    let edges_removed: Vec<ModuleEdge> = be
        .iter()
        .filter(|(k, _)| !he.contains_key(*k))
        .map(|((a, b), (n, _))| ModuleEdge {
            from: a.clone(),
            to: b.clone(),
            base: *n,
            head: 0,
            example: None,
        })
        .collect();
    let weight = |e: &ModuleEdge| std::cmp::Reverse(e.base.abs_diff(e.head));
    edges_new.sort_by_key(weight);
    edges_changed.sort_by_key(weight);
    let mut edges_removed = edges_removed;
    edges_removed.sort_by_key(weight);

    // Unresolved imports.
    let (bu, hu) = (unresolved_keys(base), unresolved_keys(head));
    let unres = |from: &BTreeMap<(&str, &str, &'static str), u32>,
                 other: &BTreeMap<(&str, &str, &'static str), u32>| {
        from.iter()
            .filter(|(k, _)| !other.contains_key(*k))
            .map(|((file, spec, reason), line)| Unresolved {
                file: file.to_string(),
                line: *line,
                specifier: clip(spec, 120),
                reason,
            })
            .collect::<Vec<_>>()
    };
    let (unresolved_new, unresolved_removed) = (unres(&hu, &bu), unres(&bu, &hu));

    let modules_kind = if modules.layers { "layers" } else { "folders" };
    fit(
        limits::DIFF,
        STEPS,
        |limit| {
            let take = |n: usize| limit.min(n);
            DiffResult {
                base: base_label.to_string(),
                head: head_label.to_string(),
                modules: modules_kind,
                files_added: added.len(),
                files_removed: removed.len(),
                added: added.iter().take(take(5)).cloned().collect(),
                removed: removed.iter().take(take(5)).cloned().collect(),
                violations: violations
                    .iter()
                    .take(limit)
                    .map(|(f, c)| brief(head, f, c, 3))
                    .collect(),
                violations_more: violations.len().saturating_sub(limit),
                new: new
                    .iter()
                    .take(limit)
                    .map(|f| brief(head, f, &Change::New, 2))
                    .collect(),
                new_more: new.len().saturating_sub(limit),
                new_by_kind: new_by_kind.clone(),
                resolved: compared
                    .resolved
                    .iter()
                    .take(limit)
                    .map(|f| Gone {
                        id: f.id.clone(),
                        violation: f.violation,
                        title: clip(&f.title, 160),
                    })
                    .collect(),
                resolved_more: compared.resolved.len().saturating_sub(limit),
                resolved_by_kind: resolved_by_kind.clone(),
                unchanged,
                edges_new: edges_new.iter().take(limit).cloned().collect(),
                edges_new_more: edges_new.len().saturating_sub(limit),
                edges_removed: edges_removed.iter().take(limit).cloned().collect(),
                edges_removed_more: edges_removed.len().saturating_sub(limit),
                edges_changed: edges_changed.iter().take(limit).cloned().collect(),
                edges_changed_more: edges_changed.len().saturating_sub(limit),
                unresolved_new: unresolved_new.iter().take(limit).cloned().collect(),
                unresolved_new_more: unresolved_new.len().saturating_sub(limit),
                unresolved_removed: unresolved_removed.iter().take(limit).cloned().collect(),
                unresolved_removed_more: unresolved_removed.len().saturating_sub(limit),
            }
        },
        render,
    )
}

fn kinds(m: &BTreeMap<String, usize>) -> String {
    m.iter()
        .map(|(k, n)| format!("{k} {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn more(n: usize) -> String {
    if n > 0 {
        format!("  +{n} more\n")
    } else {
        String::new()
    }
}

fn render(r: &DiffResult) -> String {
    let mut s = format!("diff {} → {}\n", r.base, r.head);
    if r.is_empty() {
        s.push_str("no architectural change: same files, dependencies between modules, findings and unresolved imports\n");
        return s;
    }
    // Files.
    let list = |v: &[String], total: usize| {
        let mut x = v.join(", ");
        if total > v.len() {
            x.push_str(&format!(", +{} more", total - v.len()));
        }
        x
    };
    if r.files_added + r.files_removed > 0 {
        s.push_str(&format!("files: +{} −{}", r.files_added, r.files_removed));
        if r.files_added > 0 {
            s.push_str(&format!("; added {}", list(&r.added, r.files_added)));
        }
        if r.files_removed > 0 {
            s.push_str(&format!("; removed {}", list(&r.removed, r.files_removed)));
        }
        s.push('\n');
    }
    // Findings.
    let nv = r.violations.len() + r.violations_more;
    if nv > 0 {
        s.push_str(&format!("rule violations introduced: {nv}\n"));
        for b in &r.violations {
            render_brief(b, &mut s);
        }
        s.push_str(&more(r.violations_more));
    }
    let nn = r.new.len() + r.new_more;
    if nn > 0 {
        s.push_str(&format!("new findings: {nn} ({})\n", kinds(&r.new_by_kind)));
        for b in &r.new {
            render_brief(b, &mut s);
        }
        s.push_str(&more(r.new_more));
    }
    let nr = r.resolved.len() + r.resolved_more;
    if nr > 0 {
        s.push_str(&format!(
            "resolved findings: {nr} ({})\n",
            kinds(&r.resolved_by_kind)
        ));
        for g in &r.resolved {
            let tag = if g.violation { " (violation)" } else { "" };
            s.push_str(&format!("  {}{tag}: {}\n", clip(&g.id, 160), g.title));
        }
        s.push_str(&more(r.resolved_more));
    }
    if r.unchanged > 0 {
        s.push_str(&format!("unchanged findings: {}\n", r.unchanged));
    }
    // Module edges.
    let ne = r.edges_new.len() + r.edges_new_more;
    let nre = r.edges_removed.len() + r.edges_removed_more;
    let nce = r.edges_changed.len() + r.edges_changed_more;
    if ne + nre + nce > 0 {
        let what = if r.modules == "layers" {
            "layers (folders outside them)"
        } else {
            "packages and top folders"
        };
        s.push_str(&format!(
            "dependencies between {what}: {ne} new, {nre} removed, {nce} changed\n"
        ));
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        for e in &r.edges_new {
            s.push_str(&format!(
                "  + {} → {} ({} import{}, e.g. {})\n",
                e.from,
                e.to,
                e.head,
                plural(e.head),
                clip(e.example.as_deref().unwrap_or(""), 160)
            ));
        }
        s.push_str(&more(r.edges_new_more));
        for e in &r.edges_removed {
            s.push_str(&format!(
                "  − {} → {} ({} import{})\n",
                e.from,
                e.to,
                e.base,
                plural(e.base)
            ));
        }
        s.push_str(&more(r.edges_removed_more));
        for e in &r.edges_changed {
            s.push_str(&format!(
                "  ~ {} → {}: {} → {}\n",
                e.from, e.to, e.base, e.head
            ));
        }
        s.push_str(&more(r.edges_changed_more));
    }
    // Unresolved imports.
    let nu = r.unresolved_new.len() + r.unresolved_new_more;
    let nur = r.unresolved_removed.len() + r.unresolved_removed_more;
    if nu + nur > 0 {
        s.push_str(&format!("unresolved imports: {nu} new, {nur} fixed\n"));
        for u in &r.unresolved_new {
            s.push_str(&format!(
                "  + {}:{} {:?} ({})\n",
                u.file, u.line, u.specifier, u.reason
            ));
        }
        s.push_str(&more(r.unresolved_new_more));
        for u in &r.unresolved_removed {
            s.push_str(&format!(
                "  − {}:{} {:?} ({})\n",
                u.file, u.line, u.specifier, u.reason
            ));
        }
        s.push_str(&more(r.unresolved_removed_more));
    }
    s
}
