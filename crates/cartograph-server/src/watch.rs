//! File watcher: a notify debouncer (300 ms) over the repo root. Events for paths the
//! walker would never index (the data dir, `.git`, the always-excluded dirs, gitignored
//! paths) are dropped here, so writing the database never retriggers indexing; the rest
//! become one `Watch` request on the scheduler.
//!
//! A commit that changes no working-tree file (files edited and indexed before, then
//! committed) only touches git's own files. The few that mean new history ([`GitWatch`]:
//! `HEAD`, its reflog, branch refs, `packed-refs`, `shallow`) become a `Git` request: the
//! history is read again, no file is. The git dirs are watched too when they are outside
//! the root (a linked worktree, a root below the work tree's top).

use crate::scheduler::Reason;
use crate::state::AppState;
use cartograph_index::git::snapshot::{LEGACY_SNAPSHOT_DIR, only_legacy_snapshots};
use cartograph_index::walk::EXCLUDED_DIRS;
use cartograph_index::{DATA_DIRS, DATA_FILES};
use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::RecursiveMode;
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Debounce window.
pub const DEBOUNCE: Duration = Duration::from_millis(300);

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
    _debouncer: Debouncer<notify::RecommendedWatcher>,
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

/// Watch `root` (`root` and `data_dir` normalized, see `crate::normalize`) and call
/// `on_change` with the repo-relative paths that can change the index. An empty set means
/// "unknown" (the watcher lost events): re-index everything. `on_git` is called when git's
/// history may have changed ([`GitWatch`]), with the same debounce. Used by `serve` and by
/// the MCP server.
pub fn start_with(
    root: &Path,
    data_dir: &Path,
    on_change: impl Fn(BTreeSet<String>) + Send + 'static,
    on_git: impl Fn() + Send + 'static,
) -> Result<Watcher, String> {
    let filter = WatchFilter::new(root, data_dir);
    let git = GitWatch::find(root);
    let extra = git
        .as_ref()
        .map(|g| g.extra_watches(root))
        .unwrap_or_default();
    let mut debouncer = new_debouncer(DEBOUNCE, move |res: DebounceEventResult| match res {
        Ok(events) => {
            if events.iter().any(|e| touches_ignore_rules(&e.path)) {
                filter.reload();
            }
            let changed: BTreeSet<String> = events
                .iter()
                .filter_map(|e| filter.relevant(&e.path))
                .collect();
            let git_changed = git
                .as_ref()
                .is_some_and(|g| events.iter().any(|e| g.matches(&e.path)));
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
    })
    .map_err(|e| format!("cannot create the file watcher: {e}"))?;
    debouncer
        .watcher()
        .watch(root, RecursiveMode::Recursive)
        .map_err(|e| format!("cannot watch {}: {e}", root.display()))?;
    for (dir, mode) in extra {
        // Without it, new commits show up at the next file change instead.
        if let Err(e) = debouncer.watcher().watch(&dir, mode) {
            tracing::warn!(dir = %dir.display(), error = %e, "cannot watch the git dir");
        }
    }
    Ok(Watcher {
        _debouncer: debouncer,
    })
}
