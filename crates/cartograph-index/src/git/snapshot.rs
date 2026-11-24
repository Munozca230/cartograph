//! Base snapshots: the dependency graph of a git revision, for `diff` and
//! `check --base`, built without touching the working tree or the repository.
//!
//! The tree of the commit (only the part under the indexed root) is written to a temporary
//! folder under `<data dir>/.cartograph-base/`, indexed by the usual pipeline with the
//! extraction cache of the working-tree index (read only: unchanged files are not parsed
//! again), and the folder is deleted. The graph is cached as
//! `<data dir>/.cartograph-base/<commit>.json`, so asking for the same base again only loads
//! it. (The folder was `base/` before: a name a source folder can have when the data dir is
//! the repo root. In any other data dir, what an older version left there is deleted,
//! nothing else; at the repo root nothing is, and a `base/` holding only that is left out of
//! the walk. See [`clean_legacy`] and [`only_legacy_snapshots`].)
//! Nothing is written in the repository: no checkout, no worktree, no ref.
//!
//! A cached snapshot is reused only while what its build read outside the commit is
//! unchanged: the cache key holds the versions, the size limit, the indexed sub-folder and
//! the end-of-line and attributes-file config, and the cached file lists every file the
//! build read or looked for (the files the resolvers read from disk, the `.gitattributes`
//! of every folder, `info/attributes` and the global attributes file) with its size and
//! modification time, checked again on load. Recording what was
//! actually read, rather than fingerprinting a fixed list of inputs, keeps the check right
//! when a resolver starts reading something new.
//!
//! What the base sees, file by file, is what a checkout of it would see:
//! - resolvers that look on disk for files the walk did not index (a `tsconfig` in
//!   `node_modules`) look in the working tree, which holds the same untracked files;
//! - Git LFS pointers become files of the size they point to, so they are skipped as binary
//!   or too large, as the real files are;
//! - symlinks and submodules (gitlinks) are left out, as in the working tree, where the
//!   walker skips symlinks and every folder below the root that holds a `.git` file (the
//!   checkout of a submodule or a linked worktree);
//! - blobs larger than the size limit are written as files of zeros one byte over the
//!   limit (never read; the walker skips them as too large, with their real size);
//! - text goes through the worktree conversion of the repo's config and `.gitattributes`
//!   (`core.autocrlf`, `eol`, `ident`, encodings; external drivers are never run), so a
//!   file unchanged since the base has the bytes, and hash, of the working tree's copy and
//!   comes from the extraction cache.
//!
//! A workspace ([`workspace_snapshot`]) has one base per member, each in the member's own
//! repository: the same revision for all (each resolves it, the default being each one's
//! own merge-base with its default branch), or one per member (`api=feat-x`). A member
//! without git or without that revision takes its working tree, and the label says so.
//! The trees go under `<data dir>/.cartograph-base/t<pid>/<member>/`; the cache key holds
//! every member's commit.

use crate::{IndexError, IndexOptions, Source, walk};
use cartograph_core::Graph;
use gix::bstr::ByteSlice;
use gix::prelude::ObjectIdExt;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Folder of the data dir that holds base snapshots. Its name cannot be a source folder's
/// by accident, since the data dir may be the repo root.
pub const SNAPSHOT_DIR: &str = ".cartograph-base";
/// Where snapshots were kept up to snapshot version 3 (see [`only_legacy_snapshots`]).
pub const LEGACY_SNAPSHOT_DIR: &str = "base";
/// Snapshots kept; the least recently used ones (by modification time, which a cache hit
/// refreshes) are deleted.
pub const KEEP: usize = 8;
/// Bump when a cached snapshot no longer matches what [`snapshot`] would build.
const SNAPSHOT_VERSION: &str = "5";
const LFS_POINTER: &[u8] = b"version https://git-lfs.github.com/spec/";

/// A revision resolved to a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Revision {
    /// What was asked for (`main`, `HEAD~3`, a hash), or how it was chosen.
    pub rev: String,
    /// Full commit id.
    pub commit: String,
    /// First line of the commit message.
    pub subject: String,
    /// `rev` is (a prefix of) the commit id, not a ref name that happens to look like one.
    #[serde(skip)]
    pub by_id: bool,
    /// In a workspace, the base of each member (`rev` and `commit` then stand for all).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<MemberBase>,
}

/// The base of one workspace member: a revision of its repository, or its working tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemberBase {
    pub member: String,
    /// `None`: the member's working tree is its base (see `note`).
    pub revision: Option<Revision>,
    /// Why the working tree is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl MemberBase {
    fn label(&self) -> String {
        match (&self.revision, &self.note) {
            (Some(r), _) => format!("{}: {}", self.member, r.label()),
            (None, Some(n)) => format!("{}: working tree ({n})", self.member),
            (None, None) => format!("{}: working tree", self.member),
        }
    }
}

impl Revision {
    /// `main (1a2b3c4d)`; a commit id alone is shown short.
    pub fn label(&self) -> String {
        if !self.members.is_empty() {
            let all: Vec<String> = self.members.iter().map(MemberBase::label).collect();
            return all.join(", ");
        }
        let short = &self.commit[..self.commit.len().min(8)];
        if self.by_id {
            short.to_string()
        } else {
            format!("{} ({short})", self.rev)
        }
    }
}

/// The graph of a revision.
#[derive(Debug)]
pub struct Snapshot {
    pub revision: Revision,
    pub graph: Graph,
    /// Its bridges (see [`cartograph_core::bridge`]), so `diff` and `check` compare them.
    pub bridges: BridgeRows,
    /// Loaded from the cache instead of built.
    pub cached: bool,
    /// Time to build or load it.
    pub ms: u128,
    /// Files written to the temporary tree (0 when cached).
    pub files: usize,
    /// Paths that could not be written (an invalid name on this system); they are missing
    /// from the base, so they show up as added files.
    pub write_errors: Vec<String>,
    pub cache_hits: u64,
    pub cache_misses: u64,
}

/// The bridge rows of a snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeRows {
    #[serde(default)]
    pub endpoints: Vec<cartograph_core::BridgeEndpoint>,
    #[serde(default)]
    pub bridges: Vec<cartograph_core::Bridge>,
    /// The bodies of its HTTP bridges and its MCP tools (see [`cartograph_core::contract`]).
    #[serde(default)]
    pub bodies: Vec<cartograph_core::HttpBody>,
    #[serde(default)]
    pub tools: Vec<cartograph_core::McpTool>,
}

impl BridgeRows {
    fn of(s: &cartograph_core::Symbols) -> BridgeRows {
        BridgeRows {
            endpoints: s.endpoints.clone(),
            bridges: s.bridges.clone(),
            bodies: s.bodies.clone(),
            tools: s.tools.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Cached {
    key: String,
    revision_subject: String,
    /// Working-tree files the build read or looked for (see the module docs).
    #[serde(default)]
    inputs: Vec<Input>,
    graph: Graph,
    #[serde(default)]
    bridges: BridgeRows,
}

/// A working-tree file a snapshot's build read (or looked for) and what it was then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Input {
    path: PathBuf,
    /// Size and modification time (ns since the epoch); `None` = no such file.
    stamp: Option<(u64, u64)>,
}

fn stamp(path: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    let t = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64);
    Some((m.len(), t))
}

/// The inputs at `paths`, made absolute: the CLI and the MCP server share the cache and may
/// run from any working dir.
fn inputs(paths: impl IntoIterator<Item = PathBuf>) -> Vec<Input> {
    let paths: std::collections::BTreeSet<PathBuf> = paths
        .into_iter()
        .map(|p| std::path::absolute(&p).unwrap_or(p))
        .collect();
    paths
        .into_iter()
        .map(|path| Input {
            stamp: stamp(&path),
            path,
        })
        .collect()
}

/// The first input that changed since it was recorded, if any.
fn changed(inputs: &[Input]) -> Option<&Path> {
    inputs
        .iter()
        .find(|i| stamp(&i.path) != i.stamp)
        .map(|i| i.path.as_path())
}

fn git_err(what: &str, e: impl std::fmt::Display) -> IndexError {
    IndexError::Git(format!("{what}: {e}"))
}

fn open(root: &Path) -> Result<super::scan::Opened, IndexError> {
    match super::scan::open(root).map_err(|e| git_err("cannot open the git repository", e))? {
        super::scan::Found::NoGit => Err(IndexError::Git(format!(
            "{} is not inside a git repository; diff and check --base need one",
            root.display()
        ))),
        super::scan::Found::Repo(o) => Ok(*o),
    }
}

/// Resolve `rev` (a commit, branch, tag, `HEAD~3`…) in the repository holding `root`.
pub fn resolve(root: &Path, rev: &str) -> Result<Revision, IndexError> {
    let o = open(root)?;
    resolve_in(&o, rev)
}

fn resolve_in(o: &super::scan::Opened, rev: &str) -> Result<Revision, IndexError> {
    let repo = o.repo.to_thread_local();
    let rev = rev.trim();
    let id = repo
        .rev_parse_single(rev)
        .map_err(|e| git_err(&format!("unknown revision {rev:?}"), e))?;
    let commit = id
        .object()
        .map_err(|e| git_err(&format!("cannot read {rev:?}"), e))?
        .peel_to_commit()
        .map_err(|e| git_err(&format!("{rev:?} is not a commit"), e))?;
    let subject = commit
        .message_raw()
        .map(|m| {
            m.lines()
                .next()
                .map(|l| l.to_str_lossy().into_owned())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let commit = commit.id.to_string();
    // A ref wins over an id prefix (as in git): `cafe` may be a branch.
    let is_ref = repo.try_find_reference(rev).ok().flatten().is_some();
    Ok(Revision {
        rev: rev.to_string(),
        by_id: !is_ref && !rev.is_empty() && commit.starts_with(&rev.to_ascii_lowercase()),
        commit,
        subject,
        members: Vec::new(),
    })
}

/// The default base: the merge-base of HEAD with the default branch (`origin/HEAD`, else
/// `main`, `master`, `origin/main`, `origin/master`). On the default branch itself that is
/// HEAD, so the diff shows the uncommitted changes.
pub fn default_base(root: &Path) -> Result<Revision, IndexError> {
    let o = open(root)?;
    let repo = o.repo.to_thread_local();
    let head = o
        .head
        .ok_or_else(|| IndexError::Git("the repository has no commits yet".into()))?;
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(Some(r)) = repo.try_find_reference("refs/remotes/origin/HEAD")
        && let Some(target) = r.target().try_name()
    {
        candidates.push(target.shorten().to_str_lossy().into_owned());
    }
    for c in ["main", "master", "origin/main", "origin/master"] {
        candidates.push(c.to_string());
    }
    for name in candidates {
        let Ok(id) = repo.rev_parse_single(name.as_str()) else {
            continue;
        };
        let Ok(base) = repo.merge_base(id.detach(), head) else {
            continue;
        };
        let Some(base) = base else { continue };
        let mut r = resolve_in(&o, &base.detach().to_string())?;
        r.rev = format!("merge-base with {name}");
        r.by_id = false;
        return Ok(r);
    }
    Err(IndexError::Git(
        "no default branch found (origin/HEAD, main, master); pass a base revision".into(),
    ))
}

/// The cache key: what the snapshot depends on besides the commit and the files of
/// [`Cached::inputs`].
fn cache_key(o: &super::scan::Opened, root: &Path, opts: &IndexOptions) -> String {
    let repo = o.repo.to_thread_local();
    let config = repo.config_snapshot();
    let eol: Vec<String> = ["core.autocrlf", "core.eol", "core.attributesFile"]
        .iter()
        .map(|k| {
            config
                .string(*k)
                .map(|v| v.to_str_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect();
    format!(
        "{SNAPSHOT_VERSION}|{}|{}|{}|{}|{}|{:?}",
        env!("CARGO_PKG_VERSION"),
        crate::imports::extractor_version(),
        opts.max_file_bytes,
        o.prefix,
        eol.join(","),
        bridges_of_repo(root)
    )
}

/// The graph of `rev` for the repo at `root` (see the module docs). `opts.data_dir` is the
/// data dir of the working-tree index; its extraction cache is read, never written.
pub fn snapshot(root: &Path, rev: &str, opts: &IndexOptions) -> Result<Snapshot, IndexError> {
    let o = open(root)?;
    let revision = resolve_in(&o, rev)?;
    snapshot_of(root, &o, revision, opts, None)
}

/// [`snapshot`] of a revision already resolved (by [`resolve`] or [`default_base`]), with
/// `cache` as the extraction cache when given (that of [`crate::index_working_tree`]),
/// else the one stored in the data dir.
pub fn snapshot_at(
    root: &Path,
    revision: Revision,
    opts: &IndexOptions,
    cache: Option<&walk::ExtractCache>,
) -> Result<Snapshot, IndexError> {
    let o = open(root)?;
    snapshot_of(root, &o, revision, opts, cache)
}

fn snapshot_of(
    root: &Path,
    o: &super::scan::Opened,
    revision: Revision,
    opts: &IndexOptions,
    cache: Option<&walk::ExtractCache>,
) -> Result<Snapshot, IndexError> {
    let started = Instant::now();
    let data_dir = crate::data_dir(root, opts.data_dir.as_deref());
    clean_legacy(root, &data_dir);
    let dir = data_dir.join(SNAPSHOT_DIR);
    std::fs::create_dir_all(&dir).map_err(|source| IndexError::Io {
        path: dir.clone(),
        source,
    })?;
    let key = cache_key(o, root, opts);
    let file = dir.join(format!("{}.json", revision.commit));
    if let Some((graph, bridges)) = load_cached(&file, &key) {
        touch(&file);
        tracing::info!(commit = %revision.commit, "base snapshot from cache");
        return Ok(Snapshot {
            revision,
            graph,
            bridges,
            cached: true,
            ms: started.elapsed().as_millis(),
            files: 0,
            write_errors: Vec::new(),
            cache_hits: 0,
            cache_misses: 0,
        });
    }

    // A short, unique temporary root (Windows paths are limited); its last component is
    // never used as the repo name.
    let tmp = dir.join(format!("t{}", unique()));
    let result = (|| {
        let written = materialize(o, &revision.commit, &tmp, opts.max_file_bytes)?;
        let stored;
        let cache = match cache {
            Some(c) => c,
            None => {
                stored = crate::read_cache(&data_dir);
                &stored
            }
        };
        let src = Source {
            mounts: vec![crate::SourceMount {
                prefix: "",
                walk_root: &tmp,
                resolve_root: root,
                meta: &serde_json::Value::Null,
            }],
            name: crate::repo_name(root),
            symbols: false,
            scip: &[],
            bridges: bridges_of_repo(root),
        };
        let walk_opts = IndexOptions {
            data_dir: None,
            ..opts.clone()
        };
        let (mut out, delta) = crate::index_inner(&src, &walk_opts, Some(cache))?;
        honest_sizes(&mut out.graph, &written.capped, opts.max_file_bytes);
        let read = written
            .attributes
            .into_iter()
            .chain(delta.disk_reads.iter().map(|p| root.join(p)));
        Ok::<_, IndexError>((out, written.files, written.errors, inputs(read)))
    })();
    if let Err(e) = std::fs::remove_dir_all(&tmp)
        && tmp.exists()
    {
        tracing::warn!(path = %tmp.display(), error = %e, "temporary base tree not removed");
    }
    let (out, files, write_errors, inputs) = result?;
    store_cached(
        &dir,
        &file,
        &key,
        &revision,
        &inputs,
        &out.graph,
        &out.symbols,
    );
    prune(&dir, KEEP);
    tracing::info!(
        commit = %revision.commit,
        files,
        cache_hits = out.timings.cache_hits,
        cache_misses = out.timings.cache_misses,
        ms = started.elapsed().as_millis() as u64,
        "base snapshot built"
    );
    Ok(Snapshot {
        revision,
        bridges: BridgeRows::of(&out.symbols),
        graph: out.graph,
        cached: false,
        ms: started.elapsed().as_millis(),
        files,
        write_errors,
        cache_hits: out.timings.cache_hits,
        cache_misses: out.timings.cache_misses,
    })
}

/// A workspace base as written: one revision for every member and per-member overrides,
/// `main`, `api=feat-x`, `main,api=feat-x` (see [`workspace_snapshot`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BaseSpec {
    pub all: Option<String>,
    pub members: Vec<(String, String)>,
}

impl BaseSpec {
    /// Parse `spec` for the members `names`: items separated by commas, `member=rev` for one
    /// member, a bare revision for every other one (at most one).
    pub fn parse(spec: &str, names: &[&str]) -> Result<BaseSpec, IndexError> {
        let mut out = BaseSpec::default();
        for item in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            match item.split_once('=') {
                Some((m, rev)) if names.contains(&m.trim()) => {
                    let (m, rev) = (m.trim(), rev.trim());
                    if rev.is_empty() {
                        return Err(IndexError::Git(format!("base {item:?}: no revision")));
                    }
                    if out.members.iter().any(|(x, _)| x == m) {
                        return Err(IndexError::Git(format!("base: member {m} given twice")));
                    }
                    out.members.push((m.to_string(), rev.to_string()));
                }
                Some((m, _)) => {
                    return Err(IndexError::Git(format!(
                        "base {item:?}: {m:?} is not a member (members: {})",
                        names.join(", ")
                    )));
                }
                None if out.all.is_some() => {
                    return Err(IndexError::Git(format!(
                        "base {spec:?}: more than one revision for every member"
                    )));
                }
                None => out.all = Some(item.to_string()),
            }
        }
        if out.all.is_none() && out.members.is_empty() {
            return Err(IndexError::Git("the base revision is empty".into()));
        }
        Ok(out)
    }

    fn of(&self, member: &str) -> Option<(&str, bool)> {
        self.members
            .iter()
            .find(|(m, _)| m == member)
            .map(|(_, r)| (r.as_str(), true))
            .or_else(|| self.all.as_deref().map(|r| (r, false)))
    }
}

/// The base of each member of `ws` (see the module docs). `spec` `None` = each member's
/// default base. A revision named for that member alone must resolve; one given for every
/// member (or the default) may be missing from some (they take their working tree), but
/// not from every member asked for it: that is an error, as for a single repo (a typo in
/// the revision must not compare every member with itself).
pub fn workspace_bases(
    ws: &cartograph_core::Workspace,
    spec: Option<&BaseSpec>,
) -> Result<Vec<MemberBase>, IndexError> {
    let mut out = Vec::new();
    // Members asked for the shared revision (or the default), and the first failure.
    let (mut shared_asked, mut shared_found) = (0, 0);
    let mut shared_error: Option<String> = None;
    for m in &ws.members {
        let base = |revision, note: Option<String>| MemberBase {
            member: m.name.clone(),
            revision,
            note,
        };
        let opened = match super::scan::open(&m.root) {
            Ok(super::scan::Found::Repo(o)) => *o,
            Ok(super::scan::Found::NoGit) => {
                out.push(base(None, Some("no git repository".into())));
                continue;
            }
            Err(e) => {
                out.push(base(None, Some(format!("git: {e}"))));
                continue;
            }
        };
        let wanted = spec.and_then(|s| s.of(&m.name));
        let resolved = match wanted {
            Some((rev, _)) => resolve_in(&opened, rev),
            None => default_base(&m.root),
        };
        let shared = !matches!(wanted, Some((_, true)));
        if shared {
            shared_asked += 1;
        }
        match (resolved, wanted) {
            (Ok(r), _) => {
                shared_found += usize::from(shared);
                out.push(base(Some(r), None));
            }
            (Err(e), Some((rev, true))) => {
                return Err(IndexError::Git(format!("member {}: {rev}: {e}", m.name)));
            }
            (Err(e), _) => {
                shared_error.get_or_insert_with(|| format!("member {}: {e}", m.name));
                out.push(base(None, Some(e.to_string())));
            }
        }
    }
    if shared_asked > 0 && shared_found == 0 {
        let what = match spec.and_then(|s| s.all.as_deref()) {
            Some(rev) => format!("revision {rev:?}"),
            None => "a default base".to_string(),
        };
        return Err(IndexError::Git(format!(
            "no member's repository has {what} ({})",
            shared_error.unwrap_or_default()
        )));
    }
    if !out.iter().any(|b| b.revision.is_some()) {
        return Err(IndexError::Git(
            "no member is a git repository; diff and check --base need one".into(),
        ));
    }
    Ok(out)
}

/// The graph of a workspace's base: each member's base ([`workspace_bases`]) under the
/// member's name, built as one graph (imports across members resolve as in the working
/// tree). Cached like [`snapshot`] when every member has a revision.
pub fn workspace_snapshot(
    ws: &cartograph_core::Workspace,
    spec: Option<&BaseSpec>,
    opts: &IndexOptions,
    cache: Option<&walk::ExtractCache>,
) -> Result<Snapshot, IndexError> {
    let started = Instant::now();
    let root = &ws.dir;
    let bases = workspace_bases(ws, spec)?;
    let data_dir = crate::data_dir(root, opts.data_dir.as_deref());
    let dir = data_dir.join(SNAPSHOT_DIR);
    std::fs::create_dir_all(&dir).map_err(|source| IndexError::Io {
        path: dir.clone(),
        source,
    })?;
    let parts: Vec<String> = bases
        .iter()
        .map(|b| match &b.revision {
            Some(r) => format!("{}={}", b.member, r.commit),
            None => format!("{}=worktree", b.member),
        })
        .collect();
    let key = format!(
        "{SNAPSHOT_VERSION}|workspace|{}|{}|{}|{}|{:?}",
        env!("CARGO_PKG_VERSION"),
        crate::imports::extractor_version(),
        opts.max_file_bytes,
        parts.join(","),
        ws.bridges.resolve(true)
    );
    let id = blake3::hash(key.as_bytes()).to_hex().to_string();
    let revision = Revision {
        rev: "workspace".to_string(),
        commit: id.clone(),
        subject: String::new(),
        by_id: false,
        members: bases.clone(),
    };
    // A working tree changes under the snapshot: only all-revision bases are cached.
    let cacheable = bases.iter().all(|b| b.revision.is_some());
    let file = dir.join(format!("{id}.json"));
    if cacheable && let Some((graph, bridges)) = load_cached(&file, &key) {
        touch(&file);
        return Ok(Snapshot {
            revision,
            graph,
            bridges,
            cached: true,
            ms: started.elapsed().as_millis(),
            files: 0,
            write_errors: Vec::new(),
            cache_hits: 0,
            cache_misses: 0,
        });
    }
    let tmp = dir.join(format!("t{}", unique()));
    let result = (|| {
        let mut files = 0;
        let mut errors = Vec::new();
        let mut capped = Vec::new();
        let mut attributes = Vec::new();
        let mut trees: Vec<(PathBuf, &cartograph_core::Member)> = Vec::new();
        for (b, m) in bases.iter().zip(&ws.members) {
            let Some(r) = &b.revision else {
                trees.push((m.root.clone(), m));
                continue;
            };
            let o = open(&m.root)?;
            let dest = tmp.join(&m.name);
            let w = materialize(&o, &r.commit, &dest, opts.max_file_bytes)?;
            files += w.files;
            errors.extend(w.errors.into_iter().map(|e| format!("{}/{e}", m.name)));
            capped.extend(
                w.capped
                    .into_iter()
                    .map(|(p, n)| (format!("{}/{p}", m.name), n)),
            );
            attributes.extend(w.attributes);
            trees.push((dest, m));
        }
        let metas: Vec<serde_json::Value> = ws
            .members
            .iter()
            .map(|m| serde_json::json!({ crate::MEMBER_META: { "name": m.name, "path": m.path } }))
            .collect();
        let src = Source {
            mounts: trees
                .iter()
                .zip(&metas)
                .map(|((walk_root, m), meta)| crate::SourceMount {
                    prefix: &m.name,
                    walk_root,
                    resolve_root: &m.root,
                    meta,
                })
                .collect(),
            name: ws.name.clone(),
            symbols: false,
            scip: &[],
            bridges: ws.bridges.resolve(true),
        };
        let stored;
        let cache = match cache {
            Some(c) => c,
            None => {
                stored = crate::read_cache(&data_dir);
                &stored
            }
        };
        let (mut out, delta) = crate::index_inner(&src, opts, Some(cache))?;
        honest_sizes(&mut out.graph, &capped, opts.max_file_bytes);
        let read = attributes
            .into_iter()
            .chain(delta.disk_reads.iter().filter_map(|p| ws.to_disk(p)));
        Ok::<_, IndexError>((out, files, errors, inputs(read)))
    })();
    if let Err(e) = std::fs::remove_dir_all(&tmp)
        && tmp.exists()
    {
        tracing::warn!(path = %tmp.display(), error = %e, "temporary base tree not removed");
    }
    let (out, files, write_errors, inputs) = result?;
    if cacheable {
        store_cached(
            &dir,
            &file,
            &key,
            &revision,
            &inputs,
            &out.graph,
            &out.symbols,
        );
        prune(&dir, KEEP);
    }
    Ok(Snapshot {
        revision,
        bridges: BridgeRows::of(&out.symbols),
        graph: out.graph,
        cached: false,
        ms: started.elapsed().as_millis(),
        files,
        write_errors,
        cache_hits: out.timings.cache_hits,
        cache_misses: out.timings.cache_misses,
    })
}

/// Delete what an older version left in `<data_dir>/base` (the former [`SNAPSHOT_DIR`]),
/// never anything else ([`legacy_artifact`]), then the folder if that left it empty. Only
/// when the data dir is not the repo root (the default `.cartograph`, where that folder can
/// only be ours): at the repo root it may be a source folder, and nothing in it is deleted
/// (see [`only_legacy_snapshots`]). Also run before the working tree of a comparison is
/// walked ([`crate::index_working_tree`]).
pub(crate) fn clean_legacy(root: &Path, data_dir: &Path) {
    match (std::fs::canonicalize(root), std::fs::canonicalize(data_dir)) {
        (Ok(r), Ok(d)) if r != d => {}
        _ => return,
    }
    let dir = &data_dir.join(LEGACY_SNAPSHOT_DIR);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let _ = match legacy_artifact(&e) {
            Some(true) => std::fs::remove_dir_all(e.path()),
            Some(false) => std::fs::remove_file(e.path()),
            None => Ok(()),
        };
    }
    // Fails, as it should, unless the folder is empty now.
    let _ = std::fs::remove_dir(dir);
}

/// Whether `dir`, the former snapshot folder of a data dir that is the repo root, holds
/// something and nothing but what an older version left there ([`legacy_artifact`]). Such
/// a folder is left out of the walk and the watcher, never deleted; with anything else in
/// it, it is a source folder and indexed with everything in it.
pub fn only_legacy_snapshots(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut any = false;
    for e in entries {
        match e {
            Ok(e) if legacy_artifact(&e).is_some() => any = true,
            _ => return false,
        }
    }
    any
}

/// What an older version left in its snapshot folder: a `<commit>.json` that reads as a
/// cached snapshot (a `key` and a `graph`), a `<commit>.<pid>-<n>.tmp` file (a snapshot
/// being written) or a `t<pid>-<n>` folder (a temporary base tree a crash left), `<commit>`
/// a full SHA-1 or SHA-256 in hex. `Some(true)` for a folder, `None` for anything else.
fn legacy_artifact(e: &std::fs::DirEntry) -> Option<bool> {
    #[derive(Deserialize)]
    struct OldSnapshot {
        #[allow(dead_code)]
        key: String,
        #[allow(dead_code)]
        graph: serde::de::IgnoredAny,
    }
    let commit = |c: &str| {
        matches!(c.len(), 40 | 64) && c.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    };
    let counter = |t: &str| {
        t.split_once('-').is_some_and(|(a, b)| {
            !a.is_empty()
                && !b.is_empty()
                && a.bytes().all(|x| x.is_ascii_digit())
                && b.bytes().all(|x| x.is_ascii_digit())
        })
    };
    let name = e.file_name();
    let name = name.to_str()?;
    let t = e.file_type().ok()?;
    if t.is_dir() {
        return name.strip_prefix('t').is_some_and(counter).then_some(true);
    }
    if !t.is_file() {
        return None;
    }
    let ok = match name.strip_suffix(".json") {
        Some(c) => {
            commit(c)
                && std::fs::read(e.path())
                    .is_ok_and(|b| serde_json::from_slice::<OldSnapshot>(&b).is_ok())
        }
        None => name
            .strip_suffix(".tmp")
            .and_then(|rest| rest.split_once('.'))
            .is_some_and(|(c, t)| commit(c) && counter(t)),
    };
    ok.then_some(false)
}

/// A name part no other run (in this process or another) uses at the same time.
fn unique() -> String {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{n}", std::process::id())
}

fn load_cached(file: &Path, key: &str) -> Option<(Graph, BridgeRows)> {
    let bytes = std::fs::read(file).ok()?;
    match serde_json::from_slice::<Cached>(&bytes) {
        Ok(c) if c.key == key => match changed(&c.inputs) {
            None => Some((c.graph, c.bridges)),
            Some(p) => {
                tracing::info!(path = %p.display(), "base snapshot input changed, rebuilding");
                None
            }
        },
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(path = %file.display(), error = %e, "bad base snapshot, rebuilding");
            None
        }
    }
}

/// Mark a snapshot as just used: [`prune`] keeps the most recently *used* ones.
fn touch(file: &Path) {
    let res = std::fs::File::options()
        .write(true)
        .open(file)
        .and_then(|f| f.set_modified(std::time::SystemTime::now()));
    if let Err(e) = res {
        tracing::debug!(path = %file.display(), error = %e, "base snapshot time not updated");
    }
}

/// Write the snapshot through a temporary file and a rename, so a reader never sees half of
/// it. Failing to cache is only a warning (the next run builds it again).
fn store_cached(
    dir: &Path,
    file: &Path,
    key: &str,
    revision: &Revision,
    inputs: &[Input],
    graph: &Graph,
    symbols: &cartograph_core::Symbols,
) {
    let tmp = dir.join(format!("{}.{}.tmp", revision.commit, unique()));
    let outcome = (|| -> Result<(), String> {
        let f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut w = std::io::BufWriter::new(f);
        serde_json::to_writer(
            &mut w,
            &Cached {
                key: key.to_string(),
                revision_subject: revision.subject.clone(),
                inputs: inputs.to_vec(),
                graph: graph.clone(),
                bridges: BridgeRows::of(symbols),
            },
        )
        .map_err(|e| e.to_string())?;
        use std::io::Write;
        w.flush().map_err(|e| e.to_string())?;
        drop(w);
        std::fs::rename(&tmp, file).map_err(|e| e.to_string())
    })();
    if let Err(e) = outcome {
        let _ = std::fs::remove_file(&tmp);
        tracing::warn!(path = %file.display(), error = %e, "base snapshot not cached");
    }
}

/// Keep the `keep` most recently used snapshots (built or loaded, see [`touch`]); also remove temporary trees and files left by a
/// run that died (older than an hour).
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let mut snaps: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let Ok(meta) = e.metadata() else { continue };
        let modified = meta.modified().unwrap_or(now);
        let name = e.file_name().to_string_lossy().into_owned();
        if name.ends_with(".json") {
            snaps.push((modified, path));
        } else if now
            .duration_since(modified)
            .is_ok_and(|age| age.as_secs() > 3600)
        {
            let _ = if meta.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
        }
    }
    snaps.sort_by_key(|s| std::cmp::Reverse(s.0));
    for (_, path) in snaps.into_iter().skip(keep) {
        let _ = std::fs::remove_file(path);
    }
}

/// One blob to write.
struct Blob {
    /// Path in the repository (for attributes).
    full: String,
    /// Path under the indexed root.
    rel: String,
    oid: gix::ObjectId,
}

/// What a worker thread needs to write blobs as a checkout would: its repository handle
/// and the worktree conversion (end of lines, `ident`, encodings) of the repository's
/// config and `.gitattributes`, without external filter drivers (no LFS smudge, no
/// process is ever started). Without a pipeline (it could not be set up) blobs are written
/// as stored.
struct Writer {
    repo: gix::Repository,
    filter: Option<(gix::filter::plumbing::Pipeline, gix::worktree::Stack)>,
}

impl Writer {
    fn new(ts: &gix::ThreadSafeRepository) -> Writer {
        let repo = ts.to_thread_local();
        let filter = match repo.filter_pipeline(None) {
            Ok((p, _index)) => {
                let (mut inner, stack) = p.into_parts();
                inner.options_mut().drivers.clear();
                Some((inner, stack))
            }
            Err(e) => {
                tracing::warn!(error = %e, "base snapshot: no worktree conversion");
                None
            }
        };
        Writer { repo, filter }
    }

    /// `data` as the working tree would hold it (unchanged when no filter applies).
    fn convert<'a>(&mut self, full: &str, data: &'a [u8]) -> std::borrow::Cow<'a, [u8]> {
        use gix::filter::plumbing::pipeline::convert::{ToWorktreeOutcome, to_worktree};
        let Some((inner, stack)) = &mut self.filter else {
            return data.into();
        };
        let path: &gix::bstr::BStr = full.as_bytes().as_bstr();
        let Ok(entry) = stack.at_entry(path, None, &self.repo.objects) else {
            return data.into();
        };
        let out = inner.convert_to_worktree(
            data,
            path,
            &mut |_, attrs| {
                entry.matching_attributes(attrs);
            },
            to_worktree::Options::default(),
        );
        match out {
            Ok(ToWorktreeOutcome::Buffer(b)) => b.to_vec().into(),
            // `Process` comes only from external drivers, and there are none.
            Ok(_) | Err(_) => data.into(),
        }
    }
}

/// What [`materialize`] wrote.
struct Written {
    files: usize,
    /// Paths that could not be written.
    errors: Vec<String>,
    /// Files written shorter than they are (over the size limit), with their real size.
    capped: Vec<(String, u64)>,
    /// The working tree's `.gitattributes` files the conversion reads (or would, if they
    /// existed): one per folder of the written files, and `info/attributes`.
    attributes: Vec<PathBuf>,
}

/// Write the blobs of `commit` under the indexed root into `dest`.
fn materialize(
    o: &super::scan::Opened,
    commit: &str,
    dest: &Path,
    max_file_bytes: u64,
) -> Result<Written, IndexError> {
    let repo = o.repo.to_thread_local();
    let id = gix::ObjectId::from_hex(commit.as_bytes()).map_err(|e| git_err("bad commit id", e))?;
    let tree = repo
        .find_commit(id)
        .map_err(|e| git_err("cannot read the commit", e))?
        .tree()
        .map_err(|e| git_err("cannot read the commit's tree", e))?;
    let mut rec = gix::traverse::tree::Recorder::default();
    tree.traverse()
        .breadthfirst(&mut rec)
        .map_err(|e| git_err("cannot read the tree", e))?;
    let blobs: Vec<Blob> = rec
        .records
        .into_iter()
        // Regular and executable files; symlinks and submodules are left out.
        .filter(|e| e.mode.is_blob())
        .filter_map(|e| {
            let full = e.filepath.to_str().ok()?;
            let rel = full.strip_prefix(o.prefix.as_str())?;
            Some(Blob {
                full: full.to_string(),
                rel: rel.to_string(),
                oid: e.oid,
            })
        })
        .collect();
    std::fs::create_dir_all(dest).map_err(|source| IndexError::Io {
        path: dest.to_path_buf(),
        source,
    })?;
    tracing::debug!(blobs = blobs.len(), "base snapshot: tree read");
    let ts = &o.repo;
    // One writer per chunk: setting one up reads the index and the attributes, so it must
    // not happen per file (`map_init` would do it per rayon split, hundreds of times).
    let chunk = blobs
        .len()
        .div_ceil(rayon::current_num_threads().max(1) * 2)
        .max(16);
    let results: Vec<(&Blob, Result<Option<u64>, String>)> = blobs
        .par_chunks(chunk)
        .flat_map_iter(|part| {
            let mut w = Writer::new(ts);
            part.iter()
                .map(|b| (b, write_blob(&mut w, b, dest, max_file_bytes)))
                .collect::<Vec<_>>()
        })
        .collect();
    tracing::debug!("base snapshot: files written");
    let mut errors = Vec::new();
    let mut capped = Vec::new();
    for (b, r) in results {
        match r {
            Ok(Some(size)) => capped.push((b.rel.clone(), size)),
            Ok(None) => {}
            Err(e) => errors.push(e),
        }
    }
    for e in errors.iter().take(5) {
        tracing::warn!(error = %e, "base snapshot: file not written");
    }
    let mut attributes = vec![repo.common_dir().join("info").join("attributes")];
    attributes.extend(global_attributes(&repo));
    if let Some(work) = repo.workdir() {
        let mut dirs: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        dirs.insert("");
        for b in &blobs {
            let mut d = b.full.as_str();
            while let Some((parent, _)) = d.rsplit_once('/') {
                if !dirs.insert(parent) {
                    break;
                }
                d = parent;
            }
        }
        attributes.extend(
            dirs.into_iter()
                .map(|d| work.join(d).join(".gitattributes")),
        );
    }
    Ok(Written {
        files: blobs.len() - errors.len(),
        errors,
        capped,
        attributes,
    })
}

/// The global attributes file the worktree conversion reads, as gix finds it:
/// `core.attributesFile`, else `$XDG_CONFIG_HOME/git/attributes`, else
/// `~/.config/git/attributes`.
fn global_attributes(repo: &gix::Repository) -> Option<PathBuf> {
    if let Ok(Some(p)) = repo.config_snapshot().trusted_path("core.attributesFile") {
        return Some(p);
    }
    let home = || {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|h| PathBuf::from(h).join(".config"))
    };
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(home)?;
    Some(base.join("git").join("attributes"))
}

/// A repo-relative path that stays inside the destination (and, on Windows, names no
/// alternate data stream).
fn safe(rel: &str) -> bool {
    safe_on(rel, cfg!(windows))
}

/// [`safe`] for a system: `\` and `:` are ordinary file name characters on Unix, but a
/// separator and a drive or stream marker on Windows.
fn safe_on(rel: &str, windows: bool) -> bool {
    !rel.is_empty()
        && rel.split('/').all(|c| {
            !c.is_empty() && c != "." && c != ".." && !(windows && c.contains(['\\', ':']))
        })
}

/// Write one blob as a checkout would. `Some(size)` when the file stands for a larger one
/// (over the size limit) and was written at `max + 1` bytes.
fn write_blob(w: &mut Writer, b: &Blob, dest: &Path, max: u64) -> Result<Option<u64>, String> {
    let fail = |e: &dyn std::fmt::Display| format!("{}: {e}", b.rel);
    if !safe(&b.rel) {
        return Err(fail(&"not a valid file name on this system"));
    }
    let path = dest.join(&b.rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| fail(&e))?;
    }
    let header = w.repo.find_header(b.oid).map_err(|e| fail(&e))?;
    let size = header.size();
    if size > max {
        // Never read: the walker skips it as too large from its size alone.
        return sized(&path, size, max).map_err(|e| fail(&e));
    }
    let obj = b
        .oid
        .attach(&w.repo)
        .object()
        .map_err(|e| fail(&e))?
        .detach();
    if let Some(real) = lfs_size(&obj.data) {
        // The working tree holds the real file: a binary of that size.
        return sized(&path, real.max(1), max).map_err(|e| fail(&e));
    }
    let data = w.convert(&b.full, &obj.data);
    std::fs::write(&path, &data).map_err(|e| fail(&e))?;
    Ok(None)
}

/// A file of zeros standing for one of `size` bytes: binary to the walker, and never longer
/// than `max + 1`, which the walker already skips as too large (`set_len` really reserves
/// the space on some file systems, so the full size of a huge blob is never written).
/// `Some(size)` when it was cut.
fn sized(path: &Path, size: u64, max: u64) -> std::io::Result<Option<u64>> {
    let f = std::fs::File::create(path)?;
    let len = size.min(max.saturating_add(1));
    f.set_len(len)?;
    Ok((len < size).then_some(size))
}

/// The walker saw the cut files at `max + 1` bytes: put their real size back in the detail
/// of their `too_large` record.
fn honest_sizes(graph: &mut Graph, capped: &[(String, u64)], max: u64) {
    if capped.is_empty() {
        return;
    }
    let real: std::collections::HashMap<&str, u64> =
        capped.iter().map(|(p, n)| (p.as_str(), *n)).collect();
    for s in &mut graph.skipped {
        if s.reason == cartograph_core::SkipReason::TooLarge
            && let Some(n) = real.get(s.path.as_str())
        {
            s.detail = Some(format!("{n} bytes > limit {max}"));
        }
    }
}

/// The size a Git LFS pointer stands for, or `None` when `data` is not a pointer.
fn lfs_size(data: &[u8]) -> Option<u64> {
    if data.len() > 1024 || !data.starts_with(LFS_POINTER) {
        return None;
    }
    let text = std::str::from_utf8(data).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("size "))
        .and_then(|s| s.trim().parse().ok())
}

/// The bridges a single repo's base builds: `[bridges]` of the working tree's
/// `cartograph.toml`, as for its index.
fn bridges_of_repo(root: &Path) -> cartograph_core::Bridges {
    cartograph_core::BridgeSettings::of_repo(root).resolve(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_specs() {
        let names = ["api", "lib"];
        let p = |s: &str| BaseSpec::parse(s, &names);
        assert_eq!(
            p("main").unwrap(),
            BaseSpec {
                all: Some("main".into()),
                members: vec![]
            }
        );
        assert_eq!(
            p(" main , api=feat/x ").unwrap(),
            BaseSpec {
                all: Some("main".into()),
                members: vec![("api".into(), "feat/x".into())]
            }
        );
        assert_eq!(p("api=a,lib=b").unwrap().of("lib"), Some(("b", true)));
        assert_eq!(p("main,api=a").unwrap().of("lib"), Some(("main", false)));
        assert!(p("web=x").unwrap_err().to_string().contains("not a member"));
        assert!(p("main,dev").is_err());
        assert!(p("api=").is_err());
        assert!(p(" , ").is_err());
    }

    #[test]
    fn lfs_pointers_are_recognised() {
        let p = b"version https://git-lfs.github.com/spec/v1\noid sha256:abc\nsize 12345\n";
        assert_eq!(lfs_size(p), Some(12345));
        assert_eq!(lfs_size(b"version 1\nsize 3\n"), None);
    }

    /// A blob over the size limit, and an LFS pointer to a huge file, are written at
    /// `max + 1` bytes (the walker skips them from that size), never at their full size.
    #[test]
    fn large_blobs_are_written_capped() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = gix::init(tmp.path().join("repo")).unwrap();
        let big = repo.write_blob(vec![b'x'; 5000]).unwrap().detach();
        let pointer = repo
            .write_blob(b"version https://git-lfs.github.com/spec/v1\noid sha256:ab\nsize 50000\n")
            .unwrap()
            .detach();
        let small = repo.write_blob(b"export const a = 1;\n").unwrap().detach();
        let mut w = Writer::new(&repo.into_sync());
        let dest = tmp.path().join("out");
        let blob = |name: &str, oid| Blob {
            full: name.to_string(),
            rel: name.to_string(),
            oid,
        };
        let len = |name: &str| std::fs::metadata(dest.join(name)).unwrap().len();
        assert_eq!(
            write_blob(&mut w, &blob("big.txt", big), &dest, 100),
            Ok(Some(5000))
        );
        assert_eq!(len("big.txt"), 101);
        assert_eq!(
            write_blob(&mut w, &blob("lfs.bin", pointer), &dest, 100),
            Ok(Some(50_000))
        );
        assert_eq!(len("lfs.bin"), 101);
        assert_eq!(
            write_blob(&mut w, &blob("a.ts", small), &dest, 100),
            Ok(None)
        );
        // Written as is (or with CRLF, by the end-of-line config of this machine).
        assert!((20..=21).contains(&len("a.ts")));
    }

    /// A cached snapshot as an older version wrote it.
    const OLD: &str = r#"{"key":"k","revision_subject":"s","graph":{}}"#;
    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER: &str = "89abcdef0123456789abcdef0123456789abcdef";

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// What an older version leaves in its snapshot folder, plus files of other kinds.
    fn fill_legacy(dir: &Path) {
        for d in ["sub", "t12-3/src", "tmp"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        for (f, text) in [
            (format!("{SHA}.json"), OLD),
            (format!("{SHA}.1234-0.tmp"), "{"),
            (format!("{OTHER}.json"), "{}"),
            ("deadbeef.json".to_string(), OLD),
            ("x.ts".to_string(), ""),
            ("sub/keep.ts".to_string(), ""),
            ("t12-3/src/a.ts".to_string(), ""),
        ] {
            std::fs::write(dir.join(f), text).unwrap();
        }
    }

    /// In a data dir other than the repo root, the former snapshot folder loses what an
    /// older version left there (snapshots of that shape, their temporary files, temporary
    /// trees) and nothing else, and goes once empty. In a data dir that is the repo root,
    /// that folder may be a source folder: nothing in it is deleted.
    #[test]
    fn only_legacy_snapshots_are_cleaned() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let data = root.join(".cartograph");
        let dir = data.join(LEGACY_SNAPSHOT_DIR);
        fill_legacy(&dir);
        clean_legacy(root, &data);
        assert_eq!(
            names(&dir),
            [
                format!("{OTHER}.json").as_str(),
                "deadbeef.json",
                "sub",
                "tmp",
                "x.ts"
            ]
        );
        assert!(dir.join("sub/keep.ts").is_file());

        let at_root = root.join(LEGACY_SNAPSHOT_DIR);
        fill_legacy(&at_root);
        let before = names(&at_root);
        clean_legacy(root, root);
        assert_eq!(names(&at_root), before);
        assert!(at_root.join(format!("{SHA}.json")).is_file());
        assert!(at_root.join("t12-3/src/a.ts").is_file());

        // Only what an older version left: the folder goes too.
        let only = root.join("only");
        let legacy = only.join(LEGACY_SNAPSHOT_DIR);
        std::fs::create_dir_all(legacy.join("t1-2")).unwrap();
        std::fs::write(legacy.join(format!("{SHA}.json")), OLD).unwrap();
        clean_legacy(root, &only);
        assert!(!legacy.exists());
    }

    /// A former snapshot folder holding only what an older version left there is
    /// recognized; one that is empty, missing or holds anything else is not.
    #[test]
    fn a_folder_of_legacy_snapshots_only_is_recognized() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(LEGACY_SNAPSHOT_DIR);
        assert!(!only_legacy_snapshots(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!only_legacy_snapshots(&dir));
        std::fs::create_dir_all(dir.join("t7-0/src")).unwrap();
        std::fs::write(dir.join("t7-0/src/a.ts"), "").unwrap();
        std::fs::write(dir.join(format!("{SHA}.json")), OLD).unwrap();
        std::fs::write(dir.join(format!("{SHA}.99-1.tmp")), "{").unwrap();
        assert!(only_legacy_snapshots(&dir));
        std::fs::write(dir.join(format!("{OTHER}.json")), "{}").unwrap();
        assert!(!only_legacy_snapshots(&dir));
        std::fs::remove_file(dir.join(format!("{OTHER}.json"))).unwrap();
        std::fs::write(dir.join("x.ts"), "").unwrap();
        assert!(!only_legacy_snapshots(&dir));
    }

    #[test]
    fn unsafe_paths_are_refused() {
        for windows in [false, true] {
            assert!(safe_on("src/a.ts", windows));
            assert!(!safe_on("../a", windows));
            assert!(!safe_on("a/./b", windows));
            assert!(!safe_on("a//b", windows));
            assert!(!safe_on("", windows));
        }
        // Ordinary names on Unix; a drive, a stream or a separator on Windows.
        for p in ["c:/x", "a/b:c.ts", "a\\b.ts"] {
            assert!(safe_on(p, false), "{p}");
            assert!(!safe_on(p, true), "{p}");
        }
        assert_eq!(safe("a/b:c.ts"), !cfg!(windows));
    }
}
