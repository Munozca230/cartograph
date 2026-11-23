//! The git base of `diff` and `check(base)`, shared by the MCP tools and the CLI: resolve
//! the revision, build (or load) its snapshot and make a query [`Repo`] of it with the same
//! config and git history as the working tree, so the two sides differ only in code.

use cartograph_core::History;
use cartograph_index::IndexOptions;
use cartograph_index::git::snapshot::{self, Revision, Snapshot};
use cartograph_index::walk::ExtractCache;
use cartograph_query::{Config, Repo};
use std::path::Path;

/// The error for an empty base revision.
pub const EMPTY_BASE: &str =
    "the base revision is empty: pass a branch, tag or commit, or omit it for the default";

/// The base side of a comparison.
pub struct Base {
    pub repo: Repo,
    pub revision: Revision,
    /// How the snapshot was obtained (timings, cache), for logs.
    pub snapshot: SnapshotInfo,
}

#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    pub cached: bool,
    pub ms: u128,
    pub files: usize,
    pub write_errors: Vec<String>,
    pub cache_hits: u64,
    pub cache_misses: u64,
}

impl From<&Snapshot> for SnapshotInfo {
    fn from(s: &Snapshot) -> SnapshotInfo {
        SnapshotInfo {
            cached: s.cached,
            ms: s.ms,
            files: s.files,
            write_errors: s.write_errors.clone(),
            cache_hits: s.cache_hits,
            cache_misses: s.cache_misses,
        }
    }
}

/// The base `rev` (or, without one, the merge-base of HEAD with the default branch) of the
/// repo at `root`, as a [`Repo`] with `config` and `history`. `opts.data_dir` is the data
/// dir of the working-tree index; its extraction cache is read unless `cache` is given.
/// An empty (or blank) `rev` is an error, never the default. In a workspace, `rev` is a
/// [`snapshot::BaseSpec`] (`main`, `api=feat-x`, `main,api=feat-x`) and each member has
/// its own base.
pub fn open(
    root: &Path,
    rev: Option<&str>,
    opts: &IndexOptions,
    config: &Config,
    history: &History,
    cache: Option<&ExtractCache>,
) -> Result<Base, String> {
    if rev.is_some_and(|r| r.trim().is_empty()) {
        return Err(EMPTY_BASE.to_string());
    }
    let snap = match cartograph_core::Workspace::find(root).map_err(|e| e.to_string())? {
        Some(ws) => {
            let spec = rev
                .map(|r| snapshot::BaseSpec::parse(r, &ws.names()))
                .transpose()
                .map_err(|e| e.to_string())?;
            snapshot::workspace_snapshot(&ws, spec.as_ref(), opts, cache)
                .map_err(|e| e.to_string())?
        }
        None => {
            let revision = match rev.map(str::trim) {
                Some(r) => snapshot::resolve(root, r),
                None => snapshot::default_base(root),
            }
            .map_err(|e| e.to_string())?;
            snapshot::snapshot_at(root, revision, opts, cache).map_err(|e| e.to_string())?
        }
    };
    let info = SnapshotInfo::from(&snap);
    tracing::info!(
        base = %snap.revision.label(),
        cached = info.cached,
        ms = info.ms as u64,
        files = info.files,
        cache_hits = info.cache_hits,
        cache_misses = info.cache_misses,
        "base snapshot ready"
    );
    let repo = Repo::new(&snap.graph, config.clone())
        .with_bridges(&snap.bridges.endpoints, &snap.bridges.bridges)
        .with_history(history);
    Ok(Base {
        repo,
        revision: snap.revision,
        snapshot: info,
    })
}

/// A warning line for files of the base that could not be written, if any.
pub fn write_warning(info: &SnapshotInfo) -> Option<String> {
    if info.write_errors.is_empty() {
        return None;
    }
    let n = info.write_errors.len();
    let shown: Vec<&str> = info
        .write_errors
        .iter()
        .take(3)
        .map(String::as_str)
        .collect();
    Some(format!(
        "warning: {n} file(s) of the base could not be written here and count as added: {}{}",
        shown.join("; "),
        if n > 3 { "; …" } else { "" }
    ))
}
