//! The git layer: per-file churn, last change, owners and co-change, kept up to date
//! by a commit cursor.
//!
//! [`update`] reads only the commits HEAD gained since the last pass (the cursor), appends
//! them to the raw tables and rebuilds the aggregates from every stored row. The cursor no
//! longer being an ancestor of HEAD (a rebase, a reset, another branch), a different layer
//! version or root, or a different shallow boundary (`git fetch --deepen` or `--unshallow`,
//! which leave HEAD alone) means a full rebuild. Reading the history never fails the index: a
//! problem is recorded in the status (and the previous history kept).
//!
//! Definitions (they match `git log --no-merges --full-history`):
//! - churn: non-merge commits whose diff with their first parent touches the file, renames
//!   followed (50 % similarity, as git);
//! - lines added and removed: git's line counts of those diffs (binary files count 0);
//! - recent: churn in the [`WINDOW_DAYS`] up to the newest commit (not up to today, so the
//!   numbers do not drift between runs);
//! - co-change: two files changed by one commit, commits of more than [`MAX_CHANGESET`]
//!   files and merges left out (code-maat's approach).

mod aggregate;
mod scan;
pub mod snapshot;

pub use aggregate::{Aggregated, MIN_PAIR, aggregate};
pub use scan::indent_complexity;

use crate::IndexError;
use cartograph_core::{
    GIT_META, GitAuthor, GitChange, GitCommit, GitRaw, GitState, GitStatus, History, Store,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

/// Width of the "recent" window, in days.
pub const WINDOW_DAYS: u32 = 90;
/// Commits changing more files than this are left out of co-change.
pub const MAX_CHANGESET: u32 = 50;
/// Bump when the stored rows change meaning: the next pass rebuilds everything.
const LAYER_VERSION: &str = "1";
const CURSOR_META: &str = "git_cursor";
const SCOPE_META: &str = "git_scope";

/// What a pass did.
#[derive(Debug, Clone)]
pub struct GitOutcome {
    pub status: GitStatus,
    /// What a reader sees changed: the stored history (new commits, a rebuild, a new state)
    /// or its status (another branch or flag, a new read error). Callers republish on it.
    pub changed: bool,
    /// This pass read commits (a full or incremental scan), so `status.mode`, `scanned` and
    /// `ms` describe it. Without a scan they are those of the last one, even when `changed`
    /// (only a flag moved).
    pub read: bool,
}

fn status(state: GitState) -> GitStatus {
    GitStatus {
        state,
        window_days: WINDOW_DAYS,
        max_changeset: MAX_CHANGESET,
        ..GitStatus::default()
    }
}

/// Bring the git layer of `store` up to date with the repository holding `root`.
pub fn update(root: &Path, store: &mut Store) -> Result<GitOutcome, IndexError> {
    let started = Instant::now();
    let previous = store.load_history()?.status;
    tracing::debug!(
        ms = started.elapsed().as_millis() as u64,
        "git: previous status loaded"
    );
    let found = match scan::open(root) {
        Ok(f) => f,
        Err(e) => return record_error(store, previous, e, started),
    };
    let opened = match found {
        scan::Found::NoGit => return record_state(store, previous, status(GitState::NoGit)),
        scan::Found::Repo(o) => o,
    };
    tracing::debug!(
        ms = started.elapsed().as_millis() as u64,
        "git: repository opened"
    );
    let mut st = status(GitState::Ok);
    st.branch = opened.branch.clone();
    st.detached = opened.detached;
    st.shallow = opened.shallow;
    st.prefix = opened.prefix.trim_end_matches('/').to_string();
    let Some(head) = opened.head else {
        st.state = GitState::NoCommits;
        return record_state(store, previous, st);
    };
    st.head = Some(head.to_string());
    // The shallow boundary is part of the scope: rows read up to another boundary are not
    // this history.
    let scope = match &opened.boundary {
        None => format!("{LAYER_VERSION}|{}", opened.prefix),
        Some(b) => format!("{LAYER_VERSION}|{}|shallow:{b}", opened.prefix),
    };
    let cursor = store.get_meta(CURSOR_META)?;
    // Stores written before the boundary was in the scope only know the flag.
    let unshallowed = previous
        .as_ref()
        .is_some_and(|p| p.shallow && !opened.shallow);
    let same_scope = store.get_meta(SCOPE_META)?.as_deref() == Some(scope.as_str()) && !unshallowed;
    if same_scope
        && cursor.as_deref() == Some(st.head.as_deref().unwrap_or_default())
        && let Some(prev) = &previous
        && prev.state == GitState::Ok
    {
        // Nothing new. The branch, detached and shallow flags may still have changed.
        let mut same = prev.clone();
        same.branch = st.branch.clone();
        same.detached = st.detached;
        same.shallow = st.shallow;
        let changed = same != *prev;
        if changed {
            store.set_meta(GIT_META, &serde_json::to_string(&same)?)?;
        }
        return Ok(GitOutcome {
            status: same,
            changed,
            read: false,
        });
    }
    let incremental = match (&cursor, same_scope) {
        (Some(c), true) => match scan::is_ancestor(&opened, c, head) {
            Ok(a) => a,
            Err(e) => return record_error(store, previous, e, started),
        },
        _ => false,
    };
    let hidden = if incremental {
        cursor
            .as_deref()
            .and_then(|c| gix::ObjectId::from_hex(c.as_bytes()).ok())
    } else {
        None
    };
    let scanned = match scan::scan(&opened, head, hidden) {
        Ok(s) => s,
        Err(e) => return record_error(store, previous, e, started),
    };
    tracing::debug!(
        ms = started.elapsed().as_millis() as u64,
        commits = scanned.len(),
        "git: commits scanned"
    );
    let mut raw = if incremental {
        store.load_git_raw()?
    } else {
        GitRaw::default()
    };
    let added = append(&mut raw, &scanned);
    let agg = aggregate(&raw, WINDOW_DAYS, MAX_CHANGESET);
    tracing::debug!(ms = started.elapsed().as_millis() as u64, "git: aggregated");
    let paths: HashSet<String> = agg.files.iter().map(|f| f.path.clone()).collect();
    let complexity = match scan::head_complexity(&opened, head, &paths) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "git: complexity of HEAD files not read");
            HashMap::new()
        }
    };
    st.mode = if incremental { "incremental" } else { "full" }.to_string();
    st.scanned = scanned.len() as u64;
    st.commits = agg.commits;
    st.merges = agg.merges;
    st.authors = agg.people;
    st.oldest = agg.oldest;
    st.newest = agg.newest;
    let mut files = agg.files;
    for f in &mut files {
        f.complexity = complexity.get(&f.path).copied().unwrap_or(0);
    }
    st.ms = started.elapsed().as_millis() as u64;
    let history = History {
        status: Some(st.clone()),
        files,
        pairs: agg.pairs,
    };
    let tx = store.begin_replace()?;
    if !incremental {
        tx.clear_git()?;
    }
    tx.append_git_raw(&added)?;
    tx.write_history(&history)?;
    tx.set_meta(CURSOR_META, st.head.as_deref().unwrap_or_default())?;
    tx.set_meta(SCOPE_META, &scope)?;
    tx.commit()?;
    tracing::info!(
        mode = %st.mode,
        scanned = st.scanned,
        commits = st.commits,
        files = history.files.len(),
        pairs = history.pairs.len(),
        ms = st.ms,
        "git history indexed"
    );
    Ok(GitOutcome {
        status: st,
        changed: true,
        read: true,
    })
}

/// Append `scanned` (parents first) to `raw`; returns just the new rows.
fn append(raw: &mut GitRaw, scanned: &[scan::Scanned]) -> GitRaw {
    let mut new = GitRaw::default();
    let mut ids: HashMap<(String, String), u32> = raw
        .authors
        .iter()
        .map(|a| ((a.name.clone(), a.email_hash.clone()), a.id))
        .collect();
    let mut next_author = raw.authors.iter().map(|a| a.id + 1).max().unwrap_or(0);
    let first = raw.commits.iter().map(|c| c.seq + 1).max().unwrap_or(0);
    for (seq, s) in (first..).zip(scanned) {
        let key = (s.name.clone(), s.email_hash.clone());
        let author = match ids.get(&key) {
            Some(&id) => id,
            None => {
                let id = next_author;
                next_author += 1;
                ids.insert(key, id);
                new.authors.push(GitAuthor {
                    id,
                    name: s.name.clone(),
                    email_hash: s.email_hash.clone(),
                });
                id
            }
        };
        new.commits.push(GitCommit {
            seq,
            id: s.id.clone(),
            time: s.time,
            author,
            files: s.files,
            merge: s.merge,
        });
        new.changes.extend(s.changes.iter().map(|c| GitChange {
            seq,
            path: c.path.clone(),
            old_path: c.old_path.clone(),
            kind: c.kind,
            added: c.added,
            removed: c.removed,
        }));
    }
    raw.authors.extend(new.authors.iter().cloned());
    raw.commits.extend(new.commits.iter().cloned());
    raw.changes.extend(new.changes.iter().cloned());
    new
}

/// Record a state without history (no repository, no commit): the stored history is
/// dropped, since it no longer describes this folder. `changed` only when there was history
/// to drop.
fn record_state(
    store: &mut Store,
    previous: Option<GitStatus>,
    st: GitStatus,
) -> Result<GitOutcome, IndexError> {
    let new_state = previous.as_ref().map(|p| p.state) != Some(st.state);
    let changed = previous
        .as_ref()
        .is_some_and(|p| matches!(p.state, GitState::Ok | GitState::Error));
    if new_state {
        let tx = store.begin_replace()?;
        tx.clear_git()?;
        tx.write_history(&History {
            status: Some(st.clone()),
            ..History::default()
        })?;
        tx.set_meta(CURSOR_META, "")?;
        tx.commit()?;
    }
    Ok(GitOutcome {
        status: st,
        changed,
        read: false,
    })
}

/// Record a failure, keeping the previous history rows. `changed` when the status readers
/// see is new (a first failure, or another error): they must say the last read failed. The
/// same error again is nothing new.
fn record_error(
    store: &mut Store,
    previous: Option<GitStatus>,
    error: String,
    started: Instant,
) -> Result<GitOutcome, IndexError> {
    tracing::warn!(%error, "git history not read");
    let changed = previous
        .as_ref()
        .is_none_or(|p| p.state != GitState::Error || p.error.as_deref() != Some(&error));
    let mut st = previous.unwrap_or_else(|| status(GitState::Error));
    st.state = GitState::Error;
    st.error = Some(error);
    st.ms = started.elapsed().as_millis() as u64;
    store.set_meta(GIT_META, &serde_json::to_string(&st)?)?;
    // The cursor is cleared so the next pass starts over.
    store.set_meta(CURSOR_META, "")?;
    Ok(GitOutcome {
        status: st,
        changed,
        read: false,
    })
}
