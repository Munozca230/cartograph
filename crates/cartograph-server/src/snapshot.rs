//! Immutable, query-ready view of one indexed graph.
//!
//! The canonical hierarchy is the directory tree, derived from paths: every File node sits
//! in the dir of its parent path and every dir in its parent dir, up to the root (`repo:`).
//! The store's `Contains` edges are not used, so their quirks (top-level files under the
//! Repo node, Package → Dir containment, `DependsOn` only between immediate containers)
//! never reach the API.

use cartograph_core::{Confidence, EdgeKind, Graph, History, NodeKind, Symbols};
use cartograph_index::aggregate::PackageOwners;
use cartograph_query::flow::CallIndex;
use cartograph_query::{Config, ConfigError, Finding, Repo, Severity};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Index of a dir in [`Snapshot::dirs`]; 0 is the repo root.
pub type DirIx = u32;
/// Index of a file in [`Snapshot::files`].
pub type FileIx = u32;
/// Index of an external package in [`Snapshot::externals`].
pub type ExtIx = u32;

/// The repo root's index in [`Snapshot::dirs`].
pub const ROOT: DirIx = 0;

#[derive(Debug, Clone)]
pub struct Dir {
    /// Repo-relative path (`""` for the root).
    pub path: String,
    pub name: String,
    pub parent: Option<DirIx>,
    /// Child dirs, sorted by name.
    pub dirs: Vec<DirIx>,
    /// Files directly inside, sorted by name.
    pub files: Vec<FileIx>,
    /// Descendant files.
    pub file_count: u64,
    pub loc: u64,
    /// Descendant files by language (`other` when unknown).
    pub langs: BTreeMap<String, u64>,
    /// Distinct files outside that import something inside.
    pub fan_in: u64,
    /// Distinct files outside imported from inside.
    pub fan_out: u64,
    /// Unresolved imports of descendant files.
    pub unresolved: u64,
}

#[derive(Debug, Clone)]
pub struct File {
    pub path: String,
    pub name: String,
    pub dir: DirIx,
    pub lang: Option<String>,
    pub loc: u64,
    /// Distinct files importing it.
    pub fan_in: u64,
    /// Distinct files it imports.
    pub fan_out: u64,
    pub unresolved: u64,
    /// API id (`pkg:…`) of the owning package.
    pub package: Option<String>,
}

#[derive(Debug, Clone)]
pub struct External {
    /// API id: `ext:npm:react`.
    pub id: String,
    /// `npm:react`.
    pub name: String,
}

/// A distinct file → file import (one per file pair, whatever the number of sites).
#[derive(Debug, Clone, Copy)]
pub struct FileEdge {
    pub src: FileIx,
    pub dst: FileIx,
    pub confidence: Confidence,
}

/// A file → file bridge (see [`Snapshot::bridge_edges`]).
#[derive(Debug, Clone, Copy)]
pub struct BridgeEdge {
    pub src: FileIx,
    pub dst: FileIx,
    /// Bridged sites.
    pub weight: u64,
    pub confidence: Confidence,
}

/// A file → Python namespace package (dir) import.
#[derive(Debug, Clone, Copy)]
pub struct NsEdge {
    pub src: FileIx,
    pub dst: DirIx,
    pub confidence: Confidence,
}

/// A distinct file → external package import.
#[derive(Debug, Clone, Copy)]
pub struct ExtEdge {
    pub src: FileIx,
    pub ext: ExtIx,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Counts {
    pub files: u64,
    pub dirs: u64,
    pub packages: u64,
    pub loc: u64,
    pub imports: u64,
    pub resolved: u64,
    pub external: u64,
    pub unresolved: u64,
    pub skipped: u64,
    pub diagnostics: u64,
    /// Findings that are problems (severity error or warning).
    pub findings: u64,
    /// Informational findings (bottlenecks, Stable Dependencies, deep chains…).
    pub findings_info: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LangCount {
    pub lang: String,
    pub files: u64,
    pub loc: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub dir: String,
    pub manifest: String,
    pub notes: Option<String>,
}

struct SearchEntry {
    id: String,
    kind: &'static str,
    path: String,
    name: String,
    lower_path: String,
    lower_name: String,
}

/// One indexed graph, ready to answer every API query without touching the store.
pub struct Snapshot {
    pub generation: u64,
    pub repo_name: String,
    pub indexed_at: Option<String>,
    pub index_ms: Option<u64>,
    pub counts: Counts,
    pub languages: Vec<LangCount>,
    pub dirs: Vec<Dir>,
    pub files: Vec<File>,
    pub externals: Vec<External>,
    pub file_edges: Vec<FileEdge>,
    /// Bridges between files (edges of kind `bridge`: an IPC channel, an exposed preload
    /// function, an HTTP route): never counted with the imports.
    pub bridge_edges: Vec<BridgeEdge>,
    pub ns_edges: Vec<NsEdge>,
    pub ext_edges: Vec<ExtEdge>,
    pub packages: Vec<Package>,
    /// The raw graph (import sites, unresolved records, skipped files, diagnostics).
    pub graph: Graph,
    dir_ix: HashMap<String, DirIx>,
    file_ix: HashMap<String, FileIx>,
    ext_ix: HashMap<String, ExtIx>,
    /// Import sites (indexes into `graph.imports`) by importing file path.
    sites_by_file: HashMap<String, Vec<usize>>,
    /// Import sites by target store id.
    sites_by_target: HashMap<String, Vec<usize>>,
    /// Unresolved records (indexes into `graph.unresolved`) by file path.
    unresolved_by_file: HashMap<String, Vec<usize>>,
    /// Edge confidence by (src store id, dst store id).
    confidence: HashMap<(String, String), Confidence>,
    search: Vec<SearchEntry>,
    /// Findings of this graph under the repo's rules (`cartograph-query`), sorted.
    pub findings: Vec<Finding>,
    /// Where the rules came from (`cartograph.toml`), if a file was read.
    pub config_origin: Option<String>,
    /// Why `cartograph.toml` could not be used (the findings then use the defaults).
    pub config_error: Option<String>,
    /// The query layer behind the flow endpoints (`/api/entries`, `/api/flow`,
    /// `/api/symbol`, `/api/file-flows`): the graph with its symbols and git history.
    pub flows: Option<FlowLayer>,
    /// `/api/meta`'s `workspace`, for a workspace index: its members (see [`workspace_meta`]).
    pub workspace: Option<Value>,
    /// Member names of a workspace index, to read the alias `member:path` in ids.
    members: Vec<String>,
}

/// The members of a workspace index, from its run metadata (`None` for a single repo):
/// `{ members: [{ name, path, root, git, branch, head, detached }] }`, `path` as the
/// workspace file writes it, `git` the state of the member's last git pass (`null` before
/// the first one).
pub fn workspace_meta(meta: &BTreeMap<String, String>) -> Option<Value> {
    let members: Vec<cartograph_core::Member> = meta
        .get(cartograph_index::META_WORKSPACE)
        .and_then(|t| serde_json::from_str(t).ok())?;
    let rows: Vec<Value> = members
        .iter()
        .map(|m| {
            let git: Option<cartograph_core::GitStatus> = meta
                .get(&cartograph_core::git_meta_key(&m.name))
                .and_then(|t| serde_json::from_str(t).ok());
            json!({
                "name": m.name,
                "path": m.path,
                "root": m.root.to_string_lossy().replace('\\', "/"),
                "git": git.as_ref().map(|g| g.state),
                "branch": git.as_ref().and_then(|g| g.branch.clone()),
                "head": git.as_ref().and_then(|g| g.head.clone()),
                "detached": git.as_ref().is_some_and(|g| g.detached),
            })
        })
        .collect();
    Some(json!({ "members": rows }))
}

/// The repo as `cartograph-query` sees it, with its calls indexed for flows.
pub struct FlowLayer {
    pub repo: Repo,
    pub calls: CallIndex,
}

fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn last_segment(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// API id of a store id: `external:` → `ext:`, `package:` → `pkg:`; others unchanged.
pub fn api_id(store_id: &str) -> String {
    if let Some(rest) = store_id.strip_prefix("external:") {
        format!("ext:{rest}")
    } else if let Some(rest) = store_id.strip_prefix("package:") {
        format!("pkg:{rest}")
    } else {
        store_id.to_string()
    }
}

/// Store id of an API id (accepts store ids too). `dir:` (empty path) is the root.
pub fn store_id(api: &str) -> String {
    if let Some(rest) = api.strip_prefix("ext:") {
        format!("external:{rest}")
    } else if let Some(rest) = api.strip_prefix("pkg:") {
        format!("package:{rest}")
    } else if api == "dir:" {
        "repo:".to_string()
    } else {
        api.to_string()
    }
}

/// A node of the visible frontier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeRef {
    Dir(DirIx),
    File(FileIx),
}

/// How deep a graph view opens the tree: every directory above level `n` (the top-level
/// entries are level 1, so `Levels(1)` opens nothing but the root), or all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Levels(u32),
    All,
}

/// What `/api/graph` opens: a level plus explicit overrides (API ids).
#[derive(Debug, Clone, Copy)]
pub struct GraphQuery<'a> {
    pub depth: Depth,
    pub expanded: &'a [String],
    pub collapsed: &'a [String],
}

/// Result of [`Snapshot::rollup`].
#[derive(Debug, Clone, Default)]
pub struct Rollup {
    /// Frontier nodes, in tree order (dirs before files at each level).
    pub nodes: Vec<NodeRef>,
    /// (src, dst) → (weight, [exact, resolved, heuristic]).
    pub edges: BTreeMap<(NodeRef, NodeRef), (u64, [u64; 3])>,
    /// (src, ext) → weight.
    pub externals: BTreeMap<(NodeRef, ExtIx), u64>,
    /// (src, dst) → (bridged sites, [exact, resolved, heuristic]): bridges, apart from the
    /// imports in `edges`.
    pub bridges: BTreeMap<(NodeRef, NodeRef), (u64, [u64; 3])>,
    /// Requested ids that were not applied.
    pub ignored: Vec<String>,
    /// Namespace-package imports whose target dir is expanded.
    pub hidden_namespace_imports: u64,
}

fn conf_slot(c: Confidence) -> usize {
    match c {
        Confidence::Exact => 0,
        Confidence::Resolved => 1,
        Confidence::Heuristic => 2,
    }
}

impl Snapshot {
    /// Build from a graph and its run metadata (see `cartograph_index::index_to_store`).
    pub fn build(graph: Graph, meta: &BTreeMap<String, String>, generation: u64) -> Snapshot {
        Snapshot::build_with(graph, meta, generation, Ok(Config::default()))
    }

    /// Build with the repo's `cartograph.toml` (or the error reading it: findings then use
    /// the defaults and the error is reported in `/api/findings`, never fatal).
    pub fn build_with(
        graph: Graph,
        meta: &BTreeMap<String, String>,
        generation: u64,
        config: Result<Config, ConfigError>,
    ) -> Snapshot {
        Snapshot::build_full(graph, meta, generation, config, &History::default())
    }

    /// Build with the config and the git history (hidden-coupling findings need it).
    pub fn build_full(
        graph: Graph,
        meta: &BTreeMap<String, String>,
        generation: u64,
        config: Result<Config, ConfigError>,
        history: &History,
    ) -> Snapshot {
        Snapshot::build_all(
            graph,
            meta,
            generation,
            config,
            history,
            &Symbols::default(),
        )
    }

    /// Build with the config, the git history and the symbol layer (the flow endpoints
    /// need it; without it they answer `indexed: false`). Findings never read symbols.
    pub fn build_all(
        graph: Graph,
        meta: &BTreeMap<String, String>,
        generation: u64,
        config: Result<Config, ConfigError>,
        history: &History,
        symbols: &Symbols,
    ) -> Snapshot {
        let (config, config_error) = match config {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(e.to_string())),
        };
        let config_origin = config.origin.clone();
        let repo = Repo::with_symbols(&graph, symbols, config).with_history(history);
        let findings = repo.findings().to_vec();
        let calls = CallIndex::build(&repo);
        let mut snap = Snapshot::build_graph(graph, meta, generation);
        snap.flows = Some(FlowLayer { repo, calls });
        snap.counts.findings = findings
            .iter()
            .filter(|f| f.severity != Severity::Info)
            .count() as u64;
        snap.counts.findings_info = findings.len() as u64 - snap.counts.findings;
        snap.findings = findings;
        snap.config_origin = config_origin;
        snap.config_error = config_error;
        snap
    }

    fn build_graph(graph: Graph, meta: &BTreeMap<String, String>, generation: u64) -> Snapshot {
        // Dirs: every Dir node plus every ancestor of every file, parents before children.
        let mut paths: HashSet<String> = HashSet::new();
        paths.insert(String::new());
        let add_chain = |mut p: &str, paths: &mut HashSet<String>| {
            while !p.is_empty() && paths.insert(p.to_string()) {
                p = parent_of(p);
            }
        };
        for n in &graph.nodes {
            match n.kind {
                NodeKind::Dir => add_chain(&n.path, &mut paths),
                NodeKind::File => add_chain(parent_of(&n.path), &mut paths),
                _ => {}
            }
        }
        let mut dir_paths: Vec<String> = paths.into_iter().collect();
        dir_paths.sort();
        let dir_ix: HashMap<String, DirIx> = dir_paths
            .iter()
            .enumerate()
            .map(|(i, p)| (p.clone(), i as DirIx))
            .collect();
        let mut dirs: Vec<Dir> = dir_paths
            .iter()
            .map(|p| Dir {
                name: if p.is_empty() {
                    String::new()
                } else {
                    last_segment(p).to_string()
                },
                parent: if p.is_empty() {
                    None
                } else {
                    dir_ix.get(parent_of(p)).copied()
                },
                path: p.clone(),
                dirs: Vec::new(),
                files: Vec::new(),
                file_count: 0,
                loc: 0,
                langs: BTreeMap::new(),
                fan_in: 0,
                fan_out: 0,
                unresolved: 0,
            })
            .collect();
        for i in 1..dirs.len() {
            if let Some(p) = dirs[i].parent {
                dirs[p as usize].dirs.push(i as DirIx);
            }
        }
        let names: Vec<String> = dirs.iter().map(|d| d.name.clone()).collect();
        for d in &mut dirs {
            d.dirs
                .sort_by(|a, b| names[*a as usize].cmp(&names[*b as usize]));
        }

        // Files, sorted by path.
        let owners = PackageOwners::new(&graph);
        let mut file_nodes: Vec<_> = graph.nodes_of(NodeKind::File).collect();
        file_nodes.sort_by(|a, b| a.path.cmp(&b.path));
        let mut files: Vec<File> = Vec::with_capacity(file_nodes.len());
        let mut file_ix: HashMap<String, FileIx> = HashMap::with_capacity(file_nodes.len());
        for n in &file_nodes {
            let dir = dir_ix.get(parent_of(&n.path)).copied().unwrap_or(ROOT);
            file_ix.insert(n.path.clone(), files.len() as FileIx);
            files.push(File {
                path: n.path.clone(),
                name: last_segment(&n.path).to_string(),
                dir,
                lang: n.lang.clone(),
                loc: n.loc,
                fan_in: 0,
                fan_out: 0,
                unresolved: 0,
                package: owners.of(&n.path, n.lang.as_deref()).map(api_id),
            });
        }
        drop(owners);
        for (i, f) in files.iter().enumerate() {
            dirs[f.dir as usize].files.push(i as FileIx);
        }
        for d in &mut dirs {
            d.files
                .sort_by(|a, b| files[*a as usize].name.cmp(&files[*b as usize].name));
        }

        // Unresolved per file.
        let mut unresolved_by_file: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, u) in graph.unresolved.iter().enumerate() {
            unresolved_by_file
                .entry(u.file.clone())
                .or_default()
                .push(i);
        }
        for f in &mut files {
            f.unresolved = unresolved_by_file
                .get(&f.path)
                .map_or(0, |v| v.len() as u64);
        }

        // Totals up the dir chain.
        for f in &files {
            let lang = f.lang.clone().unwrap_or_else(|| "other".to_string());
            let mut d = Some(f.dir);
            while let Some(i) = d {
                let dir = &mut dirs[i as usize];
                dir.file_count += 1;
                dir.loc += f.loc;
                dir.unresolved += f.unresolved;
                *dir.langs.entry(lang.clone()).or_insert(0) += 1;
                d = dir.parent;
            }
        }

        // Externals and edges.
        let mut externals: Vec<External> = graph
            .nodes_of(NodeKind::External)
            .map(|n| External {
                id: api_id(&n.id),
                name: n.path.clone(),
            })
            .collect();
        externals.sort_by(|a, b| a.id.cmp(&b.id));
        let ext_ix: HashMap<String, ExtIx> = externals
            .iter()
            .enumerate()
            .map(|(i, e)| (e.id.clone(), i as ExtIx))
            .collect();
        let mut file_edges = Vec::new();
        let mut bridge_edges = Vec::new();
        for e in graph.edges.iter().filter(|e| e.kind == EdgeKind::Bridge) {
            let src = e.src.strip_prefix("file:").and_then(|p| file_ix.get(p));
            let dst = e.dst.strip_prefix("file:").and_then(|p| file_ix.get(p));
            if let (Some(&src), Some(&dst)) = (src, dst) {
                bridge_edges.push(BridgeEdge {
                    src,
                    dst,
                    weight: e.weight,
                    confidence: e.confidence,
                });
            }
        }
        let mut ns_edges = Vec::new();
        let mut ext_edges = Vec::new();
        let mut confidence = HashMap::new();
        for e in &graph.edges {
            if !matches!(e.kind, EdgeKind::Import | EdgeKind::ImportExternal) {
                continue;
            }
            confidence.insert((e.src.clone(), e.dst.clone()), e.confidence);
            let Some(src) = e.src.strip_prefix("file:").and_then(|p| file_ix.get(p)) else {
                continue;
            };
            if let Some(dst) = e.dst.strip_prefix("file:").and_then(|p| file_ix.get(p)) {
                file_edges.push(FileEdge {
                    src: *src,
                    dst: *dst,
                    confidence: e.confidence,
                });
            } else if let Some(dst) = e.dst.strip_prefix("dir:").and_then(|p| dir_ix.get(p)) {
                ns_edges.push(NsEdge {
                    src: *src,
                    dst: *dst,
                    confidence: e.confidence,
                });
            } else if let Some(ext) = ext_ix.get(&api_id(&e.dst)) {
                ext_edges.push(ExtEdge {
                    src: *src,
                    ext: *ext,
                });
            }
        }

        // Fan-in / fan-out: files by distinct file pairs, dirs by distinct outside files.
        let chain = |mut d: DirIx, dirs: &[Dir]| {
            let mut v = vec![d];
            while let Some(p) = dirs[d as usize].parent {
                v.push(p);
                d = p;
            }
            v
        };
        let mut dir_in: HashSet<(DirIx, FileIx)> = HashSet::new();
        let mut dir_out: HashSet<(DirIx, FileIx)> = HashSet::new();
        for e in &file_edges {
            if e.src == e.dst {
                continue;
            }
            files[e.src as usize].fan_out += 1;
            files[e.dst as usize].fan_in += 1;
            let ca = chain(files[e.src as usize].dir, &dirs);
            let cb = chain(files[e.dst as usize].dir, &dirs);
            let common: HashSet<DirIx> = ca.iter().copied().filter(|d| cb.contains(d)).collect();
            for d in cb.iter().filter(|d| !common.contains(d)) {
                dir_in.insert((*d, e.src));
            }
            for d in ca.iter().filter(|d| !common.contains(d)) {
                dir_out.insert((*d, e.dst));
            }
        }
        for (d, _) in dir_in {
            dirs[d as usize].fan_in += 1;
        }
        for (d, _) in dir_out {
            dirs[d as usize].fan_out += 1;
        }

        // Import site lookups.
        let mut sites_by_file: HashMap<String, Vec<usize>> = HashMap::new();
        let mut sites_by_target: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, s) in graph.imports.iter().enumerate() {
            sites_by_file.entry(s.file.clone()).or_default().push(i);
            sites_by_target.entry(s.target.clone()).or_default().push(i);
        }

        // Packages.
        let mut packages: Vec<Package> = graph
            .nodes_of(NodeKind::Package)
            .map(|n| {
                let dir = n.meta["dir"].as_str().unwrap_or("");
                Package {
                    id: api_id(&n.id),
                    name: n.name.clone(),
                    kind: n.meta["manifest"].as_str().unwrap_or("").to_string(),
                    dir: if dir.is_empty() {
                        "repo:".to_string()
                    } else {
                        format!("dir:{dir}")
                    },
                    manifest: format!("file:{}", n.path),
                    notes: n.meta["error"].as_str().map(str::to_string),
                }
            })
            .collect();
        packages.sort_by(|a, b| a.id.cmp(&b.id));

        // Counts and languages.
        let external_sites = graph
            .imports
            .iter()
            .filter(|s| s.target.starts_with("external:"))
            .count() as u64;
        let counts = Counts {
            files: files.len() as u64,
            dirs: dirs.len().saturating_sub(1) as u64,
            packages: packages.len() as u64,
            loc: dirs.first().map_or(0, |d| d.loc),
            imports: (graph.imports.len() + graph.unresolved.len()) as u64,
            resolved: graph.imports.len() as u64 - external_sites,
            external: external_sites,
            unresolved: graph.unresolved.len() as u64,
            skipped: graph.skipped.len() as u64,
            diagnostics: graph.diagnostics.len() as u64,
            findings: 0,
            findings_info: 0,
        };
        let mut by_lang: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
        for f in &files {
            let e = by_lang
                .entry(f.lang.as_deref().unwrap_or("other"))
                .or_insert((0, 0));
            e.0 += 1;
            e.1 += f.loc;
        }
        let mut languages: Vec<LangCount> = by_lang
            .into_iter()
            .map(|(l, (files, loc))| LangCount {
                lang: l.to_string(),
                files,
                loc,
            })
            .collect();
        languages.sort_by(|a, b| b.files.cmp(&a.files).then(a.lang.cmp(&b.lang)));

        // Search entries.
        let mut search = Vec::with_capacity(dirs.len() + files.len() + externals.len());
        for d in dirs.iter().skip(1) {
            search.push(SearchEntry {
                id: format!("dir:{}", d.path),
                kind: "dir",
                lower_path: d.path.to_lowercase(),
                lower_name: d.name.to_lowercase(),
                path: d.path.clone(),
                name: d.name.clone(),
            });
        }
        for f in &files {
            search.push(SearchEntry {
                id: format!("file:{}", f.path),
                kind: "file",
                lower_path: f.path.to_lowercase(),
                lower_name: f.name.to_lowercase(),
                path: f.path.clone(),
                name: f.name.clone(),
            });
        }
        for e in &externals {
            search.push(SearchEntry {
                id: e.id.clone(),
                kind: "ext",
                lower_path: e.name.to_lowercase(),
                lower_name: e.name.to_lowercase(),
                path: e.name.clone(),
                name: e.name.clone(),
            });
        }

        let repo_name = meta.get("repo_name").cloned().unwrap_or_else(|| {
            graph
                .node("repo:")
                .map_or_else(|| "repo".to_string(), |n| n.name.clone())
        });
        let indexed_at = meta
            .get("indexed_at_unix")
            .and_then(|s| s.parse::<u64>().ok())
            .map(|secs| {
                humantime::format_rfc3339_seconds(
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs),
                )
                .to_string()
            });
        let index_ms = meta
            .get("timings")
            .and_then(|t| serde_json::from_str::<Value>(t).ok())
            .and_then(|t| t["total_ms"].as_u64());
        if let Some(root) = dirs.first_mut() {
            root.name = repo_name.clone();
        }
        let workspace = workspace_meta(meta);
        let members = workspace
            .as_ref()
            .and_then(|w| w["members"].as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|m| m["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        Snapshot {
            workspace,
            members,
            generation,
            repo_name,
            indexed_at,
            index_ms,
            counts,
            languages,
            dirs,
            files,
            externals,
            file_edges,
            bridge_edges,
            ns_edges,
            ext_edges,
            packages,
            graph,
            dir_ix,
            file_ix,
            ext_ix,
            sites_by_file,
            sites_by_target,
            unresolved_by_file,
            confidence,
            search,
            findings: Vec::new(),
            config_origin: None,
            config_error: None,
            flows: None,
        }
    }

    /// Resolve an API (or store) id to a dir.
    /// `id` with a workspace alias spelled canonically (`dir:api:src` → `dir:api/src`).
    fn canonical<'i>(&self, id: &'i str) -> std::borrow::Cow<'i, str> {
        match cartograph_core::expand_alias(id, &self.members) {
            Some(x) => std::borrow::Cow::Owned(x),
            None => std::borrow::Cow::Borrowed(id),
        }
    }

    pub fn dir_of_id(&self, id: &str) -> Option<DirIx> {
        let id = store_id(&self.canonical(id));
        if id == "repo:" {
            return Some(ROOT);
        }
        id.strip_prefix("dir:")
            .and_then(|p| self.dir_ix.get(p))
            .copied()
    }

    pub fn file_of_id(&self, id: &str) -> Option<FileIx> {
        self.canonical(id)
            .strip_prefix("file:")
            .and_then(|p| self.file_ix.get(p))
            .copied()
    }

    /// The path of a file or folder id (`""` for the root), or `None` for an unknown one.
    pub fn path_of_id(&self, id: &str) -> Option<String> {
        if let Some(f) = self.file_of_id(id) {
            return Some(self.files[f as usize].path.clone());
        }
        self.dir_of_id(id)
            .map(|d| self.dirs[d as usize].path.clone())
    }

    pub fn ext_of_id(&self, id: &str) -> Option<ExtIx> {
        self.ext_ix.get(&api_id(&store_id(id))).copied()
    }

    pub fn dir_id(&self, d: DirIx) -> String {
        if d == ROOT {
            "repo:".to_string()
        } else {
            format!("dir:{}", self.dirs[d as usize].path)
        }
    }

    pub fn file_id(&self, f: FileIx) -> String {
        format!("file:{}", self.files[f as usize].path)
    }

    pub fn node_ref_id(&self, n: NodeRef) -> String {
        match n {
            NodeRef::Dir(d) => self.dir_id(d),
            NodeRef::File(f) => self.file_id(f),
        }
    }

    /// The visible frontier for `expanded` (API ids, in any order, duplicates allowed) and
    /// the import graph lifted onto it. See `docs/api-f2.md`.
    pub fn rollup(&self, expanded: &[String]) -> Rollup {
        self.rollup_view(&GraphQuery {
            depth: Depth::Levels(1),
            expanded,
            collapsed: &[],
        })
    }

    /// The frontier for a level plus explicit `expanded` / `collapsed` overrides, and the
    /// import graph lifted onto it. A directory is open when its parent is open, it is not
    /// collapsed, and it either lies above the level or was expanded.
    pub fn rollup_view(&self, q: &GraphQuery<'_>) -> Rollup {
        let n = self.dirs.len();
        let mut requested = vec![false; n];
        let mut closed = vec![false; n];
        let mut ignored: Vec<String> = Vec::new();
        let mut requested_ids: Vec<(&String, DirIx)> = Vec::new();
        for id in q.expanded {
            match self.dir_of_id(id) {
                Some(ROOT) => {}
                Some(d) => {
                    requested[d as usize] = true;
                    requested_ids.push((id, d));
                }
                None => ignored.push(id.clone()),
            }
        }
        for id in q.collapsed {
            match self.dir_of_id(id) {
                // The root is always open: collapsing it is not applied.
                Some(ROOT) | None => ignored.push(id.clone()),
                Some(d) => closed[d as usize] = true,
            }
        }
        // Parents come before children in `dirs`, so one pass settles "effectively expanded".
        let mut open = vec![false; n];
        let mut level = vec![0u32; n];
        if n > 0 {
            open[ROOT as usize] = true;
        }
        for i in 1..n {
            let p = self.dirs[i].parent.unwrap_or(ROOT) as usize;
            level[i] = level[p].saturating_add(1);
            let by_depth = match q.depth {
                Depth::All => true,
                Depth::Levels(l) => level[i] < l,
            };
            open[i] = open[p] && !closed[i] && (by_depth || requested[i]);
        }
        for (id, d) in requested_ids {
            if !open[d as usize] {
                ignored.push(id.clone());
            }
        }
        let mut seen = HashSet::new();
        ignored.retain(|id| seen.insert(id.clone()));

        // Representative of each dir: itself when its parent is open, else its parent's.
        let mut rep = vec![NodeRef::Dir(ROOT); n];
        for i in 1..n {
            let p = self.dirs[i].parent.unwrap_or(ROOT);
            rep[i] = if open[p as usize] {
                NodeRef::Dir(i as DirIx)
            } else {
                rep[p as usize]
            };
        }
        let file_rep = |f: FileIx| {
            let d = self.files[f as usize].dir;
            if open[d as usize] {
                NodeRef::File(f)
            } else {
                rep[d as usize]
            }
        };

        let mut out = Rollup {
            ignored,
            ..Rollup::default()
        };
        // Frontier in tree order: an open dir's children take its place.
        let mut stack: Vec<NodeRef> = vec![NodeRef::Dir(ROOT)];
        while let Some(n) = stack.pop() {
            match n {
                NodeRef::Dir(d) if open[d as usize] => {
                    let dir = &self.dirs[d as usize];
                    let children = dir
                        .dirs
                        .iter()
                        .map(|&c| NodeRef::Dir(c))
                        .chain(dir.files.iter().map(|&f| NodeRef::File(f)));
                    let children: Vec<NodeRef> = children.collect();
                    stack.extend(children.into_iter().rev());
                }
                other => out.nodes.push(other),
            }
        }

        for e in &self.file_edges {
            let (a, b) = (file_rep(e.src), file_rep(e.dst));
            if a != b {
                let slot = out.edges.entry((a, b)).or_insert((0, [0; 3]));
                slot.0 += 1;
                slot.1[conf_slot(e.confidence)] += 1;
            }
        }
        for e in &self.ns_edges {
            if open[e.dst as usize] {
                out.hidden_namespace_imports += 1;
                continue;
            }
            let (a, b) = (file_rep(e.src), rep[e.dst as usize]);
            if a != b {
                let slot = out.edges.entry((a, b)).or_insert((0, [0; 3]));
                slot.0 += 1;
                slot.1[conf_slot(e.confidence)] += 1;
            }
        }
        for e in &self.ext_edges {
            *out.externals.entry((file_rep(e.src), e.ext)).or_insert(0) += 1;
        }
        for e in &self.bridge_edges {
            let (a, b) = (file_rep(e.src), file_rep(e.dst));
            if a != b {
                let slot = out.bridges.entry((a, b)).or_insert((0, [0; 3]));
                slot.0 += e.weight;
                slot.1[conf_slot(e.confidence)] += e.weight;
            }
        }
        out
    }

    /// JSON of a dir or file as in `/api/tree` (without children).
    pub fn node_json(&self, n: NodeRef) -> serde_json::Map<String, Value> {
        let v = match n {
            NodeRef::Dir(d) => {
                let dir = &self.dirs[d as usize];
                json!({
                    "id": self.dir_id(d),
                    "kind": if d == ROOT { "repo" } else { "dir" },
                    "name": dir.name,
                    "path": dir.path,
                    "loc": dir.loc,
                    "files": dir.file_count,
                    "fan_in": dir.fan_in,
                    "fan_out": dir.fan_out,
                    "unresolved": dir.unresolved,
                    "langs": dir.langs,
                })
            }
            NodeRef::File(f) => {
                let file = &self.files[f as usize];
                json!({
                    "id": self.file_id(f),
                    "kind": "file",
                    "name": file.name,
                    "path": file.path,
                    "lang": file.lang,
                    "loc": file.loc,
                    "fan_in": file.fan_in,
                    "fan_out": file.fan_out,
                    "unresolved": file.unresolved,
                })
            }
        };
        match v {
            Value::Object(m) => m,
            _ => serde_json::Map::new(),
        }
    }

    /// `/api/tree` body below `root`, `depth` levels deep (`None` = all).
    pub fn tree(&self, root: DirIx, depth: Option<u32>) -> Value {
        Value::Object(self.tree_node(root, depth))
    }

    fn tree_node(&self, d: DirIx, depth: Option<u32>) -> serde_json::Map<String, Value> {
        let mut m = self.node_json(NodeRef::Dir(d));
        let dir = &self.dirs[d as usize];
        let has_children = !dir.dirs.is_empty() || !dir.files.is_empty();
        if depth == Some(0) {
            if has_children {
                m.insert("truncated".into(), Value::Bool(true));
            }
            return m;
        }
        let next = depth.map(|x| x - 1);
        let mut children = Vec::with_capacity(dir.dirs.len() + dir.files.len());
        for &c in &dir.dirs {
            children.push(Value::Object(self.tree_node(c, next)));
        }
        for &f in &dir.files {
            children.push(Value::Object(self.node_json(NodeRef::File(f))));
        }
        m.insert("children".into(), Value::Array(children));
        m
    }

    /// `/api/graph` body (without `generation`).
    pub fn graph_view(&self, q: &GraphQuery<'_>) -> Value {
        let r = self.rollup_view(q);
        let mut fan_in: HashMap<NodeRef, u64> = HashMap::new();
        let mut fan_out: HashMap<NodeRef, u64> = HashMap::new();
        for ((a, b), (w, _)) in &r.edges {
            *fan_out.entry(*a).or_insert(0) += w;
            *fan_in.entry(*b).or_insert(0) += w;
        }
        let nodes: Vec<Value> = r
            .nodes
            .iter()
            .map(|n| {
                let mut m = self.node_json(*n);
                let (parent, expandable) = match n {
                    NodeRef::Dir(d) => {
                        let dir = &self.dirs[*d as usize];
                        (
                            dir.parent.unwrap_or(ROOT),
                            !dir.dirs.is_empty() || !dir.files.is_empty(),
                        )
                    }
                    NodeRef::File(f) => {
                        let file = &self.files[*f as usize];
                        let lang = file.lang.clone().unwrap_or_else(|| "other".into());
                        m.insert("files".into(), json!(1));
                        m.insert("langs".into(), json!({ lang: 1 }));
                        (file.dir, false)
                    }
                };
                m.insert("parent".into(), Value::String(self.dir_id(parent)));
                m.insert("expandable".into(), Value::Bool(expandable));
                m.insert("fan_in".into(), json!(fan_in.get(n).copied().unwrap_or(0)));
                m.insert(
                    "fan_out".into(),
                    json!(fan_out.get(n).copied().unwrap_or(0)),
                );
                Value::Object(m)
            })
            .collect();
        let edges: Vec<Value> = r
            .edges
            .iter()
            .map(|((a, b), (w, c))| {
                json!({
                    "src": self.node_ref_id(*a),
                    "dst": self.node_ref_id(*b),
                    "weight": w,
                    "confidence": { "exact": c[0], "resolved": c[1], "heuristic": c[2] },
                })
            })
            .collect();
        let externals: Vec<Value> = r
            .externals
            .iter()
            .map(|((a, e), w)| {
                json!({
                    "src": self.node_ref_id(*a),
                    "ext": self.externals[*e as usize].id,
                    "weight": w,
                })
            })
            .collect();
        let bridges: Vec<Value> = r
            .bridges
            .iter()
            .map(|((a, b), (w, c))| {
                json!({
                    "src": self.node_ref_id(*a),
                    "dst": self.node_ref_id(*b),
                    "weight": w,
                    "confidence": { "exact": c[0], "resolved": c[1], "heuristic": c[2] },
                })
            })
            .collect();
        json!({
            "nodes": nodes,
            "edges": edges,
            "bridges": bridges,
            "externals": externals,
            "ignored": r.ignored,
            "hidden_namespace_imports": r.hidden_namespace_imports,
        })
    }

    /// `/api/node` body (without `generation`), or `None` for an unknown id.
    pub fn node_detail(&self, id: &str) -> Option<Value> {
        if let Some(f) = self.file_of_id(id) {
            return Some(self.file_detail(f));
        }
        if let Some(d) = self.dir_of_id(id) {
            return Some(self.dir_detail(d));
        }
        if let Some(e) = self.ext_of_id(id) {
            let ext = &self.externals[e as usize];
            let importers: Vec<Value> = self
                .sites_by_target
                .get(&store_id(&ext.id))
                .map(|v| {
                    v.iter()
                        .map(|&i| {
                            let s = &self.graph.imports[i];
                            json!({ "source": format!("file:{}", s.file), "specifier": s.specifier, "line": s.line })
                        })
                        .collect()
                })
                .unwrap_or_default();
            return Some(json!({
                "node": { "id": ext.id, "kind": "ext", "name": ext.name, "path": ext.name },
                "importers": importers,
            }));
        }
        let api = api_id(&store_id(id));
        self.packages
            .iter()
            .find(|p| p.id == api)
            .map(|p| json!({ "node": p }))
    }

    fn file_detail(&self, f: FileIx) -> Value {
        let file = &self.files[f as usize];
        let src_id = format!("file:{}", file.path);
        let conf = |a: &str, b: &str| {
            self.confidence
                .get(&(a.to_string(), b.to_string()))
                .map(|c| c.as_str())
        };
        let mut imports = Vec::new();
        let mut externals = Vec::new();
        for &i in self.sites_by_file.get(&file.path).into_iter().flatten() {
            let s = &self.graph.imports[i];
            if s.target.starts_with("external:") {
                externals.push(
                    json!({ "ext": api_id(&s.target), "specifier": s.specifier, "line": s.line }),
                );
            } else {
                imports.push(json!({
                    "target": api_id(&s.target),
                    "specifier": s.specifier,
                    "line": s.line,
                    "confidence": conf(&src_id, &s.target),
                }));
            }
        }
        let importers: Vec<Value> = self
            .sites_by_target
            .get(&src_id)
            .into_iter()
            .flatten()
            .map(|&i| {
                let s = &self.graph.imports[i];
                let source = format!("file:{}", s.file);
                json!({ "source": source, "line": s.line, "confidence": conf(&source, &src_id) })
            })
            .collect();
        let unresolved: Vec<Value> = self
            .unresolved_by_file
            .get(&file.path)
            .into_iter()
            .flatten()
            .map(|&i| {
                let u = &self.graph.unresolved[i];
                json!({ "line": u.line, "specifier": u.specifier, "reason": u.reason.as_str(), "detail": u.detail })
            })
            .collect();
        json!({
            "node": self.node_json(NodeRef::File(f)),
            "imports": imports,
            "importers": importers,
            "externals": externals,
            "unresolved": unresolved,
            "package": file.package,
            "bridges": self.bridges_json(Some(&file.path), usize::MAX)["bridges"],
        })
    }

    /// `/api/bridges` body (without `generation`): the bridges with an end in `path` (a
    /// file, or a folder and everything under it; every bridge without one), at most `limit`,
    /// and the endpoints left unmatched there. Empty without a symbol layer.
    pub fn bridges_json(&self, path: Option<&str>, limit: usize) -> Value {
        let Some(layer) = &self.flows else {
            return json!({ "bridges": [], "unmatched": [], "total": 0 });
        };
        let t = &layer.repo.symbols.bridges;
        let under = |p: &str| match path {
            None | Some("") => true,
            Some(x) => p == x || p.strip_prefix(x).is_some_and(|r| r.starts_with('/')),
        };
        let api = |p: &str| api_id(&format!("file:{p}"));
        let rows: Vec<&cartograph_core::Bridge> = t
            .bridges
            .iter()
            .filter(|b| under(&b.src_file) || under(&b.dst_file))
            .collect();
        let bridges: Vec<Value> = rows
            .iter()
            .take(limit)
            .map(|b| {
                json!({
                    "kind": b.kind,
                    "subtype": b.subtype,
                    "key": b.key,
                    "src": api(&b.src_file),
                    "src_line": b.src_line,
                    "src_symbol": b.src,
                    "dst": api(&b.dst_file),
                    "dst_line": b.dst_line,
                    "dst_symbol": b.dst,
                    "confidence": b.confidence,
                })
            })
            .collect();
        let unmatched: Vec<Value> = t
            .endpoints
            .iter()
            .filter(|e| e.detail.is_some() && under(&e.file))
            .take(limit)
            .map(|e| {
                json!({
                    "kind": e.kind,
                    "role": e.role,
                    "key": e.key,
                    "label": e.label,
                    "file": api(&e.file),
                    "line": e.line,
                    "symbol": e.symbol,
                    "detail": e.detail,
                })
            })
            .collect();
        json!({ "bridges": bridges, "unmatched": unmatched, "total": rows.len() })
    }

    fn dir_detail(&self, d: DirIx) -> Value {
        let dir = &self.dirs[d as usize];
        // Children with the most LOC.
        let mut children: Vec<(NodeRef, u64, u64)> = dir
            .dirs
            .iter()
            .map(|&c| {
                let x = &self.dirs[c as usize];
                (NodeRef::Dir(c), x.loc, x.file_count)
            })
            .chain(
                dir.files
                    .iter()
                    .map(|&f| (NodeRef::File(f), self.files[f as usize].loc, 1)),
            )
            .collect();
        children.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let children: Vec<Value> = children
            .iter()
            .take(10)
            .map(|(n, loc, files)| {
                let (kind, name) = match n {
                    NodeRef::Dir(c) => ("dir", self.dirs[*c as usize].name.as_str()),
                    NodeRef::File(f) => ("file", self.files[*f as usize].name.as_str()),
                };
                json!({ "id": self.node_ref_id(*n), "kind": kind, "name": name, "loc": loc, "files": files })
            })
            .collect();

        // Dependencies at sibling level: expand every ancestor of `d`.
        let mut ancestors = Vec::new();
        let mut p = dir.parent;
        while let Some(a) = p {
            if a != ROOT {
                ancestors.push(self.dir_id(a));
            }
            p = self.dirs[a as usize].parent;
        }
        let me = NodeRef::Dir(d);
        let (mut deps_out, mut deps_in, mut ext) = (Vec::new(), Vec::new(), Vec::new());
        if d != ROOT {
            let r = self.rollup(&ancestors);
            for ((a, b), (w, _)) in &r.edges {
                if *a == me {
                    deps_out.push((*b, *w));
                } else if *b == me {
                    deps_in.push((*a, *w));
                }
            }
            for ((a, e), w) in &r.externals {
                if *a == me {
                    ext.push((*e, *w));
                }
            }
        } else {
            let mut m: HashMap<ExtIx, u64> = HashMap::new();
            for e in &self.ext_edges {
                *m.entry(e.ext).or_insert(0) += 1;
            }
            ext = m.into_iter().collect();
        }
        let top = |mut v: Vec<(NodeRef, u64)>| -> Vec<Value> {
            v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            v.into_iter()
                .take(10)
                .map(|(n, w)| json!({ "id": self.node_ref_id(n), "weight": w }))
                .collect()
        };
        ext.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let ext: Vec<Value> = ext
            .into_iter()
            .take(10)
            .map(|(e, w)| json!({ "ext": self.externals[e as usize].id, "weight": w }))
            .collect();
        json!({
            "node": self.node_json(me),
            "children": children,
            "deps_out": top(deps_out),
            "deps_in": top(deps_in),
            "externals": ext,
        })
    }

    /// Fuzzy search over dir and file paths and external names.
    pub fn search(&self, q: &str, limit: usize) -> Vec<Value> {
        let q = q.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<(f64, &SearchEntry)> = self
            .search
            .iter()
            .filter_map(|e| score(&q, e).map(|s| (s, e)))
            .collect();
        hits.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.path.len().cmp(&b.1.path.len()))
                .then(a.1.path.cmp(&b.1.path))
        });
        hits.into_iter()
            .take(limit)
            .map(|(s, e)| {
                json!({ "id": e.id, "kind": e.kind, "path": e.path, "name": e.name, "score": (s * 1000.0).round() / 1000.0 })
            })
            .collect()
    }
}

fn is_subsequence(q: &str, hay: &str) -> bool {
    let mut it = hay.chars();
    q.chars().all(|c| it.any(|h| h == c))
}

/// Score of `q` (lowercase, non-empty) against an entry, or `None` if it does not match.
/// Every query character must appear in order in the path. Contiguous runs, matches at
/// word starts and matches in the name score higher; long paths score a little lower.
fn score(q: &str, e: &SearchEntry) -> Option<f64> {
    let mut s = 0.0;
    if e.lower_name == q {
        s += 100.0;
    } else if e.lower_name.starts_with(q) {
        s += 60.0;
    } else if e.lower_name.contains(q) {
        s += 40.0;
    } else if e.lower_path.contains(q) {
        s += 25.0;
    } else if is_subsequence(q, &e.lower_name) {
        s += 10.0;
    }
    let hay: Vec<char> = e.lower_path.chars().collect();
    let mut hi = 0;
    let mut prev: Option<usize> = None;
    let mut run = 0.0;
    for qc in q.chars() {
        let mut found = None;
        while hi < hay.len() {
            if hay[hi] == qc {
                found = Some(hi);
                hi += 1;
                break;
            }
            hi += 1;
        }
        let i = found?;
        let boundary = i == 0 || matches!(hay[i - 1], '/' | '-' | '_' | '.' | ' ' | ':');
        if prev.is_some_and(|p| p + 1 == i) {
            run += 1.0;
            s += 2.0 + run;
        } else {
            run = 0.0;
            s += 1.0;
        }
        if boundary {
            s += 3.0;
        }
        prev = Some(i);
    }
    s -= hay.len() as f64 * 0.05;
    Some(s.max(0.0))
}
