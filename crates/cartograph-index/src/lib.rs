//! Indexer: walk a repo, detect languages and manifests, and build the structural graph
//! repo → package / dir → dir → file (N0); extract and resolve TS/JS and Python imports into
//! a file graph with external dependencies and unresolved records, plus derived Dir → Dir
//! and Package → Package dependencies (N1).
//!
//! Library code logs through `tracing` only and never writes to stdout.

// Library code must not panic or write to stdout/stderr (logs go through `tracing`).
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr
    )
)]

pub mod aggregate;
pub mod git;
pub mod godot;
pub mod grammars;
pub mod imports;
pub mod lang;
pub mod manifest;
pub mod noise;
mod paths;
pub mod report;
pub mod resolve;
pub mod scip;
pub mod spec;
pub mod symbols;
mod syntax;
pub mod walk;

use cartograph_core::{
    Confidence, Edge, EdgeKind, Graph, Node, NodeKind, SkipReason, SkippedFile, Store, node_id,
};
use manifest::Manifest;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Directory, relative to the indexed repo, that holds the graph database.
pub const DATA_DIR: &str = ".cartograph";
/// File name of the graph database inside [`DATA_DIR`].
pub const DB_FILE: &str = "graph.db";

/// Every file cartograph keeps directly in a data dir: the database and its SQLite side
/// files, the MCP server's lock, the usage log, its rotation and lock
/// (`cartograph_mcp::stats`), and the notes of `annotate` (written through a `.tmp`). When
/// the data dir *is* the repo root, these are the paths the walk and the watcher leave out
/// (otherwise the whole data dir is).
pub const DATA_FILES: [&str; 10] = [
    "graph.db",
    "graph.db-wal",
    "graph.db-shm",
    "graph.db-journal",
    "mcp.lock",
    "stats.jsonl",
    "stats.1.jsonl",
    "stats.lock",
    "annotations.json",
    "annotations.json.tmp",
];
/// Folders cartograph keeps directly in a data dir: the cached base snapshots of `diff`
/// and `check --base` ([`git::snapshot::SNAPSHOT_DIR`], `.cartograph-base`, a name no
/// source folder has by accident). Left out of the walk and the watcher, with everything in
/// them, when the data dir is the repo root.
pub const DATA_DIRS: [&str; 1] = [git::snapshot::SNAPSHOT_DIR];
/// The notes of `annotate` kept in the repository (versioned with it), and the temporary
/// file they are written through. Read by `explain`; at the repo root they are never
/// indexed nor watched, so writing a note changes no content hash.
pub const NOTES_FILE: &str = "cartograph-notes.toml";
pub const NOTES_FILES: [&str; 2] = [NOTES_FILE, "cartograph-notes.toml.tmp"];
/// Name of the repo config file (architecture rules; read by `cartograph-query`).
pub const CONFIG_FILE: &str = "cartograph.toml";
/// Run-metadata key of the size limit ([`IndexOptions::max_file_bytes`]) the stored index
/// was built with.
pub const META_MAX_FILE_BYTES: &str = "max_file_bytes";

/// Errors from indexing.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("{0} is not a directory")]
    NotADirectory(PathBuf),
    #[error("i/o error on {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Store(#[from] cartograph_core::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("indexing was cancelled")]
    Cancelled,
    /// Reading a git revision failed (unknown revision, no repository, damaged objects).
    #[error("{0}")]
    Git(String),
}

/// Indexing options.
#[derive(Debug, Clone)]
pub struct IndexOptions {
    /// Files larger than this are skipped as too large.
    pub max_file_bytes: u64,
    /// Where the database lives. `None` = `<root>/.cartograph`. When set, nothing is ever
    /// written inside the indexed repo (and the dir is excluded from the walk if it happens
    /// to be inside it).
    pub data_dir: Option<PathBuf>,
    /// Counters updated while files are read, for progress reports.
    pub progress: Option<Arc<Progress>>,
    /// SCIP indexes to import (N3), besides `<root>/index.scip`. Empty: the ones an earlier
    /// run of [`index_to_store`] was given, if they still exist.
    pub scip: Vec<PathBuf>,
    /// Import no SCIP index, not even `<root>/index.scip`, and forget the remembered ones.
    pub no_scip: bool,
}

impl Default for IndexOptions {
    fn default() -> Self {
        IndexOptions {
            max_file_bytes: walk::DEFAULT_MAX_FILE_BYTES,
            data_dir: None,
            progress: None,
            scip: Vec::new(),
            no_scip: false,
        }
    }
}

/// Progress of the read phase: `total` is 0 while the tree is still being walked, then the
/// number of candidate files; `done` counts files read so far.
///
/// It also carries a cancellation flag: once [`Progress::cancel`] is called, an index using
/// it stops reading files and returns [`IndexError::Cancelled`] at the next check (between
/// files and between phases). The flag is never cleared.
#[derive(Debug, Default)]
pub struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    cancelled: std::sync::atomic::AtomicBool,
}

impl Progress {
    /// Reset for a run that will read `total` files.
    pub fn start(&self, total: u64) {
        self.done.store(0, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
    }

    /// One more file read.
    pub fn advance(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }

    /// Back to "walking" (`total` 0).
    pub fn reset(&self) {
        self.start(0);
    }

    /// Ask every index using this to stop.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// `(done, total)`.
    pub fn get(&self) -> (u64, u64) {
        (
            self.done.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        )
    }
}

/// Wall-clock time spent in each phase, in milliseconds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Timings {
    pub walk_ms: u128,
    /// Reading, hashing and parsing imports (in parallel).
    pub read_ms: u128,
    pub build_ms: u128,
    /// Resolving imports and deriving dependency edges.
    pub resolve_ms: u128,
    /// Resolving calls and entry points (N2).
    pub symbols_ms: u128,
    /// Merging SCIP indexes (N3), decoding included.
    pub scip_ms: u128,
    pub write_ms: u128,
    pub total_ms: u128,
    /// Files whose raw imports came from the extraction cache.
    pub cache_hits: u64,
    /// Files with import support that had to be parsed.
    pub cache_misses: u64,
}

/// Result of indexing a repo in memory.
#[derive(Debug)]
pub struct IndexOutput {
    pub graph: Graph,
    /// The symbol layer (N2), kept apart from the file graph.
    pub symbols: cartograph_core::Symbols,
    pub manifests: Vec<Manifest>,
    pub timings: Timings,
}

/// Default data dir of a repo: `<root>/.cartograph`.
pub fn default_data_dir(root: &Path) -> PathBuf {
    root.join(DATA_DIR)
}

/// The data dir to use: `custom` if given, else [`default_data_dir`].
pub fn data_dir(root: &Path, custom: Option<&Path>) -> PathBuf {
    custom.map_or_else(|| default_data_dir(root), Path::to_path_buf)
}

/// The size limit the index in `data_dir` was built with, when there is one that records it
/// (see [`META_MAX_FILE_BYTES`]). Every re-index the CLI runs without an explicit
/// `--max-file-bytes` (`index`, `serve`, `mcp`) uses this one, so none undoes a
/// custom limit; `diff` and `check --base` build both sides with it too.
pub fn stored_max_file_bytes(data_dir: &Path) -> Option<u64> {
    let db = data_dir.join(DB_FILE);
    if !db.is_file() {
        return None;
    }
    Store::open(db)
        .ok()?
        .get_meta(META_MAX_FILE_BYTES)
        .ok()??
        .parse()
        .ok()
}

/// Path of the graph database for a repo root (default data dir).
pub fn db_path(root: &Path) -> PathBuf {
    default_data_dir(root).join(DB_FILE)
}

/// Display name of a repo root (its directory name, resolving `.` and friends).
pub fn repo_name(root: &Path) -> String {
    let named = |p: &Path| p.file_name().and_then(|n| n.to_str()).map(str::to_string);
    named(root)
        .or_else(|| std::fs::canonicalize(root).ok().and_then(|p| named(&p)))
        .unwrap_or_else(|| "repo".to_string())
}

/// Repo-relative paths to keep out of the walk for a data dir (both canonicalized; `dir`
/// must exist): the dir itself when it is inside `root`, or its files and folders
/// ([`DATA_FILES`], [`DATA_DIRS`]) when it *is* `root` (so the walk never reads a database
/// it is about to write, nor a log that grows with every tool call), with the snapshot
/// folder of older versions when it holds nothing else
/// ([`git::snapshot::only_legacy_snapshots`]).
fn data_dir_exclusions(root: &Path, dir: &Path) -> Vec<String> {
    let (Ok(root), Ok(dir)) = (std::fs::canonicalize(root), std::fs::canonicalize(dir)) else {
        return Vec::new();
    };
    let legacy = git::snapshot::LEGACY_SNAPSHOT_DIR;
    match walk::relative(&root, &dir) {
        Some(rel) if rel.is_empty() => DATA_FILES
            .iter()
            .chain(DATA_DIRS.iter())
            .chain(git::snapshot::only_legacy_snapshots(&dir.join(legacy)).then_some(&legacy))
            .map(|f| (*f).to_string())
            .collect(),
        Some(rel) => vec![rel],
        None => Vec::new(),
    }
}

/// Index `root` in memory (no extraction cache; SCIP indexes are decoded, not cached).
pub fn index(root: &Path, opts: &IndexOptions) -> Result<IndexOutput, IndexError> {
    let started = Instant::now();
    let inputs = scip::inputs(root, &opts.scip, &[], opts.no_scip);
    let loaded: Vec<scip::Loaded> = inputs.iter().map(|p| scip::load(root, p, None)).collect();
    let load_ms = started.elapsed().as_millis();
    let src = Source {
        scip: &loaded,
        ..Source::of(root)
    };
    let (mut out, _) = index_inner(&src, opts, None)?;
    out.timings.scip_ms += load_ms;
    Ok(out)
}

/// What [`index_inner`] reads.
struct Source<'a> {
    /// The tree to walk.
    walk_root: &'a Path,
    /// Where resolvers look on disk for files the walk did not index (a config in
    /// `node_modules`, an ignored file). For a git snapshot this is the working tree, so the
    /// base sees the untracked files a checkout of it in place would see.
    resolve_root: &'a Path,
    /// Name of the repo node.
    name: String,
    /// Resolve calls and entry points (N2).
    symbols: bool,
    /// SCIP indexes to merge (N3); used only with `symbols`.
    scip: &'a [scip::Loaded],
}

impl<'a> Source<'a> {
    fn of(root: &'a Path) -> Source<'a> {
        Source {
            walk_root: root,
            resolve_root: root,
            name: repo_name(root),
            symbols: true,
            scip: &[],
        }
    }
}

/// What a run did with the extraction cache, and what it read besides the walked files.
#[derive(Debug, Default)]
struct CacheDelta {
    /// Entries parsed in this run, ready to store.
    new: Vec<cartograph_core::CacheEntry>,
    /// Keys (see [`walk::cache_key`]) of every file with import support seen in this run.
    used: HashSet<String>,
    /// Paths under [`Source::resolve_root`] the resolvers looked at on disk (see
    /// [`resolve::Index::on_disk`]), sorted.
    disk_reads: Vec<String>,
}

fn index_inner(
    src: &Source<'_>,
    opts: &IndexOptions,
    cache: Option<&walk::ExtractCache>,
) -> Result<(IndexOutput, CacheDelta), IndexError> {
    let started = Instant::now();
    let root = src.walk_root;
    if !root.is_dir() {
        return Err(IndexError::NotADirectory(root.to_path_buf()));
    }
    tracing::info!(root = %root.display(), "indexing");
    let exclude: Vec<String> = opts
        .data_dir
        .as_deref()
        .map(|d| data_dir_exclusions(root, d))
        .unwrap_or_default();
    if let Some(p) = &opts.progress {
        p.reset();
    }
    let hooks = walk::WalkHooks {
        cache,
        progress: opts.progress.as_deref(),
    };
    let cancelled = || opts.progress.as_ref().is_some_and(|p| p.cancelled());
    if cancelled() {
        return Err(IndexError::Cancelled);
    }
    let mut walked = walk::walk(root, opts.max_file_bytes, &exclude, hooks);
    if cancelled() {
        return Err(IndexError::Cancelled);
    }

    let build_started = Instant::now();
    // Imports and config texts leave the records before `build_graph` consumes them.
    let mut file_imports: Vec<(String, &'static str, Vec<imports::RawImport>)> = Vec::new();
    let mut file_code: Vec<(String, &'static str, symbols::Code)> = Vec::new();
    let mut configs: Vec<(String, String)> = Vec::new();
    let mut delta = CacheDelta::default();
    let (mut cache_hits, mut cache_misses) = (0u64, 0u64);
    for f in &mut walked.files {
        if let Some(lang) = f.lang
            && imports::supports(lang)
        {
            let key = walk::cache_key(lang, &f.hash);
            if f.cached {
                cache_hits += 1;
            } else {
                cache_misses += 1;
                if !delta.used.contains(&key) {
                    delta.new.push(cartograph_core::CacheEntry {
                        hash: f.hash.clone(),
                        lang: lang.to_string(),
                        imports: imports::extract_json(&f.imports, f.code.as_ref())?,
                    });
                }
            }
            delta.used.insert(key);
        }
        if let Some(lang) = f.lang
            && !f.imports.is_empty()
        {
            file_imports.push((f.path.clone(), lang, std::mem::take(&mut f.imports)));
        }
        if let (Some(lang), Some(code)) = (f.lang, f.code.take()) {
            file_code.push((f.path.clone(), lang, code));
        }
        if let Some(text) = f.config.take() {
            configs.push((f.path.clone(), text));
        }
    }
    let (mut graph, manifests) = build_graph(&src.name, walked.files, walked.dirs, walked.skipped);
    let build_ms = build_started.elapsed().as_millis();

    let resolve_started = Instant::now();
    let res = {
        let ix = resolve::Index {
            root: src.resolve_root,
            files: graph
                .nodes_of(NodeKind::File)
                .map(|n| n.path.as_str())
                .collect(),
            dirs: graph
                .nodes_of(NodeKind::Dir)
                .map(|n| n.path.as_str())
                .collect(),
            skipped: graph
                .skipped
                .iter()
                .map(|s| (s.path.as_str(), s.reason))
                .collect(),
            configs: configs
                .iter()
                .map(|(p, t)| (p.as_str(), t.as_str()))
                .collect(),
            disk_reads: Default::default(),
        };
        let files: Vec<resolve::FileImports<'_>> = file_imports
            .iter()
            .map(|(path, lang, imports)| resolve::FileImports {
                path,
                lang,
                imports,
            })
            .collect();
        // Calls and entry handlers through a path (`krate::m::f()`, axum's
        // `get(handlers::list)`), resolved by the Rust import resolver.
        let rust_calls: Vec<resolve::RustCallPath> = file_code
            .iter()
            .filter(|(_, lang, _)| *lang == "rust")
            .flat_map(|(path, _, code)| {
                code.calls
                    .iter()
                    .chain(code.entries.iter().filter_map(|e| e.handler.as_ref()))
                    .filter(|c| !c.path.is_empty())
                    .map(|c| (path.clone(), c.scope.clone(), c.path.clone()))
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let res = resolve::resolve_all(&ix, &files, &rust_calls);
        delta.disk_reads = ix.take_disk_reads();
        res
    };
    let rust_call_paths = res.rust_call_paths;
    if cancelled() {
        return Err(IndexError::Cancelled);
    }
    graph.edges.extend(res.edges);
    graph.nodes.extend(res.externals);
    // N3, pass 1: file-level evidence from SCIP, before the derived edges.
    let scip_started = Instant::now();
    let mut precise = if src.symbols && !src.scip.is_empty() {
        let files: HashSet<&str> = graph
            .nodes_of(NodeKind::File)
            .map(|n| n.path.as_str())
            .collect();
        Some(scip::apply::Prepared::new(src.scip, root, &files))
    } else {
        None
    };
    if let Some(p) = precise.as_mut() {
        let edges = p.file_edges(&graph);
        graph.edges.extend(edges);
    }
    let mut scip_ms = scip_started.elapsed().as_millis();
    graph.imports = res.sites;
    graph.unresolved = res.unresolved;
    graph.diagnostics = res.diagnostics;
    if !res.file_meta.is_empty() {
        // Several sources may add keys to one file (Godot data, type-only imports): merge.
        let mut meta: std::collections::HashMap<String, serde_json::Value> =
            std::collections::HashMap::new();
        for (path, m) in res.file_meta {
            merge_meta(meta.entry(path).or_insert(serde_json::Value::Null), m);
        }
        for n in graph.nodes.iter_mut().filter(|n| n.kind == NodeKind::File) {
            if let Some(m) = meta.remove(&n.path) {
                merge_meta(&mut n.meta, m);
            }
        }
    }
    graph.normalize();
    aggregate::derive(&mut graph);
    let resolve_ms = resolve_started.elapsed().as_millis();
    if cancelled() {
        return Err(IndexError::Cancelled);
    }

    // Calls resolve against the import sites just computed; never cached.
    let symbols_started = Instant::now();
    let symbols = if !src.symbols {
        cartograph_core::Symbols::default()
    } else {
        let files: Vec<symbols::resolve::FileCode<'_>> = file_code
            .iter()
            .map(|(path, lang, code)| symbols::resolve::FileCode { path, lang, code })
            .collect();
        let mut s = symbols::resolve::resolve(&graph, &files, &rust_call_paths);
        let pages: Vec<(&str, &str)> = configs
            .iter()
            .filter(|(p, _)| p.rsplit('/').next() == Some("index.html"))
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        if !pages.is_empty() {
            let files: HashSet<&str> = graph
                .nodes_of(NodeKind::File)
                .map(|n| n.path.as_str())
                .collect();
            s.entries
                .extend(symbols::resolve::page_entries(&pages, |p| {
                    files.contains(p)
                }));
            s.normalize();
        }
        s
    };
    let symbols_ms = symbols_started.elapsed().as_millis();
    // N3, pass 2: SCIP definitions and calls over the N2 layer.
    let mut symbols = symbols;
    if let Some(p) = precise.as_mut() {
        let started = Instant::now();
        p.merge_symbols(&mut symbols);
        scip_ms += started.elapsed().as_millis();
    }

    let timings = Timings {
        walk_ms: walked.walk_ms,
        read_ms: walked.read_ms,
        build_ms,
        resolve_ms,
        symbols_ms,
        scip_ms,
        write_ms: 0,
        total_ms: started.elapsed().as_millis(),
        cache_hits,
        cache_misses,
    };
    tracing::info!(
        files = graph.nodes_of(NodeKind::File).count(),
        imports = graph.imports.len(),
        unresolved = graph.unresolved.len(),
        symbols = symbols.symbols.len(),
        calls = symbols.calls.len(),
        skipped = graph.skipped.len(),
        cache_hits,
        cache_misses,
        ms = timings.total_ms,
        "indexed"
    );
    Ok((
        IndexOutput {
            graph,
            symbols,
            manifests,
            timings,
        },
        delta,
    ))
}

/// The extraction cache, plus `(hash, lang)` of every entry that failed to deserialize.
type LoadedCache = (walk::ExtractCache, Vec<(String, String)>);

/// Load the extraction cache of the current extractor version. An entry that does not
/// deserialize is logged, left out and returned as `(hash, lang)` to be deleted. If its file
/// still exists it is parsed again and the entry rewritten, so a damaged cache costs time,
/// never correctness.
fn load_cache(store: &Store) -> Result<LoadedCache, IndexError> {
    let mut cache = walk::ExtractCache::new();
    let mut bad = Vec::new();
    for e in store.load_extract_cache(imports::extractor_version())? {
        match serde_json::from_str::<imports::Extract>(&e.imports) {
            Ok(v) => {
                cache.insert(walk::cache_key(&e.lang, &e.hash), v);
            }
            Err(err) => {
                tracing::warn!(hash = %e.hash, lang = %e.lang, error = %err, "bad extraction cache entry");
                bad.push((e.hash, e.lang));
            }
        }
    }
    Ok((cache, bad))
}

/// The extraction cache of the index in `data_dir`, read only: empty when there is no
/// database (none is created) or it cannot be read.
fn read_cache(data_dir: &Path) -> walk::ExtractCache {
    let db = data_dir.join(DB_FILE);
    if !db.is_file() {
        return walk::ExtractCache::new();
    }
    let read =
        || -> Result<walk::ExtractCache, IndexError> { Ok(load_cache(&Store::open(&db)?)?.0) };
    read().unwrap_or_else(|e| {
        tracing::warn!(path = %db.display(), error = %e, "extraction cache not read");
        walk::ExtractCache::new()
    })
}

/// The file graph of the working tree at `root`, built in memory for a comparison (`diff`,
/// `check --base`): the extraction cache of the index in the data dir (if there is one) is
/// read, never written, and nothing else is written either. Calls and entry points are not
/// resolved (`symbols` is empty), as for a base snapshot, so both sides are built alike.
///
/// Also returns that cache with the files parsed in this run added, for the base snapshot
/// ([`git::snapshot::snapshot_at`]): without a stored index, files the base shares with the
/// working tree are then parsed once, not twice.
pub fn index_working_tree(
    root: &Path,
    opts: &IndexOptions,
) -> Result<(IndexOutput, walk::ExtractCache), IndexError> {
    let data_dir = data_dir(root, opts.data_dir.as_deref());
    git::snapshot::clean_legacy(root, &data_dir);
    let mut cache = read_cache(&data_dir);
    let src = Source {
        symbols: false,
        ..Source::of(root)
    };
    // The data dir is left out of the walk, as when indexing to it (when it exists).
    let opts = IndexOptions {
        data_dir: Some(data_dir),
        ..opts.clone()
    };
    let (out, delta) = index_inner(&src, &opts, Some(&cache))?;
    for e in delta.new {
        if let Ok(x) = serde_json::from_str::<imports::Extract>(&e.imports) {
            cache.insert(walk::cache_key(&e.lang, &e.hash), x);
        }
    }
    Ok((out, cache))
}

/// Index `root` and write `<data dir>/graph.db` (see [`IndexOptions::data_dir`]).
/// Re-running replaces the previous graph atomically.
pub fn index_to_store(root: &Path, opts: &IndexOptions) -> Result<IndexOutput, IndexError> {
    let started = Instant::now();
    if !root.is_dir() {
        return Err(IndexError::NotADirectory(root.to_path_buf()));
    }
    // Create the data dir first so that, when it lies inside the repo, the walk can
    // recognise and exclude it.
    let data_dir = data_dir(root, opts.data_dir.as_deref());
    std::fs::create_dir_all(&data_dir).map_err(|source| IndexError::Io {
        path: data_dir.clone(),
        source,
    })?;
    let opts = IndexOptions {
        data_dir: Some(data_dir.clone()),
        ..opts.clone()
    };
    let mut store = Store::open(data_dir.join(DB_FILE))?;
    // Raw imports are reused for unchanged files; resolution always runs over every file,
    // because a new file can resolve an import that was unresolved before.
    let (cache, bad_entries) = load_cache(&store)?;
    // SCIP indexes: decoded once, then read from the store while unchanged.
    let explicit: Vec<PathBuf> = opts.scip.iter().map(|p| scip::absolute(p)).collect();
    let scip_started = Instant::now();
    let remembered = scip::remembered(&store);
    let scip_inputs = scip::inputs(root, &explicit, &remembered, opts.no_scip);
    let loaded: Vec<scip::Loaded> = scip_inputs
        .iter()
        .map(|p| scip::load(root, p, Some(&store)))
        .collect();
    for l in &loaded {
        if let Err(e) = &l.data {
            tracing::warn!(path = %l.label, error = %e, "scip index not imported");
        }
    }
    let src = Source {
        scip: &loaded,
        ..Source::of(root)
    };
    let scip_load_ms = scip_started.elapsed().as_millis();
    let (mut out, delta) = index_inner(&src, &opts, Some(&cache))?;
    out.timings.scip_ms += scip_load_ms;
    if opts.progress.as_ref().is_some_and(|p| p.cancelled()) {
        return Err(IndexError::Cancelled);
    }
    let write_started = Instant::now();
    // Prune before inserting: a bad entry of a file that still exists is rewritten below.
    let stale: Vec<(String, String)> = cache
        .keys()
        .filter(|k| !delta.used.contains(*k))
        .filter_map(|k| {
            k.split_once('|')
                .map(|(lang, hash)| (hash.to_string(), lang.to_string()))
        })
        .chain(bad_entries)
        .collect();
    // Graph, extraction cache and run metadata go in ONE transaction: a reader (or a crash)
    // never sees the new graph with the previous run's metadata. `write_ms` covers writing
    // the graph and the cache; the final commit is not included since timings are part of
    // it.
    let tx = store.begin_replace()?;
    tx.write_graph(&out.graph)?;
    tx.write_symbols(&out.symbols)?;
    tx.prune_extract_cache(imports::extractor_version(), &stale)?;
    tx.put_extract_cache(imports::extractor_version(), &delta.new)?;
    for row in loaded.iter().filter_map(|l| l.cache_row.as_ref()) {
        tx.put_scip_cache(row)?;
    }
    let scip_keys: Vec<String> = loaded
        .iter()
        .filter_map(|l| std::fs::canonicalize(&l.path).ok())
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    tx.retain_scip_cache(&scip_keys)?;
    if opts.no_scip {
        tx.set_meta(scip::META_SCIP_PATHS, "[]")?;
    } else if !explicit.is_empty() {
        tx.set_meta(scip::META_SCIP_PATHS, &serde_json::to_string(&explicit)?)?;
    }
    out.timings.write_ms = write_started.elapsed().as_millis();
    out.timings.total_ms = started.elapsed().as_millis();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    tx.set_meta("repo_name", &repo_name(root))?;
    tx.set_meta("manifests", &serde_json::to_string(&out.manifests)?)?;
    tx.set_meta("timings", &serde_json::to_string(&out.timings)?)?;
    tx.set_meta("indexer_version", env!("CARGO_PKG_VERSION"))?;
    tx.set_meta("indexed_at_unix", &now.to_string())?;
    tx.set_meta(META_MAX_FILE_BYTES, &opts.max_file_bytes.to_string())?;
    tx.commit()?;
    Ok(out)
}

/// Merge `add` into `into`: the keys of two objects together (`add` wins a clash); any other
/// shape replaces.
fn merge_meta(into: &mut serde_json::Value, add: serde_json::Value) {
    match (into, add) {
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => a.extend(b),
        (slot, add) => *slot = add,
    }
}

fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn last_segment(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Id of the node that contains `path`: its parent dir, or the repo for top-level entries.
pub(crate) fn container_id(path: &str) -> String {
    match parent_of(path) {
        "" => node_id(NodeKind::Repo, ""),
        dir => node_id(NodeKind::Dir, dir),
    }
}

#[derive(Default, Clone, Copy)]
struct Totals {
    files: u64,
    loc: u64,
    bytes: u64,
}

fn build_graph(
    repo_name: &str,
    files: Vec<walk::FileRecord>,
    dirs: Vec<String>,
    mut skipped: Vec<SkippedFile>,
) -> (Graph, Vec<Manifest>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let contains =
        |src: String, dst: String| Edge::new(src, dst, EdgeKind::Contains, Confidence::Exact);

    // Recursive totals per directory ("" = repo root).
    let mut totals: BTreeMap<String, Totals> = BTreeMap::new();
    totals.insert(String::new(), Totals::default());
    for d in &dirs {
        totals.insert(d.clone(), Totals::default());
    }
    let mut manifests = Vec::new();
    for f in files {
        let mut dir = parent_of(&f.path);
        loop {
            let t = totals.entry(dir.to_string()).or_default();
            t.files += 1;
            t.loc += f.loc;
            t.bytes += f.bytes;
            if dir.is_empty() {
                break;
            }
            dir = parent_of(dir);
        }
        edges.push(contains(
            container_id(&f.path),
            node_id(NodeKind::File, &f.path),
        ));
        nodes.push(Node {
            id: node_id(NodeKind::File, &f.path),
            kind: NodeKind::File,
            name: last_segment(&f.path).to_string(),
            lang: f.lang.map(str::to_string),
            loc: f.loc,
            bytes: f.bytes,
            hash: Some(f.hash),
            meta: serde_json::Value::Null,
            path: f.path,
        });
        if let Some(m) = f.manifest {
            manifests.push(m);
        }
    }

    for (path, t) in &totals {
        let meta = json!({ "files": t.files });
        if path.is_empty() {
            nodes.push(Node {
                id: node_id(NodeKind::Repo, ""),
                kind: NodeKind::Repo,
                path: String::new(),
                name: repo_name.to_string(),
                lang: None,
                loc: t.loc,
                bytes: t.bytes,
                hash: None,
                meta,
            });
            continue;
        }
        edges.push(contains(container_id(path), node_id(NodeKind::Dir, path)));
        nodes.push(Node {
            id: node_id(NodeKind::Dir, path),
            kind: NodeKind::Dir,
            path: path.clone(),
            name: last_segment(path).to_string(),
            lang: None,
            loc: t.loc,
            bytes: t.bytes,
            hash: None,
            meta,
        });
    }

    manifests.sort_by(|a, b| a.path.cmp(&b.path));
    for m in &manifests {
        if let Some(err) = &m.error {
            tracing::warn!(path = %m.path, error = %err, "manifest parse error");
            skipped.push(SkippedFile {
                path: m.path.clone(),
                reason: SkipReason::ParseError,
                detail: Some(format!("{} manifest: {err}", m.kind.as_str())),
            });
        }
        if !m.kind.declares_package() {
            continue;
        }
        let dir = m.dir();
        let name = m.name.clone().unwrap_or_else(|| {
            if dir.is_empty() {
                repo_name.to_string()
            } else {
                last_segment(dir).to_string()
            }
        });
        let t = totals.get(dir).copied().unwrap_or_default();
        let id = node_id(NodeKind::Package, &m.path);
        edges.push(contains(node_id(NodeKind::Repo, ""), id.clone()));
        edges.push(Edge::new(
            id.clone(),
            node_id(NodeKind::File, &m.path),
            EdgeKind::Manifest,
            Confidence::Exact,
        ));
        nodes.push(Node {
            id,
            kind: NodeKind::Package,
            path: m.path.clone(),
            name,
            lang: None,
            loc: t.loc,
            bytes: t.bytes,
            hash: None,
            meta: json!({
                "manifest": m.kind.as_str(),
                "dir": dir,
                "files": t.files,
                "named_in_manifest": m.name.is_some(),
                "details": m.details,
                "error": m.error,
            }),
        });
    }

    let mut graph = Graph {
        nodes,
        edges,
        skipped,
        ..Graph::default()
    };
    graph.normalize();
    (graph, manifests)
}
