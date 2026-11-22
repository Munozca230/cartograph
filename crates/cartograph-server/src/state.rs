//! Shared server state: the current snapshot (swapped atomically), run status, progress
//! and the event bus behind `/api/events`.

use crate::scheduler::Scheduler;
use crate::snapshot::Snapshot;
use cartograph_index::Progress;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use tokio::sync::broadcast;

/// One server-sent event: its name and JSON payload.
#[derive(Debug, Clone)]
pub struct ServerEvent {
    pub name: &'static str,
    pub data: Value,
}

#[derive(Debug, Default, Clone)]
struct Status {
    running: bool,
    error: Option<String>,
}

/// What `/api/meta` reports about indexing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// No snapshot yet and no failure: the first index is running (or about to).
    Indexing,
    Ready,
    /// The last run failed.
    Error,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Indexing => "indexing",
            Phase::Ready => "ready",
            Phase::Error => "error",
        }
    }
}

pub struct AppState {
    pub repo_name: String,
    /// Normalized absolute repo root (no `\\?\` prefix).
    pub root: PathBuf,
    /// `root` with forward slashes, as shown in `/api/meta`.
    pub root_display: String,
    /// Normalized absolute data dir.
    pub data_dir: PathBuf,
    pub progress: Arc<Progress>,
    pub scheduler: Scheduler,
    snapshot: RwLock<Option<Arc<Snapshot>>>,
    status: Mutex<Status>,
    watching: AtomicBool,
    /// The port the server listens on (0 until bound); requests must name it in `Host`.
    port: AtomicU16,
    events: broadcast::Sender<ServerEvent>,
}

impl AppState {
    pub fn new(repo_name: String, root: PathBuf, data_dir: PathBuf) -> AppState {
        let (events, _) = broadcast::channel(256);
        AppState {
            repo_name,
            root_display: root.to_string_lossy().replace('\\', "/"),
            root,
            data_dir,
            progress: Arc::new(Progress::default()),
            scheduler: Scheduler::default(),
            snapshot: RwLock::new(None),
            status: Mutex::new(Status::default()),
            watching: AtomicBool::new(false),
            port: AtomicU16::new(0),
            events,
        }
    }

    fn status(&self) -> MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current snapshot. Never blocks on indexing: the lock is only held to clone or
    /// swap the `Arc`.
    pub fn snapshot(&self) -> Option<Arc<Snapshot>> {
        self.snapshot
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_snapshot(&self, s: Arc<Snapshot>) {
        *self.snapshot.write().unwrap_or_else(|e| e.into_inner()) = Some(s);
    }

    /// Generation of the current snapshot (0 before the first index).
    pub fn generation(&self) -> u64 {
        self.snapshot().map_or(0, |s| s.generation)
    }

    pub fn phase(&self) -> Phase {
        let st = self.status();
        if st.error.is_some() {
            Phase::Error
        } else if self.snapshot().is_none() {
            Phase::Indexing
        } else {
            Phase::Ready
        }
    }

    pub fn error(&self) -> Option<String> {
        self.status().error.clone()
    }

    pub fn running(&self) -> bool {
        self.status().running
    }

    pub fn set_running(&self) {
        self.status().running = true;
    }

    /// End of a run: clears `running` and records the outcome.
    pub fn finish(&self, error: Option<String>) {
        let mut st = self.status();
        st.running = false;
        st.error = error;
    }

    /// `{done, total}` while a run is reading files, else `null`.
    pub fn progress_json(&self) -> Value {
        let (done, total) = self.progress.get();
        if self.running() && total > 0 {
            json!({ "done": done, "total": total })
        } else {
            Value::Null
        }
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::Relaxed)
    }

    pub fn set_port(&self, port: u16) {
        self.port.store(port, Ordering::Relaxed);
    }

    pub fn watching(&self) -> bool {
        self.watching.load(Ordering::Relaxed)
    }

    pub fn set_watching(&self, on: bool) {
        self.watching.store(on, Ordering::Relaxed);
    }

    /// Broadcast an event to every `/api/events` client (none listening is fine).
    pub fn emit(&self, name: &'static str, data: Value) {
        tracing::debug!(event = name, %data, "event");
        let _ = self.events.send(ServerEvent { name, data });
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.events.subscribe()
    }
}
