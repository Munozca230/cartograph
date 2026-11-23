//! The index behind the MCP tools: a background worker that indexes the repo (at start,
//! when the watcher sees a change, when a request names a file that changed), the current
//! query [`Repo`] swapped in atomically, and the freshness gate every request passes.
//!
//! Requests never wait on an index longer than [`Options::fresh_wait`]: with no index yet
//! they get "indexing N %", and with a stale one they get the last answer plus a line that
//! says so.
//!
//! Git history never holds up the file graph: a run publishes its state with the history
//! stored so far, then brings the history up to date and publishes one more generation only
//! if it changed (as the HTTP server's worker does). A change seen only in git (a commit of
//! files already indexed) runs just that history step.

use crate::annotations::{self, FileHashes};
use cartograph_core::{Graph, History, Store, Symbols};
use cartograph_index::{DB_FILE, IndexOptions, Progress};
use cartograph_query::{Config, ConfigError, Repo};
use cartograph_server::scheduler::{Job, Reason, Scheduler};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::watch;

/// File in the data dir that a running MCP server holds locked (see [`server_running`]).
pub const LOCK_FILE: &str = "mcp.lock";

/// How the server runs.
#[derive(Debug, Clone)]
pub struct Options {
    pub root: PathBuf,
    /// `None` = `<root>/.cartograph`.
    pub data_dir: Option<PathBuf>,
    /// Rules file instead of `<root>/cartograph.toml`.
    pub config: Option<PathBuf>,
    /// Watch the repo and re-index on changes.
    pub watch: bool,
    /// Also send the JSON of each answer as `structuredContent`. Off by default: some clients
    /// show only the structured JSON to the model, *instead of* the compact text.
    pub structured: bool,
    pub max_file_bytes: u64,
    /// Longest a tool may run before it answers with a timeout error.
    pub tool_timeout: Duration,
    /// Longest a request waits for a pending (re)index before answering anyway.
    pub fresh_wait: Duration,
    /// Queries running at once; more wait for a slot (inside their timeout).
    pub max_concurrent: usize,
    /// Record each tool call in `<data dir>/stats.jsonl` (see [`crate::stats`]).
    pub stats: bool,
    /// `annotate` writes to `<root>/cartograph-notes.toml` even before the file exists
    /// (see [`crate::annotations`]).
    pub notes_in_repo: bool,
    /// In a workspace, the member a plain path is looked for in first (`--member`).
    pub member: Option<String>,
}

impl Options {
    pub fn new(root: impl Into<PathBuf>) -> Options {
        Options {
            root: root.into(),
            data_dir: None,
            config: None,
            watch: true,
            structured: false,
            max_file_bytes: cartograph_index::walk::DEFAULT_MAX_FILE_BYTES,
            tool_timeout: Duration::from_secs(30),
            fresh_wait: Duration::from_secs(8),
            max_concurrent: 4,
            stats: true,
            notes_in_repo: false,
            member: None,
        }
    }
}

/// One indexed state of the repo.
pub struct Loaded {
    pub repo: Repo,
    /// The git history `repo` was built with: a git base (`diff`, `check(base)`) gets the
    /// same one, so the two sides never differ in history.
    pub history: History,
    /// Size and content hash of every indexed file, by repo-relative path.
    pub files: BTreeMap<String, (u64, String)>,
    pub hashes: FileHashes,
    /// Paths the indexer saw but skipped (too large, binary…): never "new files".
    pub skipped: std::collections::HashSet<String>,
    pub generation: u64,
    /// Read from the database of an earlier run, not indexed by this server yet.
    pub from_disk: bool,
    /// When the run that produced it started (files changed after this may be stale).
    pub started: SystemTime,
    pub config_error: Option<String>,
    /// The paths the run was asked for (the watcher's or a request's; empty for a startup
    /// run, a git-only update or a state read from disk).
    pub changed: BTreeSet<String>,
}

impl Loaded {
    /// In a workspace, the member plain paths are looked for in first (`--member`).
    pub fn with_member(mut self, member: Option<String>) -> Loaded {
        self.repo.set_default_member(member);
        self
    }

    pub fn build(
        graph: &Graph,
        symbols: &Symbols,
        history: History,
        config: Result<Config, String>,
        generation: u64,
        from_disk: bool,
        started: SystemTime,
    ) -> Loaded {
        let (config, config_error) = match config {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(e)),
        };
        let files = graph
            .nodes
            .iter()
            .filter(|n| n.kind == cartograph_core::NodeKind::File)
            .map(|n| {
                (
                    n.path.clone(),
                    (n.bytes, n.hash.clone().unwrap_or_default()),
                )
            })
            .collect();
        let repo = Repo::with_symbols(graph, symbols, config).with_history(&history);
        // Computed here, off the request path.
        let _ = repo.findings();
        Loaded {
            repo,
            history,
            files,
            hashes: annotations::file_hashes(graph),
            skipped: graph.skipped.iter().map(|s| s.path.clone()).collect(),
            generation,
            from_disk,
            started,
            config_error,
            changed: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Default)]
struct Status {
    /// Why the current run started, while one runs.
    running: Option<Reason>,
    /// The watcher or a request saw a change no run has picked up yet.
    dirty: bool,
    /// The last run failed.
    error: Option<String>,
    /// Duration of the last successful run.
    last_ms: Option<u64>,
}

/// What a request gets from [`Engine::ready`].
pub enum Ready {
    /// Answer from this state; `note` (if any) goes first in the answer.
    Go {
        loaded: Arc<Loaded>,
        note: Option<String>,
    },
    /// No index to answer from: the message says why and what to do.
    NotReady(String),
}

pub struct Engine {
    pub root: PathBuf,
    /// The workspace `root` holds (`cartograph-workspace.toml`), if any; read again at each
    /// index run (see [`Engine::workspace`]).
    workspace: RwLock<Option<cartograph_core::Workspace>>,
    pub data_dir: PathBuf,
    pub opts: Options,
    loaded: RwLock<Option<Arc<Loaded>>>,
    pub progress: Arc<Progress>,
    scheduler: Scheduler,
    status: Mutex<Status>,
    generation: watch::Sender<u64>,
    /// Serializes writes of `annotations.json`.
    pub notes_lock: Mutex<()>,
    /// Usage stats, unless turned off.
    pub stats: Option<crate::stats::Recorder>,
    /// Held for the server's lifetime (released by the OS when the process dies).
    _lock: Option<File>,
    _watcher: Mutex<Option<cartograph_server::watch::Watcher>>,
}

/// Whether an MCP server holds the lock of this data dir (another process then has no
/// re-index to do: the server's watcher re-indexes). The probe takes a shared lock, so two
/// probes never see each other; a server starting at that moment retries (see
/// [`take_lock`]), and a lock seen held is probed once more before it counts.
pub fn server_running(data_dir: &Path) -> bool {
    let Ok(f) = File::options()
        .read(true)
        .write(true)
        .open(data_dir.join(LOCK_FILE))
    else {
        return false;
    };
    let held = || match f.try_lock_shared() {
        Ok(()) => {
            let _ = f.unlock();
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => false,
    };
    held() && {
        std::thread::sleep(LOCK_RETRY);
        held()
    }
}

/// Pause between two attempts at the lock.
const LOCK_RETRY: Duration = Duration::from_millis(25);
/// Attempts [`take_lock`] makes before it concludes another server holds the lock.
const LOCK_ATTEMPTS: u32 = 20;

/// The data dir's [`LOCK_FILE`], locked. A probe ([`server_running`]) holds it for a moment,
/// so a lock seen held is tried again for a while before this server runs without it.
fn take_lock(data_dir: &Path) -> Option<File> {
    let f = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data_dir.join(LOCK_FILE))
        .ok()?;
    for attempt in 1..=LOCK_ATTEMPTS {
        match f.try_lock() {
            Ok(()) => return Some(f),
            Err(std::fs::TryLockError::WouldBlock) if attempt < LOCK_ATTEMPTS => {
                std::thread::sleep(LOCK_RETRY)
            }
            Err(e) => {
                tracing::info!(error = %e, "another cartograph MCP server holds the lock; running anyway");
                return None;
            }
        }
    }
    None
}

impl Engine {
    /// Set up (no indexing yet). Fails only when the root is not a directory or the data
    /// dir cannot be created.
    pub fn new(opts: Options) -> Result<Arc<Engine>, String> {
        if !opts.root.is_dir() {
            return Err(format!("{} is not a directory", opts.root.display()));
        }
        let root = cartograph_server::normalize(&opts.root).map_err(|e| e.to_string())?;
        let workspace = cartograph_core::Workspace::find(&root).map_err(|e| e.to_string())?;
        if let Some(m) = &opts.member {
            match &workspace {
                None => {
                    return Err(format!(
                        "--member {m}: {} holds no {}",
                        root.display(),
                        cartograph_core::WORKSPACE_FILE
                    ));
                }
                Some(ws) if ws.member(m).is_none() => {
                    return Err(format!(
                        "--member {m}: not a member of the workspace (members: {})",
                        ws.names().join(", ")
                    ));
                }
                Some(_) => {}
            }
        }
        let data_dir = cartograph_index::data_dir(&root, opts.data_dir.as_deref());
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| format!("cannot create {}: {e}", data_dir.display()))?;
        let data_dir = cartograph_server::normalize(&data_dir).map_err(|e| e.to_string())?;
        let (generation, _) = watch::channel(0);
        let stats = opts.stats.then(|| crate::stats::Recorder::new(&data_dir));
        Ok(Arc::new(Engine {
            stats,
            _lock: take_lock(&data_dir),
            workspace: RwLock::new(workspace),
            root,
            data_dir,
            opts,
            loaded: RwLock::new(None),
            progress: Arc::new(Progress::default()),
            scheduler: Scheduler::default(),
            status: Mutex::new(Status::default()),
            generation,
            notes_lock: Mutex::new(()),
            _watcher: Mutex::new(None),
        }))
    }

    /// Start the background work: load the previous index (if any), index, watch.
    /// Must run inside a Tokio runtime.
    pub fn start(self: &Arc<Engine>) {
        let db = self.data_dir.join(DB_FILE);
        if db.is_file() {
            let eng = self.clone();
            tokio::task::spawn_blocking(move || eng.load_from_disk(&db));
        }
        self.scheduler.request(Reason::Startup, std::iter::empty());
        tokio::spawn(self.clone().worker());
        if self.opts.watch {
            let eng = self.clone();
            let eng_git = self.clone();
            match cartograph_server::watch::start_with(
                &self.root,
                &self.data_dir,
                move |changed| eng.mark_changed(Reason::Watch, changed),
                // Not `dirty`: the file graph is not stale, and answers need not wait.
                move || eng_git.scheduler.request(Reason::Git, std::iter::empty()),
            ) {
                Ok(w) => *self._watcher.lock().unwrap_or_else(|e| e.into_inner()) = Some(w),
                Err(e) => {
                    tracing::warn!(error = %e, "file watcher failed; freshness relies on per-request checks")
                }
            }
        }
    }

    /// The workspace the root holds, as the last index run read it.
    pub fn workspace(&self) -> Option<cartograph_core::Workspace> {
        self.workspace
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Read the workspace file again (its members may have changed); an invalid one stops
    /// the run, as indexing it would.
    fn reload_workspace(&self) -> Result<(), String> {
        let ws = cartograph_core::Workspace::find(&self.root).map_err(|e| e.to_string())?;
        if let (Some(m), Some(w)) = (&self.opts.member, &ws)
            && w.member(m).is_none()
        {
            tracing::warn!(member = %m, "--member is no member of the workspace any more");
        }
        *self.workspace.write().unwrap_or_else(|e| e.into_inner()) = ws;
        Ok(())
    }

    /// Whether the file watcher runs (it may have failed to start).
    pub fn watching(&self) -> bool {
        self._watcher
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Stop any running index at its next check.
    pub fn cancel(&self) {
        self.progress.cancel();
    }

    fn status(&self) -> MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record a file change no run has picked up yet and ask for a run. `dirty` and the
    /// request change under the status lock, as [`Engine::next_job`] takes a job and clears
    /// `dirty`: a change is either in the job taken or still marked dirty, never lost in
    /// between.
    fn mark_changed(&self, reason: Reason, changed: impl IntoIterator<Item = String>) {
        let mut st = self.status();
        st.dirty = true;
        self.scheduler.request(reason, changed);
    }

    /// The next job, taken under the status lock with `running` set and `dirty` cleared
    /// (see [`Engine::mark_changed`]).
    async fn next_job(&self) -> Job {
        loop {
            {
                let mut st = self.status();
                if let Some(job) = self.scheduler.take() {
                    st.running = Some(job.reason);
                    // A git-only job carries no file change (those merge into a higher
                    // reason).
                    if job.reason != Reason::Git {
                        st.dirty = false;
                    }
                    return job;
                }
            }
            self.scheduler.wait().await;
        }
    }

    pub fn current(&self) -> Option<Arc<Loaded>> {
        self.loaded
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_loaded(&self, l: Arc<Loaded>) {
        let g = l.generation;
        *self.loaded.write().unwrap_or_else(|e| e.into_inner()) = Some(l);
        self.generation.send_replace(g);
    }

    fn config(&self) -> Result<Config, String> {
        self.load_config().map_err(|e| e.to_string())
    }

    /// The architecture rules: `--config`, else `<root>/cartograph.toml`.
    pub fn load_config(&self) -> Result<Config, ConfigError> {
        match &self.opts.config {
            Some(p) => Config::load_file(p, false),
            None => Config::load(&self.root),
        }
    }

    /// Follow the published states: the value is the generation, and every publication
    /// (a new index, a git update, the index read from disk) marks the receiver changed.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.generation.subscribe()
    }

    /// Ask for a full re-index (the web map's "reindex" button), as a request naming a
    /// changed file would.
    pub fn request_reindex(&self) {
        self.mark_changed(Reason::Manual, std::iter::empty());
    }

    fn load_from_disk(&self, db: &Path) {
        let started = Instant::now();
        let res = (|| -> Result<Loaded, String> {
            let store = Store::open(db).map_err(|e| e.to_string())?;
            let (graph, symbols) = store.load_graph_and_symbols().map_err(|e| e.to_string())?;
            let history = store.load_history().map_err(|e| e.to_string())?;
            Ok(Loaded::build(
                &graph,
                &symbols,
                history,
                self.config(),
                0,
                true,
                SystemTime::UNIX_EPOCH,
            )
            .with_member(self.opts.member.clone()))
        })();
        match res {
            Ok(l) => {
                let mut slot = self.loaded.write().unwrap_or_else(|e| e.into_inner());
                // A fresh index that finished first wins.
                if slot.is_none() {
                    *slot = Some(Arc::new(l));
                    drop(slot);
                    self.generation.send_replace(0);
                    tracing::info!(
                        ms = started.elapsed().as_millis() as u64,
                        "previous index loaded"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "previous index unreadable; waiting for a new one")
            }
        }
    }

    async fn worker(self: Arc<Engine>) {
        // Generation of the last state this worker published.
        let mut generation = 0;
        loop {
            let job = self.next_job().await;
            if job.reason == Reason::Git
                && let Some(cur) = self.current().filter(|l| !l.from_disk)
            {
                // Only git changed: the files are as indexed.
                if self.git_pass(generation + 1, None, cur.started).await {
                    generation += 1;
                }
                self.status().running = None;
                continue;
            }
            self.progress.reset();
            generation += 1;
            let eng = self.clone();
            let t0 = Instant::now();
            let res = tokio::task::spawn_blocking(move || eng.index_once(generation)).await;
            let outcome = match res {
                Ok(r) => r,
                Err(e) if e.is_panic() => Err(format!(
                    "indexer panicked: {}",
                    cartograph_server::panic_message(e.into_panic().as_ref())
                )),
                Err(e) => Err(format!("indexer task failed: {e}")),
            };
            let ms = t0.elapsed().as_millis() as u64;
            let index = match outcome {
                Ok((mut l, graph, symbols)) => {
                    tracing::info!(
                        ms,
                        reason = job.reason.as_str(),
                        changed = job.changed.len(),
                        "index finished"
                    );
                    let started = l.started;
                    l.changed = job.changed.clone();
                    self.set_loaded(Arc::new(l));
                    // Served: requests stop waiting here, not after the history.
                    let mut st = self.status();
                    st.error = None;
                    st.last_ms = Some(ms);
                    st.running = None;
                    Some((graph, symbols, started))
                }
                Err(e) => {
                    tracing::error!(error = %e, "index failed");
                    let mut st = self.status();
                    st.error = Some(e);
                    st.running = None;
                    None
                }
            };
            if self.progress.cancelled() {
                return;
            }
            if let Some((graph, symbols, started)) = index
                && self
                    .git_pass(generation + 1, Some((graph, symbols)), started)
                    .await
            {
                generation += 1;
            }
            if self.progress.cancelled() {
                return;
            }
        }
    }

    /// Index the files into the store and build the state to serve, with the git history
    /// stored so far (see [`Engine::git_pass`]). The graph and symbols come back too, for
    /// the git pass that follows.
    fn index_once(&self, generation: u64) -> Result<(Loaded, Graph, Symbols), String> {
        let started = SystemTime::now();
        self.reload_workspace()?;
        let opts = IndexOptions {
            max_file_bytes: self.opts.max_file_bytes,
            data_dir: Some(self.data_dir.clone()),
            progress: Some(self.progress.clone()),
            ..IndexOptions::default()
        };
        let out = cartograph_index::index_to_store(&self.root, &opts).map_err(|e| e.to_string())?;
        let history = self.stored_history();
        let loaded = Loaded::build(
            &out.graph,
            &out.symbols,
            history,
            self.config(),
            generation,
            false,
            started,
        )
        .with_member(self.opts.member.clone());
        Ok((loaded, out.graph, out.symbols))
    }

    /// The history in the store (empty when there is none or it cannot be read).
    fn stored_history(&self) -> History {
        Store::open(self.data_dir.join(DB_FILE))
            .and_then(|s| s.load_history())
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "stored git history not loaded");
                History::default()
            })
    }

    /// Bring the git layer up to date (incremental by commit cursor, cheap when no commit is
    /// new) and, when it changed, publish the state as `generation`: the file graph of
    /// `index` (else the one in the store, which is the one served) with the new history,
    /// and `started` of the run that indexed the files. Whether it published. A problem is
    /// logged, and the answers keep the history they have.
    async fn git_pass(
        self: &Arc<Engine>,
        generation: u64,
        index: Option<(Graph, Symbols)>,
        started: SystemTime,
    ) -> bool {
        let eng = self.clone();
        let t0 = Instant::now();
        let res = tokio::task::spawn_blocking(move || -> Result<Option<Loaded>, String> {
            let mut store = Store::open(eng.data_dir.join(DB_FILE)).map_err(|e| e.to_string())?;
            let outcome =
                cartograph_index::git::update(&eng.root, &mut store).map_err(|e| e.to_string())?;
            if !outcome.changed {
                return Ok(None);
            }
            let history = store.load_history().map_err(|e| e.to_string())?;
            let (graph, symbols) = match index {
                Some(x) => x,
                None => store.load_graph_and_symbols().map_err(|e| e.to_string())?,
            };
            Ok(Some(
                Loaded::build(
                    &graph,
                    &symbols,
                    history,
                    eng.config(),
                    generation,
                    false,
                    started,
                )
                .with_member(eng.opts.member.clone()),
            ))
        })
        .await;
        let ms = t0.elapsed().as_millis() as u64;
        match res {
            Ok(Ok(Some(l))) => {
                tracing::info!(generation, ms, "git history updated");
                self.set_loaded(Arc::new(l));
                true
            }
            Ok(Ok(None)) => {
                tracing::debug!(ms, "git history unchanged");
                false
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "git history not updated");
                false
            }
            Err(e) => {
                tracing::warn!(error = %e, "git history task failed");
                false
            }
        }
    }

    /// "indexing 37 % (1,234 of 3,300 files)" or "indexing (walking the tree)".
    pub fn progress_text(&self) -> String {
        let (done, total) = self.progress.get();
        if total == 0 {
            "walking the tree".to_string()
        } else {
            format!(
                "{} % ({done} of {total} files read)",
                (done * 100).checked_div(total).unwrap_or(0)
            )
        }
    }

    /// Wait (bounded) until the index can answer for `args` (the paths and ids the request
    /// names): an index exists, no change seen by the watcher is pending, and the named
    /// files are unchanged since they were indexed.
    pub async fn ready(&self, args: &[String]) -> Ready {
        let deadline = Instant::now() + self.opts.fresh_wait;
        let mut rx = self.generation.subscribe();
        // 1. Any index at all.
        loop {
            if self.current().is_some() {
                break;
            }
            let (running, error) = {
                let st = self.status();
                (st.running.is_some(), st.error.clone())
            };
            if let Some(e) = error.filter(|_| !running) {
                return Ready::NotReady(format!(
                    "cartograph could not index {}: {e}. Fix the cause and call the tool again \
                     (the server retries on the next file change); meanwhile use grep/read.",
                    self.root.display()
                ));
            }
            if Instant::now() >= deadline {
                return Ready::NotReady(format!(
                    "cartograph is indexing {} for the first time: {}. Call the tool again in a \
                     few seconds; meanwhile use grep/read.",
                    self.root.display(),
                    self.progress_text()
                ));
            }
            wait(&mut rx, deadline).await;
        }
        // 2. The files this request names.
        if let Some(l) = self.current() {
            let disk = DiskMap {
                root: self.root.clone(),
                workspace: self.workspace(),
                member: self.opts.member.clone(),
            };
            let args = args.to_vec();
            let stale = tokio::task::spawn_blocking(move || stale_paths(&disk, &l, &args))
                .await
                .unwrap_or_default();
            if !stale.is_empty() {
                tracing::debug!(?stale, "request names changed files; re-indexing first");
                self.mark_changed(Reason::Manual, stale);
            }
        }
        // 3. Pending changes.
        let mut note = None;
        loop {
            let pending = {
                let st = self.status();
                st.dirty || matches!(st.running, Some(Reason::Watch | Reason::Manual))
            };
            if !pending {
                break;
            }
            if Instant::now() >= deadline {
                note = Some(format!(
                    "(index refresh after recent file changes still running: {}; this answer \
                     may miss those changes)",
                    self.progress_text()
                ));
                break;
            }
            wait(&mut rx, deadline).await;
        }
        let Some(loaded) = self.current() else {
            return Ready::NotReady("cartograph has no index yet; call the tool again".into());
        };
        if note.is_none() && loaded.from_disk {
            note = Some(format!(
                "(index from an earlier session; refreshing: {})",
                self.progress_text()
            ));
        }
        Ready::Go { loaded, note }
    }

    /// Short state for the logs and `--status`: generation, last run, errors.
    pub fn describe(&self) -> String {
        let st = self.status();
        format!(
            "generation {}, running {:?}, dirty {}, last {:?} ms, error {:?}",
            self.current().map_or(0, |l| l.generation),
            st.running.map(Reason::as_str),
            st.dirty,
            st.last_ms,
            st.error
        )
    }
}

async fn wait(rx: &mut watch::Receiver<u64>, deadline: Instant) {
    let left = deadline.saturating_duration_since(Instant::now());
    let step = left.min(Duration::from_millis(50));
    let _ = tokio::time::timeout(step, rx.changed()).await;
}

/// The repo-relative path a tool argument names, if it names a file path: `file:p`,
/// `sym:p#name`, `p#name`, `p` (ids of folders and libraries are left out).
///
/// A `#` can be in a path (`src/c#/a.cs`) and in a name (`Model.#p`, `r#match`), so a
/// `file:` id is never split, and the others split as the query layer does: at the first
/// `#` whose path is an indexed file (`known`), else by the symbol-id rule
/// ([`cartograph_core::split_path_name`]); what follows a path is a name only if it holds
/// no `/`.
pub fn arg_path(arg: &str, known: impl Fn(&str) -> bool) -> Option<String> {
    let s = arg.trim().replace('\\', "/");
    if s.starts_with("dir:") || s.starts_with("external:") || s.starts_with("ext:") || s == "repo:"
    {
        return None;
    }
    let s = match s.strip_prefix("file:") {
        Some(p) => p,
        None => {
            let s = s.strip_prefix("sym:").unwrap_or(&s);
            let clean = |p: &str| p.strip_prefix("./").unwrap_or(p).to_string();
            if known(&clean(s)) {
                s
            } else {
                s.match_indices('#')
                    .map(|(i, _)| &s[..i])
                    .find(|p| known(&clean(p)))
                    .or_else(|| {
                        cartograph_core::split_path_name(s)
                            .filter(|(p, name)| !p.ends_with('.') && !name.contains('/'))
                            .map(|(p, _)| p)
                    })
                    .unwrap_or(s)
            }
        }
    };
    let s = s.strip_prefix("./").unwrap_or(s).trim_end_matches('/');
    // Only paths inside the repo: nothing absolute (the tools make those relative first)
    // and no `..`.
    let inside = !s.starts_with('/') && !s.contains(':') && !s.split('/').any(|p| p == "..");
    (inside && !s.is_empty() && s != ".").then(|| s.to_string())
}

/// Where the paths of the index are on disk: below the root, or, in a workspace, below the
/// folder of the member they start with.
#[derive(Debug, Clone)]
pub struct DiskMap {
    pub root: PathBuf,
    pub workspace: Option<cartograph_core::Workspace>,
    /// The member a plain path is looked for in first.
    pub member: Option<String>,
}

impl DiskMap {
    pub fn to_disk(&self, path: &str) -> Option<PathBuf> {
        match &self.workspace {
            Some(ws) => ws.to_disk(path),
            None => Some(self.root.join(path)),
        }
    }

    /// `arg` (a path, `member:path`, or a path relative to a member) as a path of the
    /// index, given the paths it knows: as [`cartograph_query::workspace::Members`] reads
    /// it, an ambiguous one left as it was.
    fn canonical(&self, arg: &str, known: impl Fn(&str) -> bool) -> String {
        let Some(ws) = &self.workspace else {
            return arg.to_string();
        };
        let members = cartograph_query::workspace::Members {
            names: ws.members.iter().map(|m| m.name.clone()).collect(),
            default: self.member.clone(),
        };
        members
            .canonical(arg, |p| {
                known(p) || ws.to_disk(p).is_some_and(|d| d.is_dir())
            })
            .unwrap_or_else(|_| arg.to_string())
    }
}

/// Files named by `args` whose content differs from the index (edited, deleted, or new
/// since the index run started).
pub fn stale_paths(disk: &DiskMap, l: &Loaded, args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for a in args {
        let a = disk.canonical(a, |p| l.files.contains_key(p));
        let Some(p) = arg_path(&a, |p| l.files.contains_key(p)) else {
            continue;
        };
        let Some(abs) = disk.to_disk(&p) else {
            continue;
        };
        match l.files.get(&p) {
            Some((bytes, hash)) => {
                let changed = match std::fs::metadata(&abs) {
                    Err(_) => true,
                    Ok(m) if m.len() != *bytes => true,
                    Ok(_) => std::fs::read(&abs)
                        .map(|c| blake3::hash(&c).to_hex().to_string() != *hash)
                        .unwrap_or(true),
                };
                if changed {
                    out.push(p);
                }
            }
            None => {
                if l.skipped.contains(&p) {
                    continue;
                }
                let new = std::fs::metadata(&abs)
                    .is_ok_and(|m| m.is_file() && m.modified().is_ok_and(|t| t >= l.started));
                if new {
                    out.push(p);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_file(dir: &Path) -> File {
        File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(LOCK_FILE))
            .unwrap()
    }

    /// A server starting while another process probes the lock waits for the probe, so it does not
    /// run without the lock (and later probes see it).
    #[test]
    fn a_probe_does_not_keep_a_server_from_its_lock() {
        let dir = tempfile::tempdir().unwrap();
        let probe = lock_file(dir.path());
        probe.try_lock_shared().unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(probe);
        });
        let held = take_lock(dir.path());
        release.join().unwrap();
        assert!(held.is_some());
        assert!(server_running(dir.path()));
        drop(held);
        assert!(!server_running(dir.path()));
    }

    /// Probes take a shared lock, so they never see each other as a server.
    #[test]
    fn probes_do_not_see_each_other() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!server_running(dir.path()));
        let other = lock_file(dir.path());
        other.try_lock_shared().unwrap();
        assert!(!server_running(dir.path()));
        drop(other);
        let server = lock_file(dir.path());
        server.try_lock().unwrap();
        assert!(server_running(dir.path()));
    }

    #[test]
    fn arg_paths() {
        let none = |_: &str| false;
        let arg_path = |a: &str| arg_path(a, none);
        assert_eq!(arg_path("file:src/a.rs").as_deref(), Some("src/a.rs"));
        assert_eq!(arg_path("sym:src/a.rs#run").as_deref(), Some("src/a.rs"));
        assert_eq!(arg_path("src\\a.rs#Foo.bar").as_deref(), Some("src/a.rs"));
        assert_eq!(arg_path("./src/").as_deref(), Some("src"));
        assert_eq!(arg_path("dir:src"), None);
        assert_eq!(arg_path("external:npm:react"), None);
        assert_eq!(arg_path("."), None);
        assert_eq!(arg_path("repo:"), None);
        assert_eq!(arg_path("../../etc/passwd"), None);
        assert_eq!(arg_path("C:/Windows/x"), None);
        assert_eq!(arg_path("/etc/passwd"), None);
    }

    #[test]
    fn a_hash_in_a_path_or_a_name_does_not_cut_the_path() {
        let none = |_: &str| false;
        let p = |a: &str| arg_path(a, none);
        // `#` in the path: a file id is never split, a path with a name is split where the
        // symbol-id rule says, and a bare path keeps its `#`.
        assert_eq!(p("file:src/c#/a.cs").as_deref(), Some("src/c#/a.cs"));
        assert_eq!(p("src/c#/a.cs").as_deref(), Some("src/c#/a.cs"));
        assert_eq!(p("src/c#/a.cs#run").as_deref(), Some("src/c#/a.cs"));
        assert_eq!(p("sym:src/c#/a.cs#Api.run").as_deref(), Some("src/c#/a.cs"));
        // `#` in the name.
        assert_eq!(p("sym:src/a.js#Model.#p").as_deref(), Some("src/a.js"));
        assert_eq!(p("src/a.rs#r#match").as_deref(), Some("src/a.rs"));
        // A known file wins: its path has no extension, so only the index can tell.
        let known = |s: &str| s == "tools/c#/build";
        assert_eq!(
            arg_path("tools/c#/build#main", known).as_deref(),
            Some("tools/c#/build")
        );
        assert_eq!(
            arg_path("tools/c#/build", known).as_deref(),
            Some("tools/c#/build")
        );
    }
}
