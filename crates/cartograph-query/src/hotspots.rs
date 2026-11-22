//! `hotspots(within?)`: the code files that change most and are hardest to change, within
//! [`limits::HOTSPOTS`]. Score = churn (non-merge commits, renames followed) × indentation
//! complexity of the HEAD version (a language-independent proxy for nesting). Tests,
//! scripts, data and files ignored by `cartograph.toml` are left out.

use crate::budget::{Answer, clip, fit, limits};
use crate::graph::{FileIx, NodeRef};
use crate::history::date;
use crate::kind::FileKind;
use crate::text::num;
use crate::text::short;
use crate::{QueryError, Repo};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Hotspot {
    pub id: String,
    pub score: u64,
    pub commits: u32,
    /// Commits in the recent window.
    pub recent: u32,
    pub loc: u64,
    pub complexity: u64,
    pub authors: usize,
    /// Main author and their share of the commits (%).
    pub owner: Option<String>,
    pub owner_share: u32,
    pub last_change: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hotspots {
    /// The folder or file looked at (`repo:` for everything).
    pub scope: String,
    /// Why there is no history (`None` when there is).
    pub git: Option<String>,
    /// The history shown is kept from an earlier read: the last one failed (why).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_error: Option<String>,
    /// The history is partial (a shallow clone).
    pub partial: bool,
    pub commits: u64,
    pub newest: Option<String>,
    pub window_days: u32,
    /// Code files in scope with history.
    pub files: usize,
    pub items: Vec<Hotspot>,
    pub more: usize,
}

pub fn hotspots(repo: &Repo, within: Option<&str>) -> Result<Answer<Hotspots>, QueryError> {
    let g = &repo.graph;
    let scope = match within {
        Some(w) => repo.resolve(w)?,
        None => NodeRef::Dir(crate::graph::ROOT),
    };
    if let NodeRef::Ext(_) = scope {
        return Err(QueryError::BadArgument(
            "hotspots look at files and folders, not libraries".into(),
        ));
    }
    let status = repo.git.status.as_ref();
    let mut rows: Vec<Hotspot> = g
        .files_of(scope)
        .into_iter()
        .filter(|&f| {
            let file = &g.files[f as usize];
            file.kind == FileKind::Code && !repo.config.ignore.matches(&file.path)
        })
        .filter_map(|f: FileIx| {
            let h = repo.git.of(f)?;
            let total: u32 = h.authors.iter().map(|a| a.commits).sum();
            let owner = h.authors.first();
            Some(Hotspot {
                id: g.id(NodeRef::File(f)),
                score: u64::from(h.commits) * h.complexity,
                commits: h.commits,
                recent: h.recent,
                loc: g.files[f as usize].loc,
                complexity: h.complexity,
                authors: h.authors.len(),
                owner: owner.map(|a| a.name.clone()),
                owner_share: match (owner, total) {
                    (Some(a), t) if t > 0 => (a.commits * 100 + t / 2) / t,
                    _ => 0,
                },
                last_change: date(h.last_time),
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.commits.cmp(&a.commits))
            .then(a.id.cmp(&b.id))
    });
    let files = rows.len();
    let git = repo.git.unavailable();
    let git_error = repo.git.read_error();
    let commits = status.map_or(0, |s| s.commits);
    let newest = status.filter(|s| s.newest > 0).map(|s| date(s.newest));
    let window_days = status.map_or(0, |s| s.window_days);
    let partial = repo.git.shallow();
    let scope_id = g.id(scope);
    Ok(fit(
        limits::HOTSPOTS,
        &[20, 15, 12, 10, 8, 6, 5, 4, 3, 2, 1],
        |limit| Hotspots {
            scope: scope_id.clone(),
            git: git.clone(),
            git_error: git_error.clone(),
            partial,
            commits,
            newest: newest.clone(),
            window_days,
            files,
            items: rows.iter().take(limit).cloned().collect(),
            more: rows.len().saturating_sub(limit),
        },
        render,
    ))
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

fn render(h: &Hotspots) -> String {
    let mut s = format!("hotspots in {}", short(&h.scope));
    if let Some(why) = &h.git {
        s.push_str(&format!(": git history {why}\n"));
        return s;
    }
    s.push_str(&format!(
        " (churn × indentation complexity; {} commits up to {}, recent = last {} days{})\n",
        num(h.commits),
        h.newest.as_deref().unwrap_or("?"),
        h.window_days,
        if h.partial {
            "; shallow clone: partial history"
        } else {
            ""
        }
    ));
    if let Some(e) = &h.git_error {
        s.push_str(&format!(
            "git history {e}
"
        ));
    }
    if h.items.is_empty() {
        s.push_str("no code file here has git history\n");
        return s;
    }
    for x in &h.items {
        let owner = match &x.owner {
            Some(o) => format!(" ({} {}%)", clip(o, 40), x.owner_share),
            None => String::new(),
        };
        s.push_str(&format!(
            "  {} · score {} · {} ({} recent) · {} LOC · cx {} · {}{owner} · {}\n",
            clip(&short(&x.id), 120),
            num(x.score),
            plural(x.commits as usize, "commit"),
            x.recent,
            num(x.loc),
            num(x.complexity),
            plural(x.authors, "author"),
            x.last_change
        ));
    }
    if h.more > 0 {
        s.push_str(&format!(
            "  +{} more of {} code files with history\n",
            h.more, h.files
        ));
    }
    s
}
