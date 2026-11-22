//! The pending index run: why it happens ([`Reason`]) and the one job that requests merge
//! into ([`Scheduler`]). A leaf that the app state holds and the worker
//! ([`crate::indexer`], which re-exports these) takes jobs from.

use std::collections::BTreeSet;
use std::sync::Mutex;
use tokio::sync::Notify;

/// Why a run happens. When several merge, the highest wins (`Manual` > `Startup` > `Watch`
/// > `Git`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    /// Only git changed (a commit, a checkout, a fetch that deepened a shallow clone): the
    /// files are not indexed again, only the history is brought up to date. Merged with
    /// any other reason, the run is a full one (which updates the history too).
    Git,
    Watch,
    Startup,
    Manual,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Git => "git",
            Reason::Watch => "watch",
            Reason::Startup => "startup",
            Reason::Manual => "manual",
        }
    }
}

/// A pending run.
#[derive(Debug, Clone)]
pub struct Job {
    pub reason: Reason,
    /// Repo-relative paths reported by the watcher.
    pub changed: BTreeSet<String>,
}

/// At most one pending job, plus a wake-up for the worker.
#[derive(Default)]
pub struct Scheduler {
    pending: Mutex<Option<Job>>,
    notify: Notify,
}

impl Scheduler {
    /// Ask for a run. Callable from any thread (the watcher calls it from its own).
    pub fn request(&self, reason: Reason, changed: impl IntoIterator<Item = String>) {
        {
            let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            let job = p.get_or_insert_with(|| Job {
                reason,
                changed: BTreeSet::new(),
            });
            job.reason = job.reason.max(reason);
            job.changed.extend(changed);
        }
        self.notify.notify_one();
    }

    /// Whether a job is waiting.
    pub fn has_pending(&self) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Take the pending job, if any, without waiting. With [`Scheduler::wait`] it lets a
    /// caller take a job and record that it did under one lock of its own (see the MCP
    /// engine's freshness state).
    pub fn take(&self) -> Option<Job> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Wait until a job may be pending (a request since the last [`Scheduler::take`] that
    /// found nothing returns at once).
    pub async fn wait(&self) {
        self.notify.notified().await;
    }

    /// Wait for the next job.
    pub async fn next(&self) -> Job {
        loop {
            if let Some(job) = self.take() {
                return job;
            }
            self.wait().await;
        }
    }
}
