//! The git layer as the queries see it: each file's history and co-change partners, keyed
//! by [`FileIx`]. Files of the graph with no stored history (new, untracked or outside the
//! indexed root) have none; history of paths the graph does not know is dropped.

use crate::graph::{FileIx, RepoGraph};
use cartograph_core::{FileHistory, GitState, GitStatus, History};

/// A file changed together with another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Partner {
    pub file: FileIx,
    /// Commits (not huge, not merges) that changed both.
    pub together: u32,
    pub last_time: i64,
}

#[derive(Debug, Clone, Default)]
pub struct GitTable {
    /// `None` when the git layer never ran on this index.
    pub status: Option<GitStatus>,
    /// By file.
    pub files: Vec<Option<FileHistory>>,
    /// By file, most shared commits first.
    pub partners: Vec<Vec<Partner>>,
}

impl GitTable {
    pub fn build(g: &RepoGraph, h: &History) -> GitTable {
        let mut files: Vec<Option<FileHistory>> = vec![None; g.files.len()];
        for fh in &h.files {
            if let Some(f) = g.file_by_path(&fh.path) {
                files[f as usize] = Some(fh.clone());
            }
        }
        let mut partners: Vec<Vec<Partner>> = vec![Vec::new(); g.files.len()];
        for p in &h.pairs {
            let (Some(a), Some(b)) = (g.file_by_path(&p.a), g.file_by_path(&p.b)) else {
                continue;
            };
            partners[a as usize].push(Partner {
                file: b,
                together: p.together,
                last_time: p.last_time,
            });
            partners[b as usize].push(Partner {
                file: a,
                together: p.together,
                last_time: p.last_time,
            });
        }
        for list in &mut partners {
            list.sort_by(|x, y| y.together.cmp(&x.together).then(x.file.cmp(&y.file)));
        }
        GitTable {
            status: h.status.clone(),
            files,
            partners,
        }
    }

    /// Whether history was read (the layer ran on a git repository with commits).
    pub fn indexed(&self) -> bool {
        self.status
            .as_ref()
            .is_some_and(|s| matches!(s.state, GitState::Ok | GitState::Error))
            && self.files.iter().any(Option::is_some)
    }

    pub fn of(&self, f: FileIx) -> Option<&FileHistory> {
        self.files.get(f as usize).and_then(Option::as_ref)
    }

    /// Share of `f`'s commits that also changed its partner (association-rule confidence,
    /// "when f changes, the partner changes too in this share of its commits").
    pub fn confidence(&self, f: FileIx, p: &Partner) -> f64 {
        match self.of(f) {
            Some(h) if h.co_commits > 0 => f64::from(p.together) / f64::from(h.co_commits),
            _ => 0.0,
        }
    }

    /// Whether the history is partial (a shallow clone).
    pub fn shallow(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.shallow)
    }

    /// Why there is no history to show, in a few words (`None` when there is some, even
    /// if the last read failed: see [`GitTable::read_error`]).
    pub fn unavailable(&self) -> Option<String> {
        if self.indexed() {
            return None;
        }
        missing(self.status.as_ref())
    }

    /// The history shown is the one kept from an earlier read, because the last read
    /// failed: why, in a few words.
    pub fn read_error(&self) -> Option<String> {
        self.read_failure().map(|e| {
            format!(
                "not updated, the last git read failed: {e}; showing the history kept from \
                 before"
            )
        })
    }

    /// The error of the last git read, when it failed and an earlier history is kept
    /// (clipped).
    pub fn read_failure(&self) -> Option<String> {
        let s = self.status.as_ref()?;
        (s.state == GitState::Error && self.indexed())
            .then(|| crate::budget::clip(s.error.as_deref().unwrap_or("unknown error"), 160))
    }
}

/// Fewest authors who together made at least `share` of the commits (`authors` sorted by
/// commits, most first). 0 without authors.
pub fn bus_factor(authors: &[cartograph_core::AuthorShare], share: f64) -> usize {
    let total: u64 = authors.iter().map(|a| u64::from(a.commits)).sum();
    if total == 0 {
        return 0;
    }
    let mut sum = 0u64;
    for (i, a) in authors.iter().enumerate() {
        sum += u64::from(a.commits);
        if sum as f64 >= share * total as f64 {
            return i + 1;
        }
    }
    authors.len()
}

/// `YYYY-MM-DD` (UTC) of a Unix time.
pub fn date(unix: i64) -> String {
    // Howard Hinnant's civil-from-days.
    let z = unix.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Why there is no history, in a few words (`None` when there is).
pub fn missing(status: Option<&GitStatus>) -> Option<String> {
    let Some(s) = status else {
        return Some("not indexed (run `cartograph index` without --no-git)".into());
    };
    match s.state {
        GitState::Ok => None,
        GitState::NoGit => Some("not a git repository".into()),
        GitState::NoCommits => Some("no commits yet".into()),
        GitState::Error => Some(format!(
            "not read: {}",
            crate::budget::clip(s.error.as_deref().unwrap_or("unknown error"), 160)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartograph_core::AuthorShare;

    #[test]
    fn dates_and_bus_factor() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_735_689_600), "2025-01-01");
        assert_eq!(date(1_709_164_800), "2024-02-29");
        let a = |n: u32| AuthorShare {
            name: format!("a{n}"),
            commits: n,
        };
        let authors = [a(6), a(3), a(1)];
        assert_eq!(bus_factor(&authors, 0.5), 1);
        assert_eq!(bus_factor(&authors, 0.8), 2);
        assert_eq!(bus_factor(&[], 0.5), 0);
    }
}
