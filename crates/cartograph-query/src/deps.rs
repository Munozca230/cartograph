//! `deps(node, dir=out|in|both, depth=1..3, level=file|folder)`: the dependency tree of a
//! node within [`limits::DEPS`], with counted ellipses ("+12 more").
//!
//! At file level the tree's nodes are files; a folder as the start stands for every file
//! under it, and only edges crossing its border count. At folder level the nodes are the
//! folders that directly hold the files, and an edge's weight is the number of file pairs.

use crate::budget::{Answer, clip, fit, limits};
use crate::graph::{FileIx, NodeRef, RepoGraph};
use crate::text::short;
use crate::{QueryError, Repo};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Out,
    In,
    Both,
}

impl std::str::FromStr for Direction {
    type Err = QueryError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "out" | "uses" => Ok(Direction::Out),
            "in" | "used-by" | "used_by" => Ok(Direction::In),
            "both" => Ok(Direction::Both),
            o => Err(QueryError::BadArgument(format!(
                "dir must be out, in or both, got {o:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    File,
    Folder,
}

impl std::str::FromStr for Level {
    type Err = QueryError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "file" | "files" => Ok(Level::File),
            "folder" | "folders" | "dir" | "dirs" => Ok(Level::Folder),
            o => Err(QueryError::BadArgument(format!(
                "level must be file or folder, got {o:?}"
            ))),
        }
    }
}

/// One node of a dependency tree.
#[derive(Debug, Clone, Serialize)]
pub struct Branch {
    pub id: String,
    /// File pairs behind a folder edge; import line for a file edge from a single file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Already shown higher in the tree (not expanded again).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub seen: bool,
    /// Every dependency behind this link is type-only (TypeScript `import type`…): it does
    /// not load at run time, but changing the types still breaks the build.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub type_only: bool,
    pub children: Vec<Branch>,
    /// Children left out.
    #[serde(skip_serializing_if = "is_zero")]
    pub more: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Serialize)]
pub struct Deps {
    pub id: String,
    pub level: Level,
    pub depth: u8,
    /// What it uses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uses: Option<Vec<Branch>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uses_more: Option<usize>,
    /// What uses it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_by: Option<Vec<Branch>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_by_more: Option<usize>,
    /// Libraries (direction out only).
    pub libraries: Vec<String>,
    pub libraries_more: usize,
}

/// Neighbours of a tree node: (node, weight, line, type-only).
type Step = (Key, usize, Option<u32>, bool);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Key {
    File(FileIx),
    Dir(u32),
}

struct Walker<'a> {
    g: &'a RepoGraph,
    level: Level,
    out: bool,
    /// The start's files (edges inside them do not count).
    inside: BTreeSet<FileIx>,
}

impl Walker<'_> {
    fn id(&self, k: Key) -> String {
        match k {
            Key::File(f) => self.g.id(NodeRef::File(f)),
            Key::Dir(d) => self.g.id(NodeRef::Dir(d)),
        }
    }

    fn files(&self, k: Key) -> Vec<FileIx> {
        match k {
            Key::File(f) => vec![f],
            Key::Dir(d) => self.g.dirs[d as usize].files.clone(),
        }
    }

    /// Neighbours of a set of files, heaviest first.
    fn next(&self, from: &[FileIx]) -> Vec<Step> {
        let g = self.g;
        let mut files: BTreeMap<FileIx, (usize, Option<u32>, bool)> = BTreeMap::new();
        for &f in from {
            let list = if self.out {
                &g.out[f as usize]
            } else {
                &g.inn[f as usize]
            };
            for d in list {
                // At folder level a folder stands for its own files: links inside it are
                // not dependencies of it.
                if self.inside.contains(&d.file)
                    || (self.level == Level::Folder
                        && g.files[d.file as usize].dir == g.files[f as usize].dir)
                {
                    continue;
                }
                let line = if self.out {
                    g.site(f, d.file)
                } else {
                    g.site(d.file, f)
                }
                .map(|s| s.line);
                let e = files.entry(d.file).or_insert((0, line, true));
                e.0 += 1;
                e.2 &= d.type_only;
            }
        }
        let single = from.len() == 1;
        let mut steps: Vec<Step> = match self.level {
            Level::File => files
                .into_iter()
                .map(|(f, (w, line, t))| (Key::File(f), w, if single { line } else { None }, t))
                .collect(),
            Level::Folder => {
                let mut by: BTreeMap<u32, (usize, bool)> = BTreeMap::new();
                for (f, (w, _, t)) in files {
                    let e = by.entry(g.files[f as usize].dir).or_insert((0, true));
                    e.0 += w;
                    e.1 &= t;
                }
                by.into_iter()
                    .map(|(d, (w, t))| (Key::Dir(d), w, None, t))
                    .collect()
            }
        };
        let fan = |k: &Key| match k {
            Key::File(f) => g.fan_in(*f),
            Key::Dir(_) => 0,
        };
        steps.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then(fan(&b.0).cmp(&fan(&a.0)))
                .then(a.0.cmp(&b.0))
        });
        steps
    }

    fn tree(
        &self,
        start: &[FileIx],
        start_key: Option<Key>,
        depth: u8,
        limit: usize,
    ) -> (Vec<Branch>, usize) {
        let mut seen: BTreeSet<Key> = BTreeSet::new();
        if let Some(k) = start_key {
            seen.insert(k);
        }
        let steps = self.next(start);
        let more = steps.len().saturating_sub(limit);
        let top: Vec<Step> = steps.into_iter().take(limit).collect();
        for s in &top {
            seen.insert(s.0);
        }
        let mut out = Vec::new();
        for s in top {
            out.push(self.branch(s, depth.saturating_sub(1), limit, &mut seen));
        }
        (out, more)
    }

    fn branch(
        &self,
        (k, w, line, type_only): Step,
        depth: u8,
        limit: usize,
        seen: &mut BTreeSet<Key>,
    ) -> Branch {
        let mut b = Branch {
            id: self.id(k),
            weight: (self.level == Level::Folder || w > 1).then_some(w),
            line,
            seen: false,
            type_only,
            children: Vec::new(),
            more: 0,
        };
        if depth == 0 {
            return b;
        }
        // Children get a smaller share of the budget the deeper they are.
        let child_limit = (limit / 2).max(1);
        let steps: Vec<Step> = self.next(&self.files(k));
        let mut shown = 0;
        for s in steps {
            if shown >= child_limit {
                b.more += 1;
                continue;
            }
            shown += 1;
            if seen.contains(&s.0) {
                b.children.push(Branch {
                    id: self.id(s.0),
                    weight: None,
                    line: s.2,
                    seen: true,
                    type_only: s.3,
                    children: Vec::new(),
                    more: 0,
                });
                continue;
            }
            seen.insert(s.0);
            b.children
                .push(self.branch(s, depth - 1, child_limit, seen));
        }
        b
    }
}

pub fn deps(
    repo: &Repo,
    node: &str,
    dir: Direction,
    depth: u8,
    level: Level,
) -> Result<Answer<Deps>, QueryError> {
    let g = &repo.graph;
    let n = repo.resolve(node)?;
    if !(1..=3).contains(&depth) {
        return Err(QueryError::BadArgument(format!(
            "depth must be 1, 2 or 3, got {depth}"
        )));
    }
    let (start, inside, start_key): (Vec<FileIx>, BTreeSet<FileIx>, Option<Key>) = match n {
        NodeRef::File(f) => match level {
            Level::File => (vec![f], BTreeSet::from([f]), Some(Key::File(f))),
            Level::Folder => {
                let d = g.files[f as usize].dir;
                let fs = g.dirs[d as usize].files.clone();
                (vec![f], fs.into_iter().collect(), Some(Key::Dir(d)))
            }
        },
        NodeRef::Dir(d) => {
            let fs = g.files_under(d);
            let inside = fs.iter().copied().collect();
            (fs, inside, None)
        }
        NodeRef::Ext(x) => {
            // A library has no dependencies; what uses it is its importers.
            let fs = g.ext_in[x as usize].clone();
            if dir == Direction::Out {
                return Err(QueryError::BadArgument(format!(
                    "{} is a library: it has no dependencies in the repo; try --dir in",
                    g.label(n)
                )));
            }
            (fs.clone(), BTreeSet::new(), None)
        }
    };
    let mut libs: BTreeMap<u32, usize> = BTreeMap::new();
    if dir != Direction::In {
        for &f in &start {
            for &x in &g.ext_out[f as usize] {
                *libs.entry(x).or_default() += 1;
            }
        }
    }
    let mut libs: Vec<(usize, u32)> = libs.into_iter().map(|(x, c)| (c, x)).collect();
    libs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let is_ext = matches!(n, NodeRef::Ext(_));

    Ok(fit(
        limits::DEPS,
        &[24, 18, 14, 10, 8, 6, 4, 3, 2, 1],
        |limit| {
            let walk = |out: bool| {
                let w = Walker {
                    g,
                    level,
                    out,
                    inside: inside.clone(),
                };
                if is_ext && !out {
                    // The importers are the first level.
                    let mut steps: Vec<Step> = start
                        .iter()
                        .map(|&f| (Key::File(f), 1, None, false))
                        .collect();
                    if level == Level::Folder {
                        let mut by: BTreeMap<u32, usize> = BTreeMap::new();
                        for &f in &start {
                            *by.entry(g.files[f as usize].dir).or_default() += 1;
                        }
                        steps = by
                            .into_iter()
                            .map(|(d, c)| (Key::Dir(d), c, None, false))
                            .collect();
                        steps.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                    }
                    let more = steps.len().saturating_sub(limit);
                    let mut seen = BTreeSet::new();
                    let branches = steps
                        .into_iter()
                        .take(limit)
                        .map(|s| w.branch(s, depth - 1, limit, &mut seen))
                        .collect();
                    return (branches, more);
                }
                w.tree(&start, start_key, depth, limit)
            };
            let (uses, uses_more) = if dir != Direction::In {
                let (a, b) = walk(true);
                (Some(a), Some(b))
            } else {
                (None, None)
            };
            let (used_by, used_by_more) = if dir != Direction::Out {
                let (a, b) = walk(false);
                (Some(a), Some(b))
            } else {
                (None, None)
            };
            Deps {
                id: g.id(n),
                level,
                depth,
                uses,
                uses_more,
                used_by,
                used_by_more,
                libraries: libs
                    .iter()
                    .take(limit)
                    .map(|&(_, x)| g.externals[x as usize].id.clone())
                    .collect(),
                libraries_more: libs.len().saturating_sub(limit),
            }
        },
        render,
    ))
}

fn render_tree(s: &mut String, branches: &[Branch], indent: usize) {
    for b in branches {
        let mut line = format!("{}{}", "  ".repeat(indent), clip(&short(&b.id), 120));
        if let Some(l) = b.line {
            line.push_str(&format!(" :{l}"));
        }
        if let Some(w) = b.weight {
            line.push_str(&format!(" ({w})"));
        }
        if b.type_only {
            line.push_str(" (type)");
        }
        if b.seen {
            line.push_str(" (seen)");
        }
        s.push_str(&line);
        s.push('\n');
        render_tree(s, &b.children, indent + 1);
        if b.more > 0 {
            s.push_str(&format!("{}+{} more\n", "  ".repeat(indent + 1), b.more));
        }
    }
}

fn render(d: &Deps) -> String {
    let mut s = format!(
        "{} · {} level · depth {} (:N = import line; (N) = file pairs; a folder = its own files)\n",
        short(&d.id),
        match d.level {
            Level::File => "file",
            Level::Folder => "folder",
        },
        d.depth
    );
    if let Some(u) = &d.uses {
        if u.is_empty() {
            s.push_str("uses: nothing in the repo\n");
        } else {
            s.push_str("uses:\n");
            render_tree(&mut s, u, 1);
            if let Some(m) = d.uses_more.filter(|&m| m > 0) {
                s.push_str(&format!("  +{m} more\n"));
            }
        }
    }
    if let Some(u) = &d.used_by {
        if u.is_empty() {
            s.push_str("used by: nothing in the repo\n");
        } else {
            s.push_str("used by:\n");
            render_tree(&mut s, u, 1);
            if let Some(m) = d.used_by_more.filter(|&m| m > 0) {
                s.push_str(&format!("  +{m} more\n"));
            }
        }
    }
    if !d.libraries.is_empty() {
        let list: Vec<String> = d.libraries.iter().map(|x| short(x)).collect();
        s.push_str(&format!(
            "libraries: {}{}\n",
            list.join(", "),
            crate::budget::more(d.libraries_more)
        ));
    }
    s
}
