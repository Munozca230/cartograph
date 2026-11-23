//! N3: import SCIP indexes for compiler-precise definitions, calls and dependencies.
//!
//! cartograph never runs an indexer: the user (or CI) runs scip-typescript, rust-analyzer
//! `scip`, scip-java, scip-python, scip-clang or scip-dotnet, and cartograph reads the
//! resulting `index.scip` (auto-detected at the repo root, or given with `--scip`). See
//! `docs/scip.md`.
//!
//! - [`read`] decodes a SCIP file into a compact [`ScipData`], streaming one document at a
//!   time; [`load`] caches that form in the store, keyed by the file's size, mtime and hash.
//! - [`apply`] merges it into the graph and the symbol layer: for every file a document
//!   covers (and that did not change since), SCIP is authoritative for calls to the
//!   symbols it defines; definitions
//!   N2 missed are added; references across files add the file dependencies no import
//!   statement shows; references to packages map to the graph's libraries.

pub mod apply;
pub mod read;

use cartograph_core::{ScipCacheRow, Store, SymbolKind};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// The SCIP file picked up at the repo root without a flag.
pub const AUTO_FILE: &str = "index.scip";
/// Run-metadata key of the SCIP files given with `--scip` (JSON array of absolute paths),
/// reused by later re-indexes without the flag (`cartograph index`, `serve`, `mcp`).
pub const META_SCIP_PATHS: &str = "scip_paths";
/// Version of the cached form ([`ScipData`]); a cache row of another version is decoded
/// again from the file.
const CACHE_VERSION: &str = "scip2";
/// Largest decoded form cached: one SQLite value stays well under its 1 GB limit, so a
/// huge index never fails the write of the whole index (it is decoded every time instead).
const MAX_CACHE_BYTES: usize = 256 << 20;

/// The decoded form of one SCIP file: what N3 needs and nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScipData {
    /// `name version` of the tool that wrote it.
    #[serde(rename = "t", default)]
    pub tool: String,
    /// `Metadata.project_root` (a `file://` URI).
    #[serde(rename = "r", default)]
    pub project_root: String,
    /// Global symbol strings, interned (`ScipDef::sym` and references index this).
    #[serde(rename = "s", default)]
    pub symbols: Vec<String>,
    #[serde(rename = "d", default)]
    pub docs: Vec<ScipDoc>,
    /// Documents read (including those with an unusable path).
    #[serde(rename = "n", default)]
    pub documents: u64,
    /// Occurrences read (locals included).
    #[serde(rename = "o", default)]
    pub occurrences: u64,
    /// Occurrences left out because their range is malformed.
    #[serde(rename = "b", default)]
    pub bad_ranges: u64,
    /// Documents left out because their path is absolute or climbs out of the root.
    #[serde(rename = "p", default)]
    pub bad_paths: u64,
}

impl ScipData {
    /// Every symbol index points into `symbols` (a damaged cache row may not).
    pub fn valid(&self) -> bool {
        let n = self.symbols.len() as u64;
        let ok = |s: u32| u64::from(s) < n;
        self.docs.iter().all(|d| {
            d.defs.iter().all(|x| ok(x.sym))
                && d.refs.iter().all(|[s, _, _]| ok(*s))
                && d.imports.iter().all(|[s, _]| ok(*s))
                && d.declared.iter().all(|&s| ok(s))
        })
    }
}

/// One document: definitions and references of global symbols, lines 0-based.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScipDoc {
    /// `relative_path`, cleaned (forward slashes, no `.` or `..`).
    #[serde(rename = "p")]
    pub path: String,
    #[serde(rename = "d", default, skip_serializing_if = "Vec::is_empty")]
    pub defs: Vec<ScipDef>,
    /// `[symbol, line, end]` of every reference that is not an import; `end` is the
    /// character where its range ends, in [`ScipDoc::encoding`] units ([`NO_END`] when the
    /// range spans lines).
    #[serde(rename = "r", default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<[u32; 3]>,
    /// `[symbol, line]` of references with the Import role (`import { f } from`).
    #[serde(rename = "i", default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<[u32; 2]>,
    /// Symbols the document describes (`SymbolInformation`) without a definition
    /// occurrence.
    #[serde(rename = "s", default, skip_serializing_if = "Vec::is_empty")]
    pub declared: Vec<u32>,
    /// `Document.position_encoding`: what a character offset counts (0 unspecified, 1 UTF-8
    /// bytes, 2 UTF-16 code units, 3 UTF-32 code units).
    #[serde(rename = "e", default, skip_serializing_if = "is_zero")]
    pub encoding: u8,
}

/// A reference's `end` when its range spans lines.
pub const NO_END: u32 = u32::MAX;

fn is_zero(n: &u8) -> bool {
    *n == 0
}

/// A definition occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScipDef {
    #[serde(rename = "s")]
    pub sym: u32,
    /// Line of the name.
    #[serde(rename = "l")]
    pub line: u32,
    /// Last line of the enclosing range (the whole definition) when given, else `line`.
    #[serde(rename = "e")]
    pub end: u32,
    /// Last descriptor's name (`save` of `…/Store#save().`).
    #[serde(rename = "n", default)]
    pub name: String,
    /// The type it belongs to, for a member.
    #[serde(rename = "c", default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// What N2 would call it; `None` for what N2 does not model (fields, variables…).
    #[serde(rename = "k", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<SymbolKind>,
}

/// One SCIP file, ready to apply.
#[derive(Debug, Clone)]
pub struct Loaded {
    /// The file.
    pub path: PathBuf,
    /// How to name it: repo-relative when inside the repo, else as given.
    pub label: String,
    /// Its modification time: a covered file modified later has stale SCIP lines.
    pub mtime: Option<SystemTime>,
    pub data: Result<Arc<ScipData>, String>,
    /// Decoded from the cache, not from the file.
    pub cached: bool,
    /// A cache row to write (new or refreshed); `None` when the stored one is current.
    pub cache_row: Option<ScipCacheRow>,
    /// Prefix of the mount whose files it describes (`""`: the repo root); its documents
    /// are matched to the files of that mount only.
    pub prefix: String,
}

/// Cache key of a SCIP file: its canonical path.
fn cache_key(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn nanos(t: SystemTime) -> i64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// How to name `path` in messages: repo-relative when inside `root`.
pub fn label(root: &Path, path: &Path) -> String {
    let rel = match (std::fs::canonicalize(root), std::fs::canonicalize(path)) {
        (Ok(r), Ok(p)) => crate::walk::relative(&r, &p),
        _ => None,
    };
    rel.unwrap_or_else(|| path.display().to_string())
}

/// Read the SCIP file at `path`, from `store`'s cache when the file did not change: same
/// size and mtime, or else the same content hash. Never fails: a problem is in `data`.
pub fn load(root: &Path, path: &Path, store: Option<&Store>) -> Loaded {
    let label = label(root, path);
    let meta = std::fs::metadata(path);
    let mtime = meta.as_ref().ok().and_then(|m| m.modified().ok());
    let mut out = Loaded {
        path: path.to_path_buf(),
        label,
        mtime,
        data: Err(String::new()),
        cached: false,
        cache_row: None,
        prefix: String::new(),
    };
    let meta = match meta {
        Ok(m) if m.is_file() => m,
        Ok(_) => {
            out.data = Err("not a file".to_string());
            return out;
        }
        Err(e) => {
            out.data = Err(format!("cannot read: {e}"));
            return out;
        }
    };
    let key = cache_key(path);
    let size = i64::try_from(meta.len()).unwrap_or(i64::MAX);
    let mtime_ns = mtime.map_or(0, nanos);
    let stored = store.and_then(|s| match s.load_scip_cache(&key) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(path = %out.label, error = %e, "scip cache not read");
            None
        }
    });
    if let Some(row) = stored.filter(|r| r.hash.starts_with(CACHE_VERSION)) {
        let same_stat = row.size == size && row.mtime == mtime_ns;
        let same_hash = same_stat
            || read::hash_file(path).is_ok_and(|h| row.hash == format!("{CACHE_VERSION}:{h}"));
        if same_hash {
            match serde_json::from_str::<ScipData>(&row.data)
                .map_err(|e| e.to_string())
                .and_then(|d| {
                    if d.valid() {
                        Ok(d)
                    } else {
                        Err("symbol index out of range".to_string())
                    }
                }) {
                Ok(d) => {
                    out.data = Ok(Arc::new(d));
                    out.cached = true;
                    if !same_stat {
                        out.cache_row = Some(ScipCacheRow {
                            size,
                            mtime: mtime_ns,
                            ..row
                        });
                    }
                    return out;
                }
                Err(e) => {
                    tracing::warn!(path = %out.label, error = %e, "bad scip cache row; decoding the file again");
                }
            }
        }
    }
    match read::read(path) {
        Ok((data, hash)) => {
            match serde_json::to_string(&data) {
                Ok(json) if json.len() > MAX_CACHE_BYTES => {
                    tracing::warn!(path = %out.label, bytes = json.len(), "scip data too large to cache; it is decoded on every index");
                }
                Ok(json) => {
                    out.cache_row = Some(ScipCacheRow {
                        path: key,
                        size,
                        mtime: mtime_ns,
                        hash: format!("{CACHE_VERSION}:{hash}"),
                        data: json,
                    });
                }
                Err(e) => tracing::warn!(path = %out.label, error = %e, "scip data not cached"),
            }
            out.data = Ok(Arc::new(data));
        }
        Err(e) => out.data = Err(e),
    }
    out
}

/// The SCIP files a run reads: `explicit` (`--scip`, absolute), else those remembered from
/// an earlier `--scip` (`remembered`, kept while they exist), plus `<root>/index.scip` when
/// it exists. `disabled` (`--no-scip`) reads none.
pub fn inputs(
    root: &Path,
    explicit: &[PathBuf],
    remembered: &[PathBuf],
    disabled: bool,
) -> Vec<PathBuf> {
    if disabled {
        return Vec::new();
    }
    let mut out: Vec<PathBuf> = if explicit.is_empty() {
        remembered.iter().filter(|p| p.is_file()).cloned().collect()
    } else {
        explicit.to_vec()
    };
    let auto = root.join(AUTO_FILE);
    if auto.is_file() {
        out.push(auto);
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(cache_key(p)));
    out
}

/// Absolute form of a `--scip` path (relative to the current directory).
pub fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The `--scip` paths remembered in a store (see [`META_SCIP_PATHS`]).
pub fn remembered(store: &Store) -> Vec<PathBuf> {
    store
        .get_meta(META_SCIP_PATHS)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str::<Vec<PathBuf>>(&v).ok())
        .unwrap_or_default()
}

/// What `doctor` reports about one SCIP file.
#[derive(Debug, Clone)]
pub struct Health {
    pub label: String,
    pub outcome: Result<HealthOk, String>,
}

#[derive(Debug, Clone)]
pub struct HealthOk {
    pub tool: String,
    pub documents: u64,
    /// Documents whose file exists in the repo.
    pub matched: u64,
    /// Matched files modified after the SCIP file, newest first (at most a few).
    pub stale: Vec<String>,
    pub stale_total: u64,
}

/// The SCIP files an index of `root` would read (auto-detected and remembered ones), each
/// checked: readable, how many documents match files, and which covered files are newer
/// than it (stale). Reads the decoded form from the store's cache when current, else the
/// file (streamed); writes nothing.
pub fn health(root: &Path, data_dir: &Path) -> Vec<Health> {
    let db = data_dir.join(crate::DB_FILE);
    let store = db.is_file().then(|| Store::open(&db).ok()).flatten();
    let remembered = store.as_ref().map(remembered).unwrap_or_default();
    inputs(root, &[], &remembered, false)
        .into_iter()
        .map(|p| {
            let l = load(root, &p, store.as_ref());
            let outcome = l.data.as_ref().map_err(Clone::clone).map(|d| {
                let exists = |rel: &str| root.join(rel).is_file();
                let map = apply::infer_mapping(d, root, &l.path, &exists);
                let mut matched = 0u64;
                let mut stale: Vec<(SystemTime, String)> = Vec::new();
                for doc in &d.docs {
                    let Some(rel) = map.apply(&doc.path) else {
                        continue;
                    };
                    let Ok(m) = std::fs::metadata(root.join(&rel)) else {
                        continue;
                    };
                    if !m.is_file() {
                        continue;
                    }
                    matched += 1;
                    if let (Some(scip), Ok(t)) = (l.mtime, m.modified())
                        && t > scip
                    {
                        stale.push((t, rel));
                    }
                }
                stale.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
                HealthOk {
                    tool: d.tool.clone(),
                    documents: d.documents,
                    matched,
                    stale_total: stale.len() as u64,
                    stale: stale.into_iter().take(3).map(|(_, p)| p).collect(),
                }
            });
            Health {
                label: l.label,
                outcome,
            }
        })
        .collect()
}
