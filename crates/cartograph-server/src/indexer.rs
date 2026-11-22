//! The single indexing worker. Requests (startup, watcher, `POST /api/reindex`) merge into
//! one pending job; the worker takes it, runs it, and loops. So two indexes never run at
//! once, and whatever arrives during a run becomes exactly one more run.
//!
//! The git history is read after the file graph is served: a run publishes its snapshot
//! (with the history stored so far), then brings the history up to date and, only when it
//! changed, publishes one more snapshot (next generation, one more `index-finished`).
//! A change seen only in git (a commit of files already indexed, a checkout, a fetch) is a
//! [`Reason::Git`] job: just that history step, no file is read again.

pub use crate::scheduler::{Job, Reason, Scheduler};
use crate::snapshot::Snapshot;
use crate::state::AppState;
use cartograph_core::{Graph, History, Store, Symbols};
use cartograph_index::{DB_FILE, IndexOptions, Progress};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Inputs of one index run.
#[derive(Debug, Clone)]
pub struct IndexRequest {
    pub root: PathBuf,
    pub data_dir: PathBuf,
    pub max_file_bytes: u64,
    pub progress: Arc<Progress>,
}

/// Runs one index and returns the stored graph and run metadata. Injectable for tests.
pub type IndexFn =
    Arc<dyn Fn(&IndexRequest) -> Result<(Graph, BTreeMap<String, String>), String> + Send + Sync>;

/// The real indexer: index into the store, then read the snapshot back from it.
pub fn default_index_fn() -> IndexFn {
    Arc::new(|req: &IndexRequest| {
        let opts = IndexOptions {
            max_file_bytes: req.max_file_bytes,
            data_dir: Some(req.data_dir.clone()),
            progress: Some(req.progress.clone()),
            ..IndexOptions::default()
        };
        cartograph_index::index_to_store(&req.root, &opts).map_err(|e| e.to_string())?;
        let store = Store::open(req.data_dir.join(DB_FILE)).map_err(|e| e.to_string())?;
        store.load_snapshot().map_err(|e| e.to_string())
    })
}

/// Most changed paths an event carries.
pub const MAX_CHANGED: usize = 500;

/// How often `index-progress` may be sent.
pub const PROGRESS_EVERY: Duration = Duration::from_millis(250);

fn changed_json(changed: &BTreeSet<String>) -> (serde_json::Value, usize) {
    let list: Vec<&String> = changed.iter().take(MAX_CHANGED).collect();
    (json!(list), changed.len())
}

/// The worker loop. Runs until the task is aborted.
pub async fn run_worker(state: Arc<AppState>, index_fn: IndexFn, max_file_bytes: u64, git: bool) {
    loop {
        let job = state.scheduler.next().await;
        if job.reason == Reason::Git {
            if !git {
                continue;
            }
            if state.snapshot().is_some() {
                update_history(&state).await;
                continue;
            }
            // No graph served yet: a full run, which reads the history too.
        }
        let (changed, changed_total) = changed_json(&job.changed);
        state.progress.reset();
        state.set_running();
        let mut started_ev = json!({ "reason": job.reason.as_str(), "changed": changed });
        if changed_total > MAX_CHANGED {
            started_ev["changed_total"] = json!(changed_total);
        }
        state.emit("index-started", started_ev);
        tracing::info!(
            reason = job.reason.as_str(),
            changed = changed_total,
            "index started"
        );

        let mut ticker = Ticker::start(state.clone());
        let started = Instant::now();
        let req = IndexRequest {
            root: state.root.clone(),
            data_dir: state.data_dir.clone(),
            max_file_bytes,
            progress: state.progress.clone(),
        };
        let next_generation = state.generation() + 1;
        let f = index_fn.clone();
        let res = tokio::task::spawn_blocking(move || -> Result<Snapshot, String> {
            let (graph, meta) = f(&req)?;
            // Read on every run: an edit of cartograph.toml re-indexes through the watcher.
            let config = cartograph_query::Config::load(&req.root);
            if let Err(e) = &config {
                tracing::warn!(error = %e, "cartograph.toml ignored");
            }
            let history = stored_history(&req.data_dir);
            let symbols = stored_symbols(&req.data_dir);
            Ok(Snapshot::build_all(
                graph,
                &meta,
                next_generation,
                config,
                &history,
                &symbols,
            ))
        })
        .await;
        // The ticker is fully stopped before `index-finished` / `index-error`, so no
        // `index-progress` can follow them.
        ticker.stop().await;
        let ms = started.elapsed().as_millis() as u64;
        let outcome = match res {
            Ok(r) => r,
            Err(e) if e.is_panic() => Err(format!("indexer panicked: {}", panic_text(e))),
            Err(e) => Err(format!("indexer task failed: {e}")),
        };
        match outcome {
            Ok(snap) => {
                let generation = snap.generation;
                let counts = json!(snap.counts);
                state.set_snapshot(Arc::new(snap));
                state.finish(None);
                let mut ev = json!({
                    "generation": generation,
                    "ms": ms,
                    "changed": changed,
                    "counts": counts,
                });
                if changed_total > MAX_CHANGED {
                    ev["changed_total"] = json!(changed_total);
                }
                state.emit("index-finished", ev);
                tracing::info!(generation, ms, "index finished");
                if git {
                    update_history(&state).await;
                }
            }
            Err(message) => {
                tracing::error!(%message, "index failed");
                state.finish(Some(message.clone()));
                state.emit("index-error", json!({ "message": message }));
            }
        }
    }
}

/// The history stored in the data dir (empty when there is none or it cannot be read).
fn stored_history(data_dir: &std::path::Path) -> History {
    let db = data_dir.join(DB_FILE);
    if !db.is_file() {
        return History::default();
    }
    Store::open(&db)
        .and_then(|s| s.load_history())
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "git history not loaded");
            History::default()
        })
}

/// The symbol layer stored in the data dir (empty, and not marked indexed, when there is
/// none or it cannot be read: the flow endpoints then say to re-index).
fn stored_symbols(data_dir: &std::path::Path) -> Symbols {
    let db = data_dir.join(DB_FILE);
    if !db.is_file() {
        return Symbols::default();
    }
    Store::open(&db)
        .and_then(|s| s.load_symbols())
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "symbols not loaded");
            Symbols::default()
        })
}

/// Bring the git history up to date; when it changed, publish a snapshot with it.
async fn update_history(state: &Arc<AppState>) {
    let root = state.root.clone();
    let data_dir = state.data_dir.clone();
    let next_generation = state.generation() + 1;
    let started = Instant::now();
    let res = tokio::task::spawn_blocking(move || -> Result<Option<Snapshot>, String> {
        let db = data_dir.join(DB_FILE);
        let mut store = Store::open(&db).map_err(|e| e.to_string())?;
        let outcome =
            cartograph_index::git::update(&root, &mut store).map_err(|e| e.to_string())?;
        if !outcome.changed {
            return Ok(None);
        }
        let (graph, meta) = store.load_snapshot().map_err(|e| e.to_string())?;
        let history = store.load_history().map_err(|e| e.to_string())?;
        let symbols = store.load_symbols().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "symbols not loaded");
            Symbols::default()
        });
        let config = cartograph_query::Config::load(&root);
        Ok(Some(Snapshot::build_all(
            graph,
            &meta,
            next_generation,
            config,
            &history,
            &symbols,
        )))
    })
    .await;
    let ms = started.elapsed().as_millis() as u64;
    match res {
        Ok(Ok(Some(snap))) => {
            let generation = snap.generation;
            let counts = json!(snap.counts);
            state.set_snapshot(Arc::new(snap));
            state.emit(
                "index-finished",
                json!({
                    "generation": generation,
                    "ms": ms,
                    "changed": [],
                    "counts": counts,
                    "git": true,
                }),
            );
            tracing::info!(generation, ms, "git history updated");
        }
        Ok(Ok(None)) => tracing::debug!(ms, "git history unchanged"),
        Ok(Err(e)) => tracing::warn!(error = %e, "git history not updated"),
        Err(e) => tracing::warn!(error = %e, "git history task failed"),
    }
}

fn panic_text(e: tokio::task::JoinError) -> String {
    match e.try_into_panic() {
        Ok(p) => crate::panic_message(p.as_ref()),
        Err(e) => e.to_string(),
    }
}

/// The `index-progress` task of one run. Dropping it (e.g. when the worker is aborted)
/// aborts the task, so it never outlives its run.
struct Ticker {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Ticker {
    fn start(state: Arc<AppState>) -> Ticker {
        let (tx, rx) = tokio::sync::oneshot::channel();
        Ticker {
            stop: Some(tx),
            handle: Some(tokio::spawn(progress_loop(state, rx))),
        }
    }

    /// Stop the task and wait until it has ended.
    async fn stop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = h.await;
        }
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        if let Some(h) = &self.handle {
            h.abort();
        }
    }
}

async fn progress_loop(state: Arc<AppState>, mut stop: tokio::sync::oneshot::Receiver<()>) {
    let mut tick = tokio::time::interval(PROGRESS_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last = (u64::MAX, u64::MAX);
    loop {
        tokio::select! {
            _ = &mut stop => return,
            _ = tick.tick() => {}
        }
        let now = state.progress.get();
        if now != last && now.1 > 0 {
            state.emit("index-progress", json!({ "done": now.0, "total": now.1 }));
            last = now;
        }
    }
}
