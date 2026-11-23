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
//!
//! In a workspace every member is its own repository: each has its own cursor, scope and
//! status (`git:<member>`), its commits carry its name and its paths start with its folder,
//! and the recent window ends at its own newest commit. The status under [`GIT_META`] then
//! sums the members up.

mod aggregate;
mod scan;
pub mod snapshot;

pub use aggregate::{Aggregated, MIN_PAIR, aggregate};
pub use scan::indent_complexity;

use crate::IndexError;
use cartograph_core::{
    GIT_META, GitAuthor, GitChange, GitCommit, GitRaw, GitState, GitStatus, History, Store,
    git_meta_key,
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

/// Bring the git layer of `store` up to date with the repository holding `root`, or, when
/// `root` holds a `cartograph-workspace.toml`, with each member's repository.
pub fn update(root: &Path, store: &mut Store) -> Result<GitOutcome, IndexError> {
    match cartograph_core::Workspace::find(root)? {
        None => update_member(root, "", store),
        Some(ws) => update_workspace(&ws, store),
    }
}

/// [`update`] for every member of `ws`, then the summary status (see [`summary`]).
fn update_workspace(
    ws: &cartograph_core::Workspace,
    store: &mut Store,
) -> Result<GitOutcome, IndexError> {
    // Members gone from the file (or the single repo this data dir held) leave no history.
    let tx = store.begin_replace()?;
    tx.retain_git_members(&ws.names())?;
    tx.commit()?;
    let mut outcomes = Vec::new();
    for m in &ws.members {
        outcomes.push((m.name.as_str(), update_member(&m.root, &m.name, store)?));
    }
    let mut st = summary(&outcomes);
    let previous = store.load_history()?.status;
    let read = outcomes.iter().any(|(_, o)| o.read);
    // Without a read, the pass numbers are those of the last one (as for a single repo).
    if !read && let Some(p) = &previous {
        st.mode = p.mode.clone();
        st.scanned = p.scanned;
        st.ms = p.ms;
    }
    let changed = outcomes.iter().any(|(_, o)| o.changed) || previous.as_ref() != Some(&st);
    if previous.as_ref() != Some(&st) {
        store.set_meta(GIT_META, &serde_json::to_string(&st)?)?;
    }
    Ok(GitOutcome {
        status: st,
        changed,
        read,
    })
}

/// The status of a workspace: `ok` when one member's history was read (else the first
/// member's state), counts summed, the oldest and newest commit of all, and the members'
/// errors. No head nor branch: each member has its own (`git:<member>`).
fn summary(outcomes: &[(&str, GitOutcome)]) -> GitStatus {
    let ok: Vec<&GitStatus> = outcomes
        .iter()
        .map(|(_, o)| &o.status)
        .filter(|s| s.state == GitState::Ok)
        .collect();
    let mut st = status(match (ok.is_empty(), outcomes.first()) {
        (false, _) => GitState::Ok,
        (true, Some((_, o))) => o.status.state,
        (true, None) => GitState::NoGit,
    });
    st.commits = ok.iter().map(|s| s.commits).sum();
    st.merges = ok.iter().map(|s| s.merges).sum();
    st.authors = ok.iter().map(|s| s.authors).sum();
    st.oldest = ok
        .iter()
        .map(|s| s.oldest)
        .filter(|&t| t > 0)
        .min()
        .unwrap_or(0);
    st.newest = ok.iter().map(|s| s.newest).max().unwrap_or(0);
    st.shallow = ok.iter().any(|s| s.shallow);
    st.scanned = outcomes.iter().map(|(_, o)| o.status.scanned).sum();
    st.ms = outcomes.iter().map(|(_, o)| o.status.ms).sum();
    st.mode = if outcomes.iter().any(|(_, o)| o.read) {
        "members".to_string()
    } else {
        String::new()
    };
    let errors: Vec<String> = outcomes
        .iter()
        .filter_map(|(m, o)| o.status.error.as_ref().map(|e| format!("{m}: {e}")))
        .collect();
    if !errors.is_empty() {
        st.error = Some(errors.join("; "));
    }
    st
}

/// The meta keys of one member's git layer (`""`: a single repo, the plain keys).
struct Keys {
    member: String,
    status: String,
    cursor: String,
    scope: String,
}

impl Keys {
    fn of(member: &str) -> Keys {
        let key = |base: &str| {
            if member.is_empty() {
                base.to_string()
            } else {
                format!("{base}:{member}")
            }
        };
        Keys {
            member: member.to_string(),
            status: git_meta_key(member),
            cursor: key(CURSOR_META),
            scope: key(SCOPE_META),
        }
    }

    fn previous(&self, store: &Store) -> Result<Option<GitStatus>, IndexError> {
        if self.member.is_empty() {
            return Ok(store.load_history()?.status);
        }
        Ok(store
            .get_meta(&self.status)?
            .and_then(|t| serde_json::from_str(&t).ok()))
    }

    /// A path of the member's repository as a path of the graph.
    fn path(&self, p: &str) -> String {
        if self.member.is_empty() {
            p.to_string()
        } else {
            format!("{}/{p}", self.member)
        }
    }
}

/// [`update`] for the repository holding `root`, the member `member` of a workspace
/// (`""`: a single repo).
fn update_member(root: &Path, member: &str, store: &mut Store) -> Result<GitOutcome, IndexError> {
    let started = Instant::now();
    let keys = Keys::of(member);
    let previous = keys.previous(store)?;
    tracing::debug!(
        ms = started.elapsed().as_millis() as u64,
        "git: previous status loaded"
    );
    let found = match scan::open(root) {
        Ok(f) => f,
        Err(e) => return record_error(store, &keys, previous, e, started),
    };
    let opened = match found {
        scan::Found::NoGit => {
            return record_state(store, &keys, previous, status(GitState::NoGit));
        }
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
        return record_state(store, &keys, previous, st);
    };
    st.head = Some(head.to_string());
    // The shallow boundary is part of the scope: rows read up to another boundary are not
    // this history.
    let scope = match &opened.boundary {
        None => format!("{LAYER_VERSION}|{}", opened.prefix),
        Some(b) => format!("{LAYER_VERSION}|{}|shallow:{b}", opened.prefix),
    };
    let cursor = store.get_meta(&keys.cursor)?;
    // Stores written before the boundary was in the scope only know the flag.
    let unshallowed = previous
        .as_ref()
        .is_some_and(|p| p.shallow && !opened.shallow);
    let same_scope =
        store.get_meta(&keys.scope)?.as_deref() == Some(scope.as_str()) && !unshallowed;
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
            store.set_meta(&keys.status, &serde_json::to_string(&same)?)?;
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
            Err(e) => return record_error(store, &keys, previous, e, started),
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
        Err(e) => return record_error(store, &keys, previous, e, started),
    };
    tracing::debug!(
        ms = started.elapsed().as_millis() as u64,
        commits = scanned.len(),
        "git: commits scanned"
    );
    // A member shares the tables with the others: its new rows are numbered after theirs,
    // and only its own rows are aggregated.
    let mut raw = if incremental {
        store.load_git_raw()?
    } else if member.is_empty() {
        GitRaw::default()
    } else {
        let mut all = store.load_git_raw()?;
        let mine: HashSet<u32> = all
            .commits
            .iter()
            .filter(|c| c.member == member)
            .map(|c| c.seq)
            .collect();
        all.commits.retain(|c| !mine.contains(&c.seq));
        all.changes.retain(|c| !mine.contains(&c.seq));
        all
    };
    let added = append(&mut raw, &scanned, &keys);
    let agg = if member.is_empty() {
        aggregate(&raw, WINDOW_DAYS, MAX_CHANGESET)
    } else {
        aggregate(&only_member(raw, member), WINDOW_DAYS, MAX_CHANGESET)
    };
    tracing::debug!(ms = started.elapsed().as_millis() as u64, "git: aggregated");
    // The repository's own paths: the member's folder is not part of them.
    let strip = |p: &str| -> String {
        match member {
            "" => p.to_string(),
            m => p
                .strip_prefix(m)
                .and_then(|r| r.strip_prefix('/'))
                .unwrap_or(p)
                .to_string(),
        }
    };
    let paths: HashSet<String> = agg.files.iter().map(|f| strip(&f.path)).collect();
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
        f.complexity = complexity.get(&strip(&f.path)).copied().unwrap_or(0);
    }
    st.ms = started.elapsed().as_millis() as u64;
    let history = History {
        status: Some(st.clone()),
        files,
        pairs: agg.pairs,
    };
    let tx = store.begin_replace()?;
    if !incremental {
        tx.clear_git_member(member)?;
    }
    tx.append_git_raw(&added)?;
    tx.write_member_history(member, &history)?;
    tx.set_meta(&keys.cursor, st.head.as_deref().unwrap_or_default())?;
    tx.set_meta(&keys.scope, &scope)?;
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

/// The rows of one member: its commits and their changes, with every author.
fn only_member(raw: GitRaw, member: &str) -> GitRaw {
    let seqs: HashSet<u32> = raw
        .commits
        .iter()
        .filter(|c| c.member == member)
        .map(|c| c.seq)
        .collect();
    GitRaw {
        commits: raw
            .commits
            .into_iter()
            .filter(|c| seqs.contains(&c.seq))
            .collect(),
        changes: raw
            .changes
            .into_iter()
            .filter(|c| seqs.contains(&c.seq))
            .collect(),
        authors: raw.authors,
    }
}

/// Append `scanned` (parents first) to `raw`, as commits of `keys.member` with its paths;
/// returns just the new rows.
fn append(raw: &mut GitRaw, scanned: &[scan::Scanned], keys: &Keys) -> GitRaw {
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
            member: keys.member.clone(),
        });
        new.changes.extend(s.changes.iter().map(|c| GitChange {
            seq,
            path: keys.path(&c.path),
            old_path: c.old_path.as_deref().map(|p| keys.path(p)),
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
    keys: &Keys,
    previous: Option<GitStatus>,
    st: GitStatus,
) -> Result<GitOutcome, IndexError> {
    let new_state = previous.as_ref().map(|p| p.state) != Some(st.state);
    let changed = previous
        .as_ref()
        .is_some_and(|p| matches!(p.state, GitState::Ok | GitState::Error));
    if new_state {
        let tx = store.begin_replace()?;
        tx.clear_git_member(&keys.member)?;
        tx.write_member_history(
            &keys.member,
            &History {
                status: Some(st.clone()),
                ..History::default()
            },
        )?;
        tx.set_meta(&keys.cursor, "")?;
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
    keys: &Keys,
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
    store.set_meta(&keys.status, &serde_json::to_string(&st)?)?;
    // The cursor is cleared so the next pass starts over.
    store.set_meta(&keys.cursor, "")?;
    Ok(GitOutcome {
        status: st,
        changed,
        read: false,
    })
}
