//! The git layer: raw history rows (the source of truth, appended as new commits
//! arrive) and the per-file aggregates rebuilt from them (churn, owners, co-change).
//!
//! Authors are personal data: only a display name and a hash of the email (for telling
//! people apart) are kept, never the email itself.

use serde::{Deserialize, Serialize};
use std::fmt;

/// How one commit changed one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    /// Moved from `old_path` (with or without edits).
    Renamed,
}

impl ChangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Added => "A",
            ChangeKind::Modified => "M",
            ChangeKind::Deleted => "D",
            ChangeKind::Renamed => "R",
        }
    }
}

impl fmt::Display for ChangeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ChangeKind {
    type Err = crate::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "A" => ChangeKind::Added,
            "M" => ChangeKind::Modified,
            "D" => ChangeKind::Deleted,
            "R" => ChangeKind::Renamed,
            other => {
                return Err(crate::Error::UnknownVariant {
                    ty: "ChangeKind",
                    value: other.to_string(),
                });
            }
        })
    }
}

/// One commit reachable from HEAD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommit {
    /// Processing order: parents always come before their children.
    pub seq: u32,
    /// Full hex id.
    pub id: String,
    /// Author time, Unix seconds.
    pub time: i64,
    /// Index into [`GitRaw::authors`].
    pub author: u32,
    /// Paths it changed in the whole repository (outside the indexed folder too); 0 for a
    /// merge, whose changes are not recorded.
    pub files: u32,
    pub merge: bool,
}

/// One author identity as written in commits (after `.mailmap`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitAuthor {
    pub id: u32,
    pub name: String,
    /// Short hash of the lowercased email: tells two people with one name apart, and joins
    /// one person writing two names.
    pub email_hash: String,
}

/// One path changed by one non-merge commit (paths relative to the indexed root).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitChange {
    pub seq: u32,
    pub path: String,
    /// The source of a rename.
    pub old_path: Option<String>,
    pub kind: ChangeKind,
    /// Lines added and removed; `None` for a binary file.
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

/// The raw history, as stored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitRaw {
    pub commits: Vec<GitCommit>,
    pub authors: Vec<GitAuthor>,
    pub changes: Vec<GitChange>,
}

/// One author's share of a file's commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorShare {
    pub name: String,
    pub commits: u32,
}

/// The history of one file that exists at HEAD, renames followed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileHistory {
    pub path: String,
    /// Non-merge commits that touched it.
    pub commits: u32,
    /// Lines added and removed over those commits (binary versions count 0).
    pub added: u64,
    pub removed: u64,
    /// Commits in the [`GitStatus::window_days`] up to the newest commit of the repo.
    pub recent: u32,
    pub first_time: i64,
    pub last_time: i64,
    /// Its commits that are not huge (see [`GitStatus::max_changeset`]): the base of
    /// co-change confidence.
    pub co_commits: u32,
    /// Indentation complexity of the HEAD version (sum of the indentation levels of its
    /// non-blank lines).
    pub complexity: u64,
    /// Authors, most commits first (ties by name).
    pub authors: Vec<AuthorShare>,
}

/// Two files (`a < b`) changed by the same commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoChange {
    pub a: String,
    pub b: String,
    /// Commits (not huge, not merges) that changed both.
    pub together: u32,
    pub last_time: i64,
}

/// What the last git pass found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GitState {
    /// History indexed.
    Ok,
    /// The folder is not inside a git repository.
    #[default]
    NoGit,
    /// The repository has no commit yet.
    NoCommits,
    /// Reading the history failed (the previous history, if any, is kept).
    Error,
}

/// Run metadata of the git layer (stored as JSON in the `git` meta key).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GitStatus {
    pub state: GitState,
    /// HEAD commit (full hex).
    pub head: Option<String>,
    /// Branch HEAD points at (`None` when detached).
    pub branch: Option<String>,
    pub detached: bool,
    /// A shallow clone: the history is partial.
    pub shallow: bool,
    /// The indexed root inside the repository's work tree (`""` = the whole repo).
    pub prefix: String,
    /// Commits reachable from HEAD, merges included.
    pub commits: u64,
    pub merges: u64,
    /// Distinct people (after joining names and email hashes).
    pub authors: u64,
    pub oldest: i64,
    pub newest: i64,
    /// Width of the "recent" window, in days, ending at `newest`.
    pub window_days: u32,
    /// Commits changing more files than this are left out of co-change.
    pub max_changeset: u32,
    /// `full`, `incremental` or `unchanged`.
    pub mode: String,
    /// Commits read in the last pass.
    pub scanned: u64,
    pub ms: u64,
    pub error: Option<String>,
}

/// The aggregated history the queries read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    /// `None` when the git layer never ran on this index.
    pub status: Option<GitStatus>,
    /// Sorted by path.
    pub files: Vec<FileHistory>,
    /// Sorted by `(a, b)`.
    pub pairs: Vec<CoChange>,
}

impl History {
    pub fn normalize(&mut self) {
        self.files.sort_by(|x, y| x.path.cmp(&y.path));
        self.pairs.sort_by(|x, y| (&x.a, &x.b).cmp(&(&y.a, &y.b)));
    }
}
