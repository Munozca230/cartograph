//! File watcher: notify events over the repo root, batched until 300 ms pass without a new
//! one. Reads are not changes: on Linux inotify also reports files being opened, and the
//! indexer opens every file, so access events are dropped (else indexing re-triggers
//! itself). Events for paths the
//! walker would never index (the data dir, `.git`, the always-excluded dirs, gitignored
//! paths) are dropped here, so writing the database never retriggers indexing; the rest
//! become one `Watch` request on the scheduler.
//!
//! A commit that changes no working-tree file (files edited and indexed before, then
//! committed) only touches git's own files. The few that mean new history ([`GitWatch`]:
//! `HEAD`, its reflog, branch refs, `packed-refs`, `shallow`) become a `Git` request: the
//! history is read again, no file is. The git dirs are watched too when they are outside
//! the root (a linked worktree, a root below the work tree's top).
//!
//! A workspace root (`cartograph-workspace.toml`) is watched member by member: each
//! member's folder with its own ignore rules and git dirs, its paths reported under the
//! member's name; the workspace file itself (non-recursively) re-indexes everything.

use crate::scheduler::Reason;
use crate::state::AppState;
use cartograph_index::git::snapshot::{LEGACY_SNAPSHOT_DIR, only_legacy_snapshots};
use cartograph_index::walk::EXCLUDED_DIRS;
use cartograph_index::{DATA_DIRS, DATA_FILES, NOTES_FILES};
use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::event::{AccessKind, AccessMode};
use notify::{EventKind, RecursiveMode, Watcher as _};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Debounce window: a batch is handed over once this long passes without a new event.
pub const DEBOUNCE: Duration = Duration::from_millis(300);
/// Longest a batch waits under a steady stream of events.
const MAX_BATCH_WAIT: Duration = Duration::from_secs(2);

/// Whether an event can change a file: anything but a read (open, read, close after read).
/// A close after writing is kept: some editors only report that.
fn is_change(kind: &EventKind) -> bool {
    match kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        _ => true,
    }
}

/// Decides which changed paths matter. Mirrors the walker (`ignore::WalkBuilder` with
/// `hidden(false)`, `parents(false)`, `require_git(false)`, no global gitignore):
/// - always-excluded dir names and the data dir;
/// - per directory, `.ignore` beats `.gitignore`, which beats the git exclude file
///   (`.git/info/exclude`, or the common dir's for a worktree). Within each kind the
///   deepest directory with an opinion wins.
/// - With `require_git(false)` a nested `.git` does *not* stop the parents' rules (the
///   walker keeps applying them), so neither does this filter.
/// - The walker never descends into an ignored directory, so a path is ignored when any
///   of its ancestors is.
pub struct WatchFilter {
    root: PathBuf,
    /// Repo-relative path of the data dir when it is strictly inside the root.
    data_rel: Option<String>,
    /// The data dir is the root itself.
    data_is_root: bool,
    /// Ignore rules by directory (absolute), loaded lazily.
    rules: Mutex<HashMap<PathBuf, Arc<DirRules>>>,
}

/// The ignore files of one directory, by kind (highest precedence first).
struct DirRules {
    ignore: Gitignore,
    gitignore: Gitignore,
    exclude: Gitignore,
}

fn load_rules(dir: &Path, files: &[PathBuf]) -> Gitignore {
    let mut b = GitignoreBuilder::new(dir);
    for f in files {
        if !f.is_file() {
            continue;
        }
        if let Some(err) = b.add(f) {
            tracing::debug!(file = %f.display(), error = %err, "ignore file problem");
        }
    }
    b.build().unwrap_or_else(|e| {
        tracing::warn!(dir = %dir.display(), error = %e, "cannot build ignore rules");
        Gitignore::empty()
    })
}

/// The git dir whose `info/exclude` applies to `dir`, as the `ignore` crate resolves it:
/// `.git` itself, or for a worktree (`.git` file) the common dir named by its git dir.
fn git_common_dir(dir: &Path) -> Option<PathBuf> {
    let dot_git = dir.join(".git");
    let meta = std::fs::metadata(&dot_git).ok()?;
    if !meta.is_file() {
        return Some(dot_git);
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let real = PathBuf::from(text.lines().next()?.strip_prefix("gitdir: ")?);
    let common = std::fs::read_to_string(real.join("commondir")).ok()?;
    let common = common.lines().next()?;
    Some(if common.starts_with('.') {
        real.join(common)
    } else {
        PathBuf::from(common)
    })
}

impl WatchFilter {
    /// `root` and `data_dir` must be normalized absolute paths (see `crate::normalize`).
    pub fn new(root: &Path, data_dir: &Path) -> WatchFilter {
        let data_rel = cartograph_index::walk::relative(root, data_dir);
        WatchFilter {
            root: root.to_path_buf(),
            data_is_root: data_rel.as_deref() == Some(""),
            data_rel: data_rel.filter(|r| !r.is_empty()),
            rules: Mutex::new(HashMap::new()),
        }
    }

    /// Forget loaded ignore files (after one of them changed).
    pub fn reload(&self) {
        self.rules.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    fn rules_for(&self, dir: &Path) -> Arc<DirRules> {
        let mut cache = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = cache.get(dir) {
            return r.clone();
        }
        let exclude = git_common_dir(dir)
            .map(|g| load_rules(dir, &[g.join("info").join("exclude")]))
            .unwrap_or_else(Gitignore::empty);
        let r = Arc::new(DirRules {
            ignore: load_rules(dir, &[dir.join(".ignore")]),
            gitignore: load_rules(dir, &[dir.join(".gitignore")]),
            exclude,
        });
        cache.insert(dir.to_path_buf(), r.clone());
        r
    }

    /// The repo-relative path of `abs` when a change there can change the index, else
    /// `None`.
    pub fn relevant(&self, abs: &Path) -> Option<String> {
        let rel = cartograph_index::walk::relative(&self.root, abs)?;
        if rel.is_empty() {
            return None;
        }
        let parts: Vec<&str> = rel.split('/').collect();
        let last = parts.len() - 1;
        for (i, p) in parts.iter().enumerate() {
            // A path *inside* an excluded dir, or the excluded dir itself.
            if EXCLUDED_DIRS.contains(p) && (i < last || !abs.is_file()) {
                return None;
            }
        }
        if let Some(d) = &self.data_rel
            && (rel == *d || rel.starts_with(&format!("{d}/")))
        {
            return None;
        }
        if parts.len() == 1 && NOTES_FILES.contains(&rel.as_str()) {
            return None;
        }
        // What the walk leaves out by name alone (worktree copies, output, minified bundles).
        if cartograph_index::noise::watch_ignores(&self.root, &rel) {
            return None;
        }
        if self.data_is_root
            && ((parts.len() == 1 && DATA_FILES.contains(&rel.as_str()))
                || DATA_DIRS.contains(&parts[0]))
        {
            return None;
        }
        // The snapshot folder of older versions, left out of the walk while it holds nothing
        // else. A file gone from it (it may have been the last source file) still counts.
        if self.data_is_root
            && parts.len() > 1
            && parts[0] == LEGACY_SNAPSHOT_DIR
            && std::fs::symlink_metadata(self.root.join(parts[0]).join(parts[1])).is_ok()
            && only_legacy_snapshots(&self.root.join(LEGACY_SNAPSHOT_DIR))
        {
            return None;
        }
        // The SCIP index at the root is read even when ignored (it usually is): a new one
        // must re-index.
        if rel == cartograph_index::scip::AUTO_FILE {
            return Some(rel);
        }
        // Every ancestor dir (as the walker would have pruned it), then the path itself.
        let mut dirs = vec![self.root.clone()];
        for (i, p) in parts.iter().enumerate() {
            let path = dirs[i].join(p);
            let is_dir = i < last || abs.is_dir();
            if self.ignored(&dirs, &path, is_dir) {
                return None;
            }
            dirs.push(path);
        }
        Some(rel)
    }

    /// The walker's decision for `path`, whose ancestor dirs (root first) are `dirs`.
    fn ignored(&self, dirs: &[PathBuf], path: &Path, is_dir: bool) -> bool {
        let (mut m_ignore, mut m_gi, mut m_ex) = (Match::None, Match::None, Match::None);
        for dir in dirs.iter().rev() {
            let r = self.rules_for(dir);
            if m_ignore.is_none() {
                m_ignore = r.ignore.matched(path, is_dir).map(|_| ());
            }
            if m_gi.is_none() {
                m_gi = r.gitignore.matched(path, is_dir).map(|_| ());
            }
            if m_ex.is_none() {
                m_ex = r.exclude.matched(path, is_dir).map(|_| ());
            }
        }
        m_ignore.or(m_gi).or(m_ex).is_ignore()
    }
}

/// The git files whose change can mean new history: `HEAD` and its reflog `logs/HEAD` in
/// the git dir (a linked worktree has its own), and in the common dir the branch refs
/// (`refs/heads/**`), `packed-refs` and `shallow` (a fetch that deepens a clone). Never
/// `index`, objects or lock files: staging and object writes are not history.
#[derive(Debug, Clone)]
pub struct GitWatch {
    git_dir: PathBuf,
    common_dir: PathBuf,
}

impl GitWatch {
    /// The git dirs of the repository holding `root` (a `.git` dir or file in `root` or a
    /// parent, as the git layer finds it); `None` outside a repository.
    pub fn find(root: &Path) -> Option<GitWatch> {
        let dir = root.ancestors().find(|d| d.join(".git").exists())?;
        let dot = dir.join(".git");
        let git_dir = if dot.is_dir() {
            dot
        } else {
            let text = std::fs::read_to_string(&dot).ok()?;
            let p = PathBuf::from(text.lines().next()?.strip_prefix("gitdir:")?.trim());
            if p.is_relative() { dir.join(p) } else { p }
        };
        let git_dir = dunce::canonicalize(git_dir).ok()?;
        let common_dir = match std::fs::read_to_string(git_dir.join("commondir")) {
            Ok(c) => {
                let p = PathBuf::from(c.lines().next()?.trim());
                dunce::canonicalize(if p.is_relative() { git_dir.join(p) } else { p }).ok()?
            }
            Err(_) => git_dir.clone(),
        };
        Some(GitWatch {
            git_dir,
            common_dir,
        })
    }

    /// Whether a change of `abs` can change the history.
    pub fn matches(&self, abs: &Path) -> bool {
        let rel = cartograph_index::walk::relative;
        if let Some(r) = rel(&self.git_dir, abs)
            && (r == "HEAD" || r == "logs/HEAD")
        {
            return true;
        }
        rel(&self.common_dir, abs).is_some_and(|r| {
            r == "packed-refs"
                || r == "shallow"
                || (r.starts_with("refs/heads/") && !r.ends_with(".lock"))
        })
    }

    /// What to watch besides the recursive watch of `root`: the dirs of those files that
    /// are outside it.
    pub fn extra_watches(&self, root: &Path) -> Vec<(PathBuf, RecursiveMode)> {
        let mut out = Vec::new();
        if !self.git_dir.starts_with(root) {
            out.push((self.git_dir.clone(), RecursiveMode::NonRecursive));
            out.push((self.git_dir.join("logs"), RecursiveMode::NonRecursive));
        }
        if !self.common_dir.starts_with(root) {
            if self.common_dir != self.git_dir {
                out.push((self.common_dir.clone(), RecursiveMode::NonRecursive));
            }
            out.push((
                self.common_dir.join("refs").join("heads"),
                RecursiveMode::Recursive,
            ));
        }
        out.retain(|(p, _)| p.is_dir());
        out
    }
}

/// Keeps the watcher alive; dropping it stops watching.
pub struct Watcher {
    // Dropping the watcher closes the channel, which ends the batching thread.
    _watcher: notify::RecommendedWatcher,
}

fn touches_ignore_rules(p: &Path) -> bool {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name == ".gitignore" || name == ".ignore" || name == "exclude"
}

/// Start watching `state.root`. On failure the caller keeps serving without a watcher.
pub fn start(state: Arc<AppState>) -> Result<Watcher, String> {
    let st = state.clone();
    let st_git = state.clone();
    start_with(
        &state.root,
        &state.data_dir,
        move |changed| st.scheduler.request(Reason::Watch, changed),
        move || st_git.scheduler.request(Reason::Git, std::iter::empty()),
    )
}

/// One watched tree: a single repo (prefix `""`) or a workspace member.
struct Tree {
    prefix: String,
    root: PathBuf,
    filter: WatchFilter,
    git: Option<GitWatch>,
}

impl Tree {
    fn new(prefix: &str, root: &Path, data_dir: &Path) -> Tree {
        Tree {
            prefix: prefix.to_string(),
            root: root.to_path_buf(),
            filter: WatchFilter::new(root, data_dir),
            git: GitWatch::find(root),
        }
    }

    /// The path of the index of `abs` when a change there can change the index.
    fn relevant(&self, abs: &Path) -> Option<String> {
        let rel = self.filter.relevant(abs)?;
        Some(if self.prefix.is_empty() {
            rel
        } else {
            format!("{}/{rel}", self.prefix)
        })
    }
}

/// Watch `root` (`root` and `data_dir` normalized, see `crate::normalize`) and call
/// `on_change` with the repo-relative paths that can change the index. An empty set means
/// "unknown" (the watcher lost events): re-index everything. `on_git` is called when git's
/// history may have changed ([`GitWatch`]), with the same debounce. A workspace root is
/// watched member by member (see the module docs). Used by `serve` and by the MCP server.
pub fn start_with(
    root: &Path,
    data_dir: &Path,
    on_change: impl Fn(BTreeSet<String>) + Send + 'static,
    on_git: impl Fn() + Send + 'static,
) -> Result<Watcher, String> {
    let workspace = cartograph_core::Workspace::find(root).map_err(|e| e.to_string())?;
    let trees: Vec<Tree> = match &workspace {
        None => vec![Tree::new("", root, data_dir)],
        Some(ws) => ws
            .members
            .iter()
            .filter_map(|m| {
                let r = crate::normalize(&m.root).ok()?;
                Some(Tree::new(&m.name, &r, data_dir))
            })
            .collect(),
    };
    let ws_file = workspace
        .as_ref()
        .map(|_| root.join(cartograph_core::WORKSPACE_FILE));
    let mut extra: Vec<(PathBuf, RecursiveMode)> = Vec::new();
    for t in &trees {
        if let Some(g) = &t.git {
            extra.extend(g.extra_watches(&t.root));
        }
    }
    let roots: Vec<PathBuf> = trees.iter().map(|t| t.root.clone()).collect();
    let handle = move |res: Result<Vec<PathBuf>, String>| match res {
        Ok(paths) => {
            if ws_file.as_ref().is_some_and(|f| paths.contains(f)) {
                tracing::debug!("workspace file changed; scheduling a full re-index");
                on_change(BTreeSet::new());
                return;
            }
            if paths.iter().any(|p| touches_ignore_rules(p)) {
                trees.iter().for_each(|t| t.filter.reload());
            }
            // The tree with the longest root holding each path.
            let tree_of = |p: &Path| {
                trees
                    .iter()
                    .filter(|t| p.starts_with(&t.root))
                    .max_by_key(|t| t.root.as_os_str().len())
            };
            let changed: BTreeSet<String> = paths
                .iter()
                .filter_map(|p| tree_of(p).and_then(|t| t.relevant(p)))
                .collect();
            let git_changed = paths.iter().any(|p| {
                trees
                    .iter()
                    .any(|t| t.git.as_ref().is_some_and(|g| g.matches(p)))
            });
            if !changed.is_empty() {
                tracing::debug!(count = changed.len(), paths = ?changed, "watched changes");
                on_change(changed);
            }
            if git_changed {
                tracing::debug!("git history may have changed");
                on_git();
            }
        }
        Err(e) => {
            // Events may have been lost (e.g. a buffer overflow): re-index everything
            // rather than serve a stale graph.
            tracing::warn!(error = %e, "watcher error; scheduling a full re-index");
            on_change(BTreeSet::new());
        }
    };
    let (tx, rx) = mpsc::channel::<Result<Vec<PathBuf>, String>>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let msg = match res {
            Ok(ev) if !is_change(&ev.kind) || ev.paths.is_empty() => return,
            Ok(ev) => Ok(ev.paths),
            Err(e) => Err(e.to_string()),
        };
        // The receiver is gone only when the batching thread ended.
        let _ = tx.send(msg);
    })
    .map_err(|e| format!("cannot create the file watcher: {e}"))?;
    std::thread::Builder::new()
        .name("cartograph-watch".into())
        .spawn(move || batch_events(&rx, handle))
        .map_err(|e| format!("cannot start the watcher thread: {e}"))?;
    for r in &roots {
        watcher
            .watch(r, RecursiveMode::Recursive)
            .map_err(|e| format!("cannot watch {}: {e}", r.display()))?;
    }
    if workspace.is_some()
        && let Err(e) = watcher.watch(root, RecursiveMode::NonRecursive)
    {
        tracing::warn!(dir = %root.display(), error = %e, "cannot watch the workspace file");
    }
    for (dir, mode) in extra {
        // Without it, new commits show up at the next file change instead.
        if let Err(e) = watcher.watch(&dir, mode) {
            tracing::warn!(dir = %dir.display(), error = %e, "cannot watch the git dir");
        }
    }
    Ok(Watcher { _watcher: watcher })
}

/// Hand `handle` one batch per burst of events: the paths seen until [`DEBOUNCE`] passes
/// without a new one (at most [`MAX_BATCH_WAIT`]), deduplicated. An error ends the batch
/// and is handed over on its own. Returns when the watcher is dropped.
fn batch_events(
    rx: &mpsc::Receiver<Result<Vec<PathBuf>, String>>,
    handle: impl Fn(Result<Vec<PathBuf>, String>),
) {
    while let Ok(first) = rx.recv() {
        let mut paths: BTreeSet<PathBuf> = BTreeSet::new();
        let mut error = None;
        let started = Instant::now();
        let mut next = Some(first);
        loop {
            match next.take() {
                Some(Ok(p)) => paths.extend(p),
                Some(Err(e)) => error = Some(e),
                None => {}
            }
            if started.elapsed() >= MAX_BATCH_WAIT {
                break;
            }
            match rx.recv_timeout(DEBOUNCE) {
                Ok(msg) => next = Some(msg),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }
        if let Some(e) = error {
            handle(Err(e));
        }
        if !paths.is_empty() {
            handle(Ok(paths.into_iter().collect()));
        }
    }
}

#[cfg(test)]
mod event_tests {
    use super::*;
    use notify::event::{CreateKind, ModifyKind};

    #[test]
    fn reads_are_not_changes() {
        assert!(!is_change(&EventKind::Access(AccessKind::Open(
            AccessMode::Any
        ))));
        assert!(!is_change(&EventKind::Access(AccessKind::Read)));
        assert!(!is_change(&EventKind::Access(AccessKind::Close(
            AccessMode::Read
        ))));
        assert!(is_change(&EventKind::Access(AccessKind::Close(
            AccessMode::Write
        ))));
        assert!(is_change(&EventKind::Modify(ModifyKind::Any)));
        assert!(is_change(&EventKind::Create(CreateKind::File)));
        assert!(is_change(&EventKind::Any));
    }

    #[test]
    fn a_burst_becomes_one_deduplicated_batch() {
        let (tx, rx) = mpsc::channel();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let t = std::thread::spawn(move || {
            batch_events(&rx, move |b| s2.lock().unwrap().push(b));
        });
        tx.send(Ok(vec![PathBuf::from("a"), PathBuf::from("b")]))
            .unwrap();
        tx.send(Ok(vec![PathBuf::from("a")])).unwrap();
        std::thread::sleep(DEBOUNCE * 3);
        tx.send(Ok(vec![PathBuf::from("c")])).unwrap();
        drop(tx);
        t.join().unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(
            *seen,
            vec![
                Ok(vec![PathBuf::from("a"), PathBuf::from("b")]),
                Ok(vec![PathBuf::from("c")])
            ]
        );
    }
}
