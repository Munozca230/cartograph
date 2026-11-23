//! Call flows for the web view "Flujos": where flows start, the bounded call tree
//! from one function, one function's callers and callees, and the functions of a file.
//!
//! Everything here reads [`crate::symbols::SymbolTable`] through a [`CallIndex`] built once
//! per indexed snapshot (caller → callees, id → symbol, file → worst finding). Unlike the
//! agent queries, the results are not clipped to a token budget: they are bounded by node
//! and row counts instead ([`FlowOptions`], the `MAX_*` constants), because a screen, not a
//! context window, receives them.

use crate::graph::{FileIx, ROOT};
use crate::kind::FileKind;
use crate::overview::in_test_tree;
use crate::{QueryError, Repo, Severity};
use cartograph_core::{Confidence, EntryKind, EntryPoint, Symbol, SymbolKind, split_symbol_id};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// Default depth of a flow (calls followed from the start).
pub const DEFAULT_DEPTH: u32 = 4;
/// Deepest flow a request may ask for.
pub const MAX_DEPTH: u32 = 8;
/// Default number of functions in a flow.
pub const DEFAULT_NODES: usize = 60;
/// Most functions a flow may hold, expansions included.
pub const MAX_NODES: usize = 300;
/// Callees shown under one function before the rest becomes "+N more".
pub const PER_NODE: usize = 8;
/// Callees shown under a function the user expanded.
pub const EXPAND_PER: usize = 40;
/// Most entries listed.
pub const MAX_ENTRIES: usize = 1000;
/// Most callers listed by [`symbol_card`] (production sites; tests are only counted).
pub const MAX_CALLERS: usize = 30;
/// Most callees listed by [`symbol_card`].
pub const MAX_CALLEES: usize = 40;
/// Most functions listed by [`file_flows`].
pub const MAX_FILE_FUNCTIONS: usize = 40;
/// How far [`file_flows`] walks callers back to find the entries a file's functions serve.
const THROUGH_DEPTH: u32 = 8;
/// How many functions that backward walk may visit.
const THROUGH_BUDGET: usize = 4000;
/// Folders whose subfolders are better lanes than themselves (`src/core`, not `src`).
const SOURCE_ROOTS: &[&str] = &["src", "lib", "app", "source", "pkg", "internal", "packages"];

/// The `via` of a flow edge from an interface method to a bean's method that implements
/// it (Spring runs the bean's).
pub const SPRING_VIA: &str = "Spring bean";

/// Calls of one caller to one target, merged.
#[derive(Debug, Clone)]
struct Callee {
    target: String,
    /// First call line.
    line: u32,
    /// Most confident of the sites.
    confidence: Confidence,
    calls: u32,
    /// Over a bridge rather than a call: `ipc invoke users:get`.
    via: Option<String>,
}

/// Call data indexed for flows. Build it once per snapshot.
#[derive(Debug, Default)]
pub struct CallIndex {
    /// Caller id (`sym:…`, or `file:…` for module-level calls) → its callees, in the order
    /// of their first call.
    out: HashMap<String, Vec<Callee>>,
    /// Symbol id → index in `repo.symbols.symbols` (the first definition when ids repeat).
    sym: HashMap<String, usize>,
    /// Callable symbols by file path, in line order.
    callables: HashMap<String, Vec<usize>>,
    /// File path → its worst problem finding (errors and warnings only).
    mark: HashMap<String, Severity>,
    /// File path → titles of its problem findings.
    titles: HashMap<String, Vec<String>>,
    /// Folders of declared packages other than the root, deepest first.
    packages: Vec<String>,
    /// Callable symbols by name in lower case without `_` (`cmd_index` → `cmdindex`), for
    /// finding the handler of a command or route.
    by_flat: HashMap<String, Vec<usize>>,
    /// Where each entry's flow starts ([`CallIndex::entry_root`]), by index in
    /// `repo.symbols.entries`: worked out once per snapshot.
    roots: Vec<String>,
}

fn rank(c: Confidence) -> u8 {
    match c {
        Confidence::Exact => 0,
        Confidence::Resolved => 1,
        Confidence::Heuristic => 2,
    }
}

impl CallIndex {
    pub fn build(repo: &Repo) -> CallIndex {
        let t = &repo.symbols;
        let mut ix = CallIndex::default();
        for (i, s) in t.symbols.iter().enumerate() {
            ix.sym.entry(s.id()).or_insert(i);
            if s.kind.is_callable() {
                ix.callables.entry(s.file.clone()).or_default().push(i);
                ix.by_flat.entry(flat_name(&s.name)).or_default().push(i);
            }
        }
        let mut calls: Vec<&cartograph_core::SymbolCall> = t.calls.iter().collect();
        calls.sort_by(|a, b| (&a.file, a.line, &a.target).cmp(&(&b.file, b.line, &b.target)));
        let mut pos: HashMap<(String, String), usize> = HashMap::new();
        for c in calls {
            let caller = c
                .caller
                .clone()
                .unwrap_or_else(|| format!("file:{}", c.file));
            let list = ix.out.entry(caller.clone()).or_default();
            match pos.get(&(caller.clone(), c.target.clone())) {
                Some(&k) => {
                    let e = &mut list[k];
                    e.calls += 1;
                    if rank(c.confidence) < rank(e.confidence) {
                        e.confidence = c.confidence;
                    }
                }
                None => {
                    pos.insert((caller, c.target.clone()), list.len());
                    list.push(Callee {
                        target: c.target.clone(),
                        line: c.line,
                        confidence: c.confidence,
                        calls: 1,
                        via: None,
                    });
                }
            }
        }
        // Bridges between two functions (a renderer view → the exposed preload function →
        // the main-process handler) continue a flow, marked as such.
        for b in &t.bridges.bridges {
            let (Some(src), Some(dst)) = (&b.src, &b.dst) else {
                continue;
            };
            if src == dst {
                continue;
            }
            let list = ix.out.entry(src.clone()).or_default();
            if list.iter().any(|c| c.target == *dst) {
                continue;
            }
            list.push(Callee {
                target: dst.clone(),
                line: b.src_line,
                confidence: b.confidence,
                calls: 1,
                via: Some(crate::bridges::step_label(b)),
            });
        }
        // Spring: a call to an interface method goes on to the methods of the beans that
        // implement it (a constructor's or a list's beans alike).
        if !t.spring.beans.is_empty() {
            let mut targets: Vec<String> = ix
                .out
                .values()
                .flatten()
                .filter(|c| c.via.is_none())
                .map(|c| c.target.clone())
                .collect();
            targets.sort();
            targets.dedup();
            for target in targets {
                // A base class's method runs as it is when an override calls it
                // (`super.save()`): only an interface's is always another's.
                let interface = crate::spring::type_of_method(t, &target)
                    .and_then(|ty| t.symbol(&ty))
                    .is_some_and(|s| s.kind == SymbolKind::Interface);
                if !interface {
                    continue;
                }
                let impls = crate::spring::dispatch(t, &target);
                if impls.is_empty() {
                    continue;
                }
                let target_id = target.clone();
                let list = ix.out.entry(target).or_default();
                for (m, _) in impls {
                    if list.iter().any(|c| c.target == m) || m == target_id {
                        continue;
                    }
                    let line = t.symbol(&m).map_or(0, |s| s.line);
                    list.push(Callee {
                        target: m,
                        line,
                        confidence: Confidence::Resolved,
                        calls: 1,
                        via: Some(SPRING_VIA.to_string()),
                    });
                }
            }
        }
        for f in repo.findings() {
            if f.severity == Severity::Info {
                continue;
            }
            for n in &f.nodes {
                let Some(path) = n.strip_prefix("file:") else {
                    continue;
                };
                let m = ix.mark.entry(path.to_string()).or_insert(f.severity);
                if f.severity < *m {
                    *m = f.severity;
                }
                let titles = ix.titles.entry(path.to_string()).or_default();
                if titles.len() < 3 {
                    titles.push(f.title.clone());
                }
            }
        }
        let mut packages: Vec<String> = repo
            .graph
            .package_dirs()
            .filter(|d| !d.is_empty())
            .map(str::to_string)
            .collect();
        packages.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        ix.packages = packages;
        ix.roots = t.entries.iter().map(|e| ix.entry_root(repo, e)).collect();
        ix
    }

    /// Where entry `i` of `repo.symbols.entries` starts its flow.
    fn root_of(&self, i: usize) -> &str {
        self.roots.get(i).map_or("", String::as_str)
    }

    fn symbol<'r>(&self, repo: &'r Repo, id: &str) -> Option<&'r Symbol> {
        self.sym.get(id).map(|&i| &repo.symbols.symbols[i])
    }

    fn callees(&self, id: &str) -> &[Callee] {
        self.out.get(id).map_or(&[], Vec::as_slice)
    }

    /// The worst problem finding on a file (`error` or `warning`).
    pub fn mark(&self, path: &str) -> Option<Severity> {
        self.mark.get(path).copied()
    }

    /// The swimlane of a file: its package (the deepest declared package folder other than
    /// the root), else its top folder, or the first two levels under a source root (`src/ui`).
    /// Files at the root are in the lane `""`.
    pub fn lane_of(&self, path: &str) -> String {
        if let Some(p) = self.packages.iter().find(|p| {
            path.len() > p.len() && path.starts_with(p.as_str()) && path.as_bytes()[p.len()] == b'/'
        }) {
            return p.clone();
        }
        let parts: Vec<&str> = path.split('/').collect();
        match parts.len() {
            0 | 1 => String::new(),
            2 => parts[0].to_string(),
            _ if SOURCE_ROOTS.contains(&parts[0].to_ascii_lowercase().as_str()) => {
                format!("{}/{}", parts[0], parts[1])
            }
            _ => parts[0].to_string(),
        }
    }

    /// Where an entry's flow starts: the function it names; for a command or route that names
    /// a type (a `clap` enum, a controller class), the handler function of the same name in
    /// that file (`cmd_index`, `handleIndex`, `index`…) or anywhere; for a whole file (a
    /// script, a Godot scene) the file, whose module-level calls are its flow.
    fn entry_root(&self, repo: &Repo, e: &EntryPoint) -> String {
        if let Some(t) = e.target.as_deref() {
            if t.starts_with("file:") {
                return t.to_string();
            }
            if self.symbol(repo, t).is_some_and(|s| s.kind.is_callable()) {
                return t.to_string();
            }
        }
        if let Some(h) = self.handler(repo, e.kind, &e.name, &e.file) {
            return h;
        }
        // A command whose handler has no telling name: the program's `main` dispatches it.
        let t = &repo.symbols;
        if let Some(m) = self
            .callables
            .get(&e.file)
            .into_iter()
            .flatten()
            .map(|&i| &t.symbols[i])
            .find(|s| s.name == "main" || s.name == "Main")
        {
            return m.id();
        }
        format!("file:{}", e.file)
    }

    /// The function that handles a command or route called `name`, declared in `file` or,
    /// failing that, anywhere in the repo when only one function fits.
    fn handler(&self, repo: &Repo, kind: EntryKind, name: &str, file: &str) -> Option<String> {
        let (flat, wanted) = handler_names(kind, name)?;
        let t = &repo.symbols;
        let fits = |s: &Symbol| s.kind.is_callable() && wanted.contains(&flat_name(&s.name));
        let local: Vec<&Symbol> = self
            .callables
            .get(file)
            .into_iter()
            .flatten()
            .map(|&i| &t.symbols[i])
            .filter(|s| fits(s))
            .collect();
        // Prefer the prefixed handler (`cmd_index`) over a same-named helper (`index`).
        let best = |v: &[&Symbol]| -> Option<String> {
            v.iter()
                .min_by_key(|s| (flat_name(&s.name) == flat, s.line))
                .map(|s| s.id())
        };
        if !local.is_empty() {
            return best(&local);
        }
        // Elsewhere, outside tests: a prefixed handler, else a function of exactly that name,
        // when only one fits.
        let mut anywhere: Vec<usize> = wanted
            .iter()
            .filter_map(|w| self.by_flat.get(w))
            .flatten()
            .copied()
            .collect();
        anywhere.sort_unstable();
        anywhere.dedup();
        let anywhere: Vec<&Symbol> = anywhere
            .into_iter()
            .map(|i| &t.symbols[i])
            .filter(|s| !is_test_file(repo, &s.file))
            .collect();
        for prefixed in [true, false] {
            let pick: Vec<&Symbol> = anywhere
                .iter()
                .copied()
                .filter(|s| (flat_name(&s.name) != flat) == prefixed)
                .collect();
            let ids: HashSet<String> = pick.iter().map(|s| s.id()).collect();
            if ids.len() == 1 {
                return best(&pick);
            }
            if ids.len() > 1 {
                return None;
            }
        }
        None
    }
}

/// A name in lower case without `_`: `cmd_index`, `cmdIndex` and `CmdIndex` are alike.
fn flat_name(name: &str) -> String {
    name.to_ascii_lowercase().replace('_', "")
}

/// The names a handler of the command or route `name` may have: its last word (`index`, the
/// last literal part of a route's path) bare and with a prefix or suffix (`cmd_index`,
/// `handleIndex`, `index_command`...), all flattened ([`flat_name`]); and the bare one apart.
/// None when nothing in it can name a function: a route's method alone (`GET /`), parameters
/// (`GET /:id`, `/{id}`) or a wildcard (`*`); and for an IPC channel, whose handler the
/// indexer already knows when it is named, and whose name (`doc:save`) is also the name of
/// the renderer-side wrapper that sends to it.
fn handler_names(kind: EntryKind, name: &str) -> Option<(String, Vec<String>)> {
    if kind == EntryKind::Ipc {
        return None;
    }
    let mut name = name.trim();
    if kind == EntryKind::Http {
        // `GET /users`: the method says nothing about the handler's name.
        if let Some((method, rest)) = name.split_once(char::is_whitespace)
            && !method.is_empty()
            && method.chars().all(|c| c.is_ascii_uppercase())
        {
            name = rest.trim();
        }
    }
    let word = name.rsplit([' ', '/']).find(|w| {
        !w.is_empty() && !w.starts_with(':') && !w.starts_with('{') && !w.starts_with('*')
    })?;
    let flat: String = word
        .chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if flat.is_empty() {
        return None;
    }
    let mut wanted = vec![flat.clone()];
    for p in ["cmd", "run", "handle", "do", "command"] {
        wanted.push(format!("{p}{flat}"));
        wanted.push(format!("{flat}{p}"));
    }
    Some((flat, wanted))
}

fn is_test_file(repo: &Repo, path: &str) -> bool {
    let g = &repo.graph;
    match g.file_by_path(path) {
        Some(f) => test_ix(repo, f),
        None => crate::kind::is_test(path),
    }
}

fn test_ix(repo: &Repo, f: FileIx) -> bool {
    let g = &repo.graph;
    g.files[f as usize].kind == FileKind::Test || in_test_tree(g, f, ROOT)
}

/// The short name shown on a card: `Container.name` for a method, else the name.
fn name_of(id: &str, s: Option<&Symbol>) -> String {
    match s {
        Some(s) => s.qualified(),
        None => match split_symbol_id(id) {
            Some((_, n)) => n.to_string(),
            None => id
                .strip_prefix("file:")
                .map_or(id, |p| p.rsplit('/').next().unwrap_or(p))
                .to_string(),
        },
    }
}

fn file_of(id: &str, s: Option<&Symbol>) -> String {
    match s {
        Some(s) => s.file.clone(),
        None => match split_symbol_id(id) {
            Some((p, _)) => p.to_string(),
            None => id.strip_prefix("file:").unwrap_or(id).to_string(),
        },
    }
}

// ------------------------------------------------------------------------------ entries

#[derive(Debug, Clone, Serialize)]
pub struct FlowEntry {
    pub kind: EntryKind,
    pub name: String,
    pub file: String,
    pub line: u32,
    /// What the index says runs (`sym:…`, `file:…`), when known.
    pub target: Option<String>,
    /// Where its flow starts: a function, or `file:<path>` (see `CallIndex::entry_root`).
    pub root: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntryList {
    /// Whether the index has a symbol layer (an index written before it has none).
    pub indexed: bool,
    pub total: usize,
    pub entries: Vec<FlowEntry>,
    /// Entries inside test, fixture and example trees, left out.
    pub hidden: usize,
    pub truncated: bool,
}

fn kind_rank(k: EntryKind) -> u8 {
    match k {
        EntryKind::Cli => 0,
        EntryKind::Http => 1,
        EntryKind::Main => 2,
        EntryKind::Ipc => 3,
        EntryKind::GodotMainScene => 4,
        EntryKind::GodotAutoload => 5,
    }
}

fn flow_entry(ix: &CallIndex, i: usize, e: &EntryPoint) -> FlowEntry {
    FlowEntry {
        kind: e.kind,
        name: e.name.clone(),
        file: e.file.clone(),
        line: e.line,
        target: e.target.clone(),
        root: ix.root_of(i).to_string(),
    }
}

/// The entries outside test, fixture and example trees, with their index.
fn repo_entries(repo: &Repo) -> impl Iterator<Item = (usize, &EntryPoint)> {
    repo.symbols
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| !is_test_file(repo, &e.file))
}

/// Every flow entry outside test, fixture and example trees, by kind (commands, routes,
/// `main`, Godot), then file and line.
pub fn entries(repo: &Repo, ix: &CallIndex) -> EntryList {
    let t = &repo.symbols;
    let mut list: Vec<(usize, &EntryPoint)> = repo_entries(repo).collect();
    let hidden = t.entries.len() - list.len();
    list.sort_by(|(_, a), (_, b)| {
        (kind_rank(a.kind), &a.file, a.line, &a.name).cmp(&(
            kind_rank(b.kind),
            &b.file,
            b.line,
            &b.name,
        ))
    });
    EntryList {
        indexed: t.indexed,
        total: list.len(),
        truncated: list.len() > MAX_ENTRIES,
        entries: list
            .iter()
            .take(MAX_ENTRIES)
            .map(|&(i, e)| flow_entry(ix, i, e))
            .collect(),
        hidden,
    }
}

// --------------------------------------------------------------------------------- flow

#[derive(Debug, Clone)]
pub struct FlowOptions {
    /// Calls followed from the start (1..=[`MAX_DEPTH`]).
    pub depth: u32,
    /// Functions shown (10..=[`MAX_NODES`]).
    pub max_nodes: usize,
    /// Functions whose callees are all shown (up to [`EXPAND_PER`]), past the depth and the
    /// node budget (up to [`MAX_NODES`]), in the order they were opened. They add to the base
    /// flow (the one without them) and never take its place.
    pub expand: Vec<String>,
}

impl Default for FlowOptions {
    fn default() -> FlowOptions {
        FlowOptions {
            depth: DEFAULT_DEPTH,
            max_nodes: DEFAULT_NODES,
            expand: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FlowNode {
    /// `sym:<path>#<name>`, or `file:<path>` for a file's module-level code.
    pub id: String,
    pub name: String,
    /// The symbol kind (`function`, `method`…), or `file`.
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub end_line: u32,
    /// Call sites that reach it, in the whole repo.
    pub callers: usize,
    /// Calls from the start.
    pub depth: u32,
    /// Distinct functions it calls.
    pub callees: usize,
    /// Of those, how many this flow leaves out ("+N more").
    pub more: usize,
    /// Opened with `expand`. A `more` left on an open function is what the flow's limits
    /// ([`EXPAND_PER`], [`MAX_NODES`]) still leave out: opening it again shows nothing new.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub open: bool,
    /// The worst problem finding on its file: `error` or `warning`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finding: Option<Severity>,
    /// Swimlane ([`CallIndex::lane_of`]).
    pub lane: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FlowEdge {
    pub src: String,
    pub dst: String,
    pub confidence: Confidence,
    /// First call line, in `src`'s file.
    pub line: u32,
    /// Call sites merged into it.
    pub calls: u32,
    /// A bridge, not a call (`ipc invoke users:get`): drawn dashed, with this label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FlowLane {
    pub id: String,
    /// The folder's name (`cartograph-cli`, `src/ui`), or `""` for the repo root.
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FlowGraph {
    pub root: String,
    pub depth: u32,
    pub max_nodes: usize,
    pub nodes: Vec<FlowNode>,
    pub edges: Vec<FlowEdge>,
    /// Lanes in order of first appearance (the start's lane first).
    pub lanes: Vec<FlowLane>,
    /// Some callees were left out (depth, per-function or node budget).
    pub truncated: bool,
}

/// The call flow from `from` (a symbol id, `path#name`, a bare name, or `file:<path>` for a
/// file's module-level calls), breadth first: each function shows up to [`PER_NODE`] callees
/// in the order it first calls them, down to `depth` calls and `max_nodes` functions; what is
/// left out is counted per function in `more`. Within a level, the functions that call the
/// most are followed first, so the node budget goes to them before small helpers. The
/// functions in `opts.expand` then add their callees on top of that base flow, so opening one
/// never removes another. A function called from several places is one node with several
/// edges, so the result is a DAG (a cycle when the code recurses).
pub fn flow(
    repo: &Repo,
    ix: &CallIndex,
    from: &str,
    opts: &FlowOptions,
) -> Result<FlowGraph, QueryError> {
    let t = &repo.symbols;
    if !t.indexed {
        return Err(QueryError::BadArgument(
            "this index has no symbols; re-run `cartograph index`".into(),
        ));
    }
    let root = if let Some(path) = from.strip_prefix("file:") {
        if repo.graph.file_by_path(path).is_none() {
            return Err(QueryError::UnknownNode {
                input: from.to_string(),
                suggestions: Vec::new(),
            });
        }
        from.to_string()
    } else {
        t.resolve(from)?
    };
    let depth = opts.depth.clamp(1, MAX_DEPTH);
    let budget = opts.max_nodes.clamp(10, MAX_NODES);

    let mut nodes: Vec<FlowNode> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    let make = |id: &str, d: u32| -> FlowNode {
        let s = ix.symbol(repo, id);
        let file = file_of(id, s);
        FlowNode {
            id: id.to_string(),
            name: name_of(id, s),
            kind: s.map_or("file", |s| s.kind.as_str()).to_string(),
            line: s.map_or(1, |s| s.line),
            end_line: s.map_or(1, |s| s.end_line),
            callers: t.callers_of(id).len(),
            depth: d,
            callees: ix.callees(id).iter().filter(|c| c.target != id).count(),
            more: 0,
            open: false,
            finding: ix.mark(&file),
            lane: ix.lane_of(&file),
            file,
        }
    };
    // Appends the callees of `n` that are not in the flow yet, at most `per` of them and
    // while the flow holds fewer than `cap` functions. Returns the new nodes.
    let grow = |nodes: &mut Vec<FlowNode>,
                at: &mut HashMap<String, usize>,
                n: usize,
                per: usize,
                cap: usize|
     -> Vec<usize> {
        let id = nodes[n].id.clone();
        let d = nodes[n].depth;
        let mut added = Vec::new();
        for c in ix.callees(&id) {
            if c.target == id || at.contains_key(&c.target) {
                continue;
            }
            if added.len() >= per || nodes.len() >= cap {
                break;
            }
            at.insert(c.target.clone(), nodes.len());
            added.push(nodes.len());
            nodes.push(make(&c.target, d + 1));
        }
        added
    };
    nodes.push(make(&root, 0));
    at.insert(root.clone(), 0);

    // 1. The base flow, the same whatever is expanded: level by level, and within a level
    // the functions that call the most first, so the budget goes to the ones that do the
    // work before small helpers (`new`, `default`).
    let mut level: Vec<usize> = vec![0];
    while !level.is_empty() {
        level.sort_by(|&a, &b| nodes[b].callees.cmp(&nodes[a].callees).then(a.cmp(&b)));
        let mut next = Vec::new();
        for &n in &level {
            if nodes[n].depth < depth {
                next.extend(grow(&mut nodes, &mut at, n, PER_NODE, budget));
            }
        }
        level = next;
    }

    // 2. Expansions on top of it, in the order they were opened: each shows its callees as
    // cards of their own (with their cut counted), past the depth and the budget, up to
    // MAX_NODES. Nothing already shown goes away. An expansion of a card that only another
    // expansion shows waits for it.
    let mut done: HashSet<&str> = HashSet::new();
    loop {
        let mut progress = false;
        for x in &opts.expand {
            let Some(&n) = at.get(x.as_str()) else {
                continue;
            };
            if !done.insert(x.as_str()) {
                continue;
            }
            progress = true;
            nodes[n].open = true;
            grow(&mut nodes, &mut at, n, EXPAND_PER, MAX_NODES);
        }
        if !progress {
            break;
        }
    }

    // 3. Every call between two cards of the flow is an edge (a function called from several
    // places is one card with several edges), and what a card calls outside it is its "+N".
    let mut edges: Vec<FlowEdge> = Vec::new();
    let mut truncated = false;
    for node in &mut nodes {
        let id = node.id.as_str();
        let mut more = 0;
        for c in ix.callees(id) {
            if c.target == id {
                continue;
            }
            if at.contains_key(&c.target) {
                edges.push(FlowEdge {
                    src: id.to_string(),
                    dst: c.target.clone(),
                    confidence: c.confidence,
                    line: c.line,
                    calls: c.calls,
                    via: c.via.clone(),
                });
            } else {
                more += 1;
            }
        }
        node.more = more;
        truncated |= more > 0;
    }

    refine_lanes(&mut nodes);
    let mut lanes: Vec<FlowLane> = Vec::new();
    for n in &nodes {
        if !lanes.iter().any(|l| l.id == n.lane) {
            lanes.push(FlowLane {
                id: n.lane.clone(),
                label: n.lane.clone(),
            });
        }
    }
    Ok(FlowGraph {
        root,
        depth,
        max_nodes: budget,
        nodes,
        edges,
        lanes,
        truncated,
    })
}

/// A flow that stays in one lane says nothing with it: split it by the next folder level
/// (`src/server` → `src/server/api`, `src/server/auth`), as long as that gives
/// at least two lanes. Files right in the lane's folder keep it.
fn refine_lanes(nodes: &mut [FlowNode]) {
    for _ in 0..3 {
        let first = match nodes.first() {
            Some(n) => n.lane.clone(),
            None => return,
        };
        if nodes.iter().any(|n| n.lane != first) {
            return;
        }
        let next = |n: &FlowNode| -> Option<String> {
            let rest = if first.is_empty() {
                n.file.as_str()
            } else {
                n.file.strip_prefix(first.as_str())?.strip_prefix('/')?
            };
            let (seg, _) = rest.split_once('/')?;
            Some(if first.is_empty() {
                seg.to_string()
            } else {
                format!("{first}/{seg}")
            })
        };
        let split: Vec<Option<String>> = nodes.iter().map(next).collect();
        let distinct: HashSet<&String> = split.iter().flatten().collect();
        if distinct.len() + usize::from(split.iter().any(Option::is_none)) < 2 {
            // One subfolder holds everything: go one level deeper.
            match distinct.into_iter().next() {
                Some(only) if split.iter().all(Option::is_some) => {
                    let only = only.clone();
                    for n in nodes.iter_mut() {
                        n.lane = only.clone();
                    }
                    continue;
                }
                _ => return,
            }
        }
        for (n, s) in nodes.iter_mut().zip(split) {
            if let Some(s) = s {
                n.lane = s;
            }
        }
        return;
    }
}

// ------------------------------------------------------------------------- symbol card

#[derive(Debug, Clone, Serialize)]
pub struct SymbolInfo {
    /// `sym:…`, or `file:<path>` for a file's module-level code.
    pub id: String,
    /// `Container.name`, or the file's name.
    pub name: String,
    /// The symbol kind (`function`, `method`…), or `file`.
    pub kind: String,
    pub file: String,
    pub line: u32,
    /// The symbol's last line. For a `file` card, the file's non-blank lines (its
    /// [`cartograph_core::Node::loc`]; the graph keeps no count of all its lines).
    pub end_line: u32,
    pub exported: bool,
    pub lane: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallerSite {
    pub file: String,
    pub line: u32,
    /// The calling function (`None` at module level).
    pub caller: Option<String>,
    /// Its short name.
    pub caller_name: Option<String>,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallerSummary {
    /// Call sites outside tests, production code first in the listing.
    pub production: Vec<CallerSite>,
    pub production_total: usize,
    pub production_files: usize,
    /// Call sites in tests, fixtures and examples: only counted.
    pub test_sites: usize,
    pub test_files: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CalleeRow {
    pub id: String,
    pub name: String,
    pub file: String,
    pub line: u32,
    pub confidence: Confidence,
    pub calls: u32,
    pub lane: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthorRow {
    pub name: String,
    pub commits: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitSummary {
    /// Non-merge commits that touched the file.
    pub commits: u32,
    /// Of those, in the recent window.
    pub recent: u32,
    pub window_days: Option<u32>,
    /// Unix seconds of its last commit.
    pub last_time: i64,
    pub authors: Vec<AuthorRow>,
    pub authors_total: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolCard {
    pub symbol: SymbolInfo,
    pub callers: CallerSummary,
    pub callees: Vec<CalleeRow>,
    pub callees_total: usize,
    /// Distinct lanes its callees are in.
    pub callee_lanes: usize,
    /// The file's history, when the git layer ran.
    pub git: Option<GitSummary>,
    pub finding: Option<Severity>,
    /// Titles of the problem findings on its file (at most 3).
    pub findings: Vec<String>,
    /// Entries whose flow starts here.
    pub entries: Vec<FlowEntry>,
}

/// One function: what it is, who calls it (production sites first, tests counted), what it
/// calls, and its file's history. `file:<path>` gives the same card for a file's module-level
/// code (a script, a Godot scene, an entry with no handler): its calls, and who runs it.
pub fn symbol_card(repo: &Repo, ix: &CallIndex, id: &str) -> Result<SymbolCard, QueryError> {
    let t = &repo.symbols;
    if !t.indexed {
        return Err(QueryError::BadArgument(
            "this index has no symbols; re-run `cartograph index`".into(),
        ));
    }
    let info = if let Some(path) = id.strip_prefix("file:") {
        let Some(f) = repo.graph.file_by_path(path) else {
            return Err(QueryError::UnknownNode {
                input: id.to_string(),
                suggestions: Vec::new(),
            });
        };
        let node = &repo.graph.files[f as usize];
        SymbolInfo {
            id: id.to_string(),
            name: name_of(id, None),
            kind: "file".into(),
            file: path.to_string(),
            line: 1,
            end_line: u32::try_from(node.loc).unwrap_or(u32::MAX).max(1),
            exported: false,
            lane: ix.lane_of(path),
        }
    } else {
        let id = t.resolve(id)?;
        let Some(s) = ix.symbol(repo, &id) else {
            return Err(QueryError::UnknownNode {
                input: id,
                suggestions: Vec::new(),
            });
        };
        SymbolInfo {
            name: s.qualified(),
            kind: s.kind.as_str().to_string(),
            file: s.file.clone(),
            line: s.line,
            end_line: s.end_line,
            exported: s.exported,
            lane: ix.lane_of(&s.file),
            id,
        }
    };
    let id = info.id.clone();
    let file = info.file.clone();
    let mut prod: Vec<CallerSite> = Vec::new();
    let mut prod_files: HashSet<&str> = HashSet::new();
    let (mut test_sites, mut test_files) = (0usize, HashSet::new());
    for &i in t.callers_of(&id) {
        let c = &t.calls[i];
        if is_test_file(repo, &c.file) {
            test_sites += 1;
            test_files.insert(c.file.as_str());
            continue;
        }
        prod_files.insert(c.file.as_str());
        prod.push(CallerSite {
            file: c.file.clone(),
            line: c.line,
            caller_name: c.caller.as_deref().map(|x| name_of(x, ix.symbol(repo, x))),
            caller: c.caller.clone(),
            confidence: c.confidence,
        });
    }
    // Most confident first, then by place.
    prod.sort_by(|a, b| {
        (rank(a.confidence), &a.file, a.line).cmp(&(rank(b.confidence), &b.file, b.line))
    });
    let production_total = prod.len();
    prod.truncate(MAX_CALLERS);

    let all: Vec<&Callee> = ix.callees(&id).iter().filter(|c| c.target != id).collect();
    let callee_lanes: HashSet<String> = all
        .iter()
        .map(|c| ix.lane_of(&file_of(&c.target, ix.symbol(repo, &c.target))))
        .collect();
    let callees: Vec<CalleeRow> = all
        .iter()
        .take(MAX_CALLEES)
        .map(|c| {
            let cs = ix.symbol(repo, &c.target);
            let file = file_of(&c.target, cs);
            CalleeRow {
                id: c.target.clone(),
                name: name_of(&c.target, cs),
                line: cs.map_or(1, |x| x.line),
                confidence: c.confidence,
                calls: c.calls,
                lane: ix.lane_of(&file),
                file,
            }
        })
        .collect();

    let git = repo.graph.file_by_path(&file).and_then(|f| {
        let h = repo.git.files.get(f as usize)?.as_ref()?;
        Some(GitSummary {
            commits: h.commits,
            recent: h.recent,
            window_days: repo.git.status.as_ref().map(|st| st.window_days),
            last_time: h.last_time,
            authors: h
                .authors
                .iter()
                .take(3)
                .map(|a| AuthorRow {
                    name: a.name.clone(),
                    commits: a.commits,
                })
                .collect(),
            authors_total: h.authors.len(),
        })
    });

    let entries = repo_entries(repo)
        .filter(|&(i, _)| ix.root_of(i) == id)
        .take(12)
        .map(|(i, e)| flow_entry(ix, i, e))
        .collect();

    Ok(SymbolCard {
        symbol: info,
        callers: CallerSummary {
            production: prod,
            production_total,
            production_files: prod_files.len(),
            test_sites,
            test_files: test_files.len(),
        },
        callees_total: all.len(),
        callee_lanes: callee_lanes.len(),
        callees,
        git,
        finding: ix.mark(&file),
        findings: ix.titles.get(&file).cloned().unwrap_or_default(),
        entries,
    })
}

// --------------------------------------------------------------------------- file flows

#[derive(Debug, Clone, Serialize)]
pub struct FileFunction {
    pub id: String,
    pub name: String,
    pub kind: SymbolKind,
    pub line: u32,
    /// Call sites that reach it.
    pub callers: usize,
    /// Distinct functions it calls.
    pub callees: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileFlows {
    pub file: String,
    pub indexed: bool,
    /// Its functions and methods, most called first.
    pub functions: Vec<FileFunction>,
    pub functions_total: usize,
    /// Entries declared in the file, and entries whose flow reaches one of its functions
    /// (found by walking callers back from them, [`THROUGH_DEPTH`] calls at most).
    pub entries: Vec<FlowEntry>,
    /// The backward walk stopped at its budget: some entries may be missing.
    pub partial: bool,
}

/// What "Ver flujos" lists for a file of the map: the flows that pass through it and its
/// functions.
pub fn file_flows(repo: &Repo, ix: &CallIndex, file: &str) -> Result<FileFlows, QueryError> {
    let t = &repo.symbols;
    let path = file.strip_prefix("file:").unwrap_or(file);
    if repo.graph.file_by_path(path).is_none() {
        return Err(QueryError::UnknownNode {
            input: file.to_string(),
            suggestions: Vec::new(),
        });
    }
    let own: Vec<&Symbol> = ix
        .callables
        .get(path)
        .into_iter()
        .flatten()
        .map(|&i| &t.symbols[i])
        .collect();
    let mut functions: Vec<FileFunction> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for s in &own {
        let id = s.id();
        if !seen.insert(id.clone()) {
            continue;
        }
        functions.push(FileFunction {
            callers: t.callers_of(&id).len(),
            callees: ix.callees(&id).iter().filter(|c| c.target != id).count(),
            name: s.qualified(),
            kind: s.kind,
            line: s.line,
            id,
        });
    }
    functions.sort_by(|a, b| b.callers.cmp(&a.callers).then(a.line.cmp(&b.line)));
    let functions_total = functions.len();
    functions.truncate(MAX_FILE_FUNCTIONS);

    // Walk callers back from the file's functions (and its module-level code).
    let mut reached: HashSet<String> = seen.clone();
    reached.insert(format!("file:{path}"));
    let mut frontier: Vec<String> = reached.iter().cloned().collect();
    let mut partial = false;
    'walk: for _ in 0..THROUGH_DEPTH {
        let mut next = Vec::new();
        for id in &frontier {
            for &i in t.callers_of(id) {
                let c = &t.calls[i];
                if is_test_file(repo, &c.file) {
                    continue;
                }
                let up = c
                    .caller
                    .clone()
                    .unwrap_or_else(|| format!("file:{}", c.file));
                if reached.insert(up.clone()) {
                    if reached.len() > THROUGH_BUDGET {
                        partial = true;
                        break 'walk;
                    }
                    next.push(up);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    let mut entries: Vec<FlowEntry> = repo_entries(repo)
        .filter(|&(i, e)| e.file == path || reached.contains(ix.root_of(i)))
        .map(|(i, e)| flow_entry(ix, i, e))
        .collect();
    entries.sort_by(|a, b| {
        (a.file != path, kind_rank(a.kind), &a.file, a.line).cmp(&(
            b.file != path,
            kind_rank(b.kind),
            &b.file,
            b.line,
        ))
    });
    entries.truncate(40);
    Ok(FileFlows {
        file: format!("file:{path}"),
        indexed: t.indexed,
        functions,
        functions_total,
        entries,
        partial,
    })
}

// ------------------------------------------------------------------------------- search

/// A function found by [`search_functions`].
#[derive(Debug, Clone, Serialize)]
pub struct FunctionHit {
    pub id: String,
    pub name: String,
    pub kind: SymbolKind,
    pub file: String,
    pub line: u32,
    pub callers: usize,
}

/// Functions and methods whose name contains `q` (case-insensitive; `Class.method` and
/// `path#name` work too): exact names first, then prefixes, then the most called.
pub fn search_functions(repo: &Repo, q: &str, limit: usize) -> Vec<FunctionHit> {
    let q = q.trim().to_lowercase();
    if q.is_empty() || !repo.symbols.indexed {
        return Vec::new();
    }
    let (want_path, want_name) = match q.rsplit_once('#') {
        Some((p, n)) => (Some(p.to_string()), n.to_string()),
        None => (None, q.clone()),
    };
    let t = &repo.symbols;
    let mut seen: HashSet<String> = HashSet::new();
    let mut hits: Vec<(u8, usize, FunctionHit)> = Vec::new();
    for s in &t.symbols {
        if !s.kind.is_callable() {
            continue;
        }
        let qual = s.qualified().to_lowercase();
        let name = s.name.to_lowercase();
        if want_path
            .as_deref()
            .is_some_and(|p| !s.file.to_lowercase().contains(p))
        {
            continue;
        }
        let tier = if name == want_name || qual == want_name {
            0
        } else if name.starts_with(&want_name) || qual.starts_with(&want_name) {
            1
        } else if qual.contains(&want_name) {
            2
        } else {
            continue;
        };
        let id = s.id();
        if !seen.insert(id.clone()) {
            continue;
        }
        let callers = t.callers_of(&id).len();
        let test = is_test_file(repo, &s.file);
        hits.push((
            tier + if test { 3 } else { 0 },
            callers,
            FunctionHit {
                name: s.qualified(),
                kind: s.kind,
                file: s.file.clone(),
                line: s.line,
                callers,
                id,
            },
        ));
    }
    hits.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(b.1.cmp(&a.1))
            .then(a.2.file.len().cmp(&b.2.file.len()))
            .then(a.2.id.cmp(&b.2.id))
    });
    hits.into_iter().take(limit).map(|h| h.2).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(file: &str, lane: &str) -> FlowNode {
        FlowNode {
            id: format!("sym:{file}#f"),
            name: "f".into(),
            kind: "function".into(),
            file: file.into(),
            line: 1,
            end_line: 1,
            callers: 0,
            depth: 0,
            callees: 0,
            more: 0,
            open: false,
            finding: None,
            lane: lane.into(),
        }
    }

    fn lanes(v: &[FlowNode]) -> Vec<&str> {
        v.iter().map(|x| x.lane.as_str()).collect()
    }

    #[test]
    fn a_single_lane_splits_by_the_next_folder() {
        let mut v = vec![
            n("src/server/main.ts", "src/server"),
            n("src/server/api/a.ts", "src/server"),
            n("src/server/auth/b.ts", "src/server"),
        ];
        refine_lanes(&mut v);
        assert_eq!(
            lanes(&v),
            vec!["src/server", "src/server/api", "src/server/auth"]
        );
    }

    #[test]
    fn one_subfolder_for_everything_goes_one_level_deeper() {
        let mut v = vec![
            n("app/x/a/1.ts", "app/x"),
            n("app/x/a/b/2.ts", "app/x"),
            n("app/x/a/c/3.ts", "app/x"),
        ];
        refine_lanes(&mut v);
        assert_eq!(lanes(&v), vec!["app/x/a", "app/x/a/b", "app/x/a/c"]);
    }

    #[test]
    fn a_route_handler_is_named_by_its_path_never_by_its_method() {
        let flat = |kind, name| handler_names(kind, name).map(|(f, _)| f);
        assert_eq!(flat(EntryKind::Http, "GET /api/tree"), Some("tree".into()));
        assert_eq!(
            flat(EntryKind::Http, "POST /users/:id"),
            Some("users".into())
        );
        assert_eq!(flat(EntryKind::Http, "GET /files/*"), Some("files".into()));
        // Nothing in these names a function: no `get`, and not every `run` or `handle`.
        assert_eq!(flat(EntryKind::Http, "GET /"), None);
        assert_eq!(flat(EntryKind::Http, "GET /:id"), None);
        assert_eq!(flat(EntryKind::Http, "GET /{id}"), None);
        assert_eq!(flat(EntryKind::Http, "GET *"), None);
        assert_eq!(flat(EntryKind::Http, "ANY /*rest"), None);
        let (f, wanted) = handler_names(EntryKind::Cli, "index").unwrap();
        assert_eq!(f, "index");
        assert!(
            wanted.contains(&"cmdindex".to_string()) && wanted.contains(&"handleindex".to_string())
        );
        assert!(!wanted.contains(&"cmd".to_string()));
    }

    #[test]
    fn lanes_already_apart_or_flat_stay() {
        let mut v = vec![n("a/1.rs", "a"), n("b/2.rs", "b")];
        refine_lanes(&mut v);
        assert_eq!(lanes(&v), vec!["a", "b"]);
        let mut flat = vec![n("src/1.rs", "src"), n("src/2.rs", "src")];
        refine_lanes(&mut flat);
        assert_eq!(lanes(&flat), vec!["src", "src"]);
    }
}
