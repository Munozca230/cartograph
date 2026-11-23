//! `impact(nodes[])`: what breaks if these change, within [`limits::IMPACT`]. Transitive
//! dependents (tests apart), file by file when they all fit, else grouped by folder; the
//! **tests to run** (test files that reach any of the nodes, directly or transitively;
//! classified as in `kind.rs`), and the entry
//! points reached. Type-only dependencies (TypeScript `import type`…) count: changing a
//! type still breaks the build. What is reached only through them is marked `type_only`.
//! With git history, files that usually change with a target but are not reached through
//! dependencies are listed apart ("also usually changes with").

use crate::budget::{Answer, clip, fit, limits};
use crate::graph::{DirIx, FileIx, NodeRef};
use crate::kind::FileKind;
use crate::text::files;
use crate::text::short;
use crate::{QueryError, Repo};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Debug, Clone, Serialize)]
pub struct FolderImpact {
    pub dir: String,
    /// Dependent files in it (tests apart).
    pub files: usize,
    /// Of those, the ones that depend on a target directly.
    pub direct: usize,
    /// Of those, the ones that reach a target only through type-only imports.
    #[serde(skip_serializing_if = "is_zero")]
    pub type_only: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Serialize)]
pub struct TestHit {
    pub id: String,
    /// Dependency steps from the test to the nearest target (1 = imports it).
    pub hops: u32,
    /// It reaches the targets only through type-only imports.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub type_only: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Impact {
    pub targets: Vec<String>,
    /// Targets left out of `targets` (they still count).
    pub targets_more: usize,
    /// Files in the targets themselves.
    pub target_files: usize,
    /// Files other than tests that depend on a target, directly or not.
    pub dependents: usize,
    /// Of those, the ones that import a target.
    pub direct: usize,
    /// Of those, the ones that reach a target only through type-only imports (they break
    /// the build, not the run).
    #[serde(skip_serializing_if = "is_zero")]
    pub type_only: usize,
    pub folders: Vec<FolderImpact>,
    pub folders_more: usize,
    /// Every dependent file, nearest first, when they all fit the answer (a handful);
    /// empty otherwise, and `folders` sums them up.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<TestHit>,
    /// Test files that reach a target, nearest first.
    pub tests: Vec<TestHit>,
    pub tests_more: usize,
    /// Of those, the ones that import a target.
    pub tests_direct: usize,
    /// Test files in the whole repo (to tell "run these" from "run everything").
    pub tests_in_repo: usize,
    /// Folders holding the tests to run, most tests first (a test runner filter).
    pub test_folders: Vec<(String, usize)>,
    pub test_folders_more: usize,
    /// Entry points that reach a target.
    pub entries: Vec<String>,
    pub entries_more: usize,
    /// Files outside the impact that usually change with a target (git co-change).
    pub also_changes: Vec<crate::explain::CoPartner>,
    pub also_changes_more: usize,
}

/// Co-change partners worth naming: at least this many shared commits…
const CO_MIN_TOGETHER: u32 = 3;
/// …and at least this share of the target's commits.
const CO_MIN_CONFIDENCE: f64 = 0.3;

pub fn impact(repo: &Repo, nodes: &[String]) -> Result<Answer<Impact>, QueryError> {
    let g = &repo.graph;
    if nodes.is_empty() {
        return Err(QueryError::BadArgument(
            "impact needs at least one node".into(),
        ));
    }
    let mut targets = Vec::new();
    let mut start: Vec<FileIx> = Vec::new();
    for n in nodes {
        let r = repo.resolve(n)?;
        if let NodeRef::Ext(x) = r {
            // Changing a library affects its importers.
            start.extend(g.ext_in[x as usize].iter().copied());
        } else {
            start.extend(g.files_of(r));
        }
        targets.push(g.id(r));
    }
    start.sort_unstable();
    start.dedup();
    let target_set: BTreeSet<FileIx> = start.iter().copied().collect();
    // Reverse BFS: dist[f] = steps from f to the nearest target.
    let mut dist = vec![u32::MAX; g.files.len()];
    let mut q = VecDeque::new();
    for &f in &start {
        dist[f as usize] = 0;
        q.push_back(f);
    }
    while let Some(u) = q.pop_front() {
        for d in &g.inn[u as usize] {
            if dist[d.file as usize] == u32::MAX {
                dist[d.file as usize] = dist[u as usize] + 1;
                q.push_back(d.file);
            }
        }
    }
    // The same over run-time dependencies only: what is not reached here is reached only
    // through type-only imports.
    let mut runtime = vec![false; g.files.len()];
    for &f in &start {
        runtime[f as usize] = true;
        q.push_back(f);
    }
    while let Some(u) = q.pop_front() {
        for d in &g.inn[u as usize] {
            if !d.type_only && !runtime[d.file as usize] {
                runtime[d.file as usize] = true;
                q.push_back(d.file);
            }
        }
    }
    let typed = |f: FileIx| !runtime[f as usize];
    let is_test = |f: FileIx| g.files[f as usize].kind == FileKind::Test;
    let reached: Vec<FileIx> = (0..g.files.len() as FileIx)
        .filter(|&f| dist[f as usize] != u32::MAX)
        .collect();
    let dependents: Vec<FileIx> = reached
        .iter()
        .copied()
        .filter(|&f| !target_set.contains(&f) && !is_test(f))
        .collect();
    let direct = dependents
        .iter()
        .filter(|&&f| dist[f as usize] == 1)
        .count();
    let type_only = dependents.iter().filter(|&&f| typed(f)).count();
    let mut by: BTreeMap<DirIx, (usize, usize, usize)> = BTreeMap::new();
    for &f in &dependents {
        let e = by.entry(g.files[f as usize].dir).or_default();
        e.0 += 1;
        if dist[f as usize] == 1 {
            e.1 += 1;
        }
        if typed(f) {
            e.2 += 1;
        }
    }
    let mut nearest: Vec<(u32, FileIx)> =
        dependents.iter().map(|&f| (dist[f as usize], f)).collect();
    nearest.sort_by(|a, b| (a.0, g.path(a.1)).cmp(&(b.0, g.path(b.1))));
    let mut folders: Vec<(DirIx, usize, usize, usize)> =
        by.into_iter().map(|(d, (a, b, t))| (d, a, b, t)).collect();
    folders.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)).then(a.0.cmp(&b.0)));
    // Tests that reach a target (a target that is a test counts too, at 0 hops).
    let mut tests: Vec<(u32, FileIx)> = reached
        .iter()
        .copied()
        .filter(|&f| is_test(f))
        .map(|f| (dist[f as usize], f))
        .collect();
    tests.sort();
    let tests_direct = tests.iter().filter(|t| t.0 <= 1).count();
    let tests_in_repo = g.files.iter().filter(|f| f.kind == FileKind::Test).count();
    let mut test_dirs: BTreeMap<DirIx, usize> = BTreeMap::new();
    for &(_, f) in &tests {
        *test_dirs.entry(g.files[f as usize].dir).or_default() += 1;
    }
    let mut test_dirs: Vec<(usize, DirIx)> = test_dirs.into_iter().map(|(d, n)| (n, d)).collect();
    test_dirs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let entries: Vec<FileIx> = dependents
        .iter()
        .copied()
        .filter(|&f| crate::overview::root_entry(g, f, crate::graph::ROOT).is_some())
        .collect();
    // Git partners not already in the impact: (together, confidence %, file), strongest
    // first; a partner of several targets keeps its strongest link.
    let mut co: BTreeMap<FileIx, (u32, u32)> = BTreeMap::new();
    for &t in &start {
        for p in repo.git.partners.get(t as usize).into_iter().flatten() {
            let conf = repo.git.confidence(t, p);
            if dist[p.file as usize] != u32::MAX
                || p.together < CO_MIN_TOGETHER
                || conf < CO_MIN_CONFIDENCE
            {
                continue;
            }
            let e = co.entry(p.file).or_default();
            *e = (*e).max((p.together, (conf * 100.0).round() as u32));
        }
    }
    let mut co: Vec<(u32, u32, FileIx)> = co.into_iter().map(|(f, (n, c))| (n, c, f)).collect();
    co.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));

    Ok(fit(
        limits::IMPACT,
        &[30, 24, 18, 14, 10, 8, 6, 4, 3, 2, 1],
        |limit| Impact {
            targets: targets.iter().take(10).cloned().collect(),
            targets_more: targets.len().saturating_sub(10),
            target_files: start.len(),
            dependents: dependents.len(),
            direct,
            type_only,
            folders: folders
                .iter()
                .take(limit)
                .map(|&(d, files, direct, type_only)| FolderImpact {
                    dir: g.id(NodeRef::Dir(d)),
                    files,
                    direct,
                    type_only,
                })
                .collect(),
            folders_more: folders.len().saturating_sub(limit),
            files: if nearest.len() <= limit {
                nearest
                    .iter()
                    .map(|&(hops, f)| TestHit {
                        id: g.id(NodeRef::File(f)),
                        hops,
                        type_only: typed(f),
                    })
                    .collect()
            } else {
                Vec::new()
            },
            tests: tests
                .iter()
                .take(limit)
                .map(|&(hops, f)| TestHit {
                    id: g.id(NodeRef::File(f)),
                    hops,
                    type_only: typed(f),
                })
                .collect(),
            tests_more: tests.len().saturating_sub(limit),
            tests_direct,
            tests_in_repo,
            test_folders: test_dirs
                .iter()
                .take(limit.min(8))
                .map(|&(n, d)| (g.id(NodeRef::Dir(d)), n))
                .collect(),
            test_folders_more: test_dirs.len().saturating_sub(limit.min(8)),
            entries: entries
                .iter()
                .take(limit.min(8))
                .map(|&f| g.id(NodeRef::File(f)))
                .collect(),
            entries_more: entries.len().saturating_sub(limit.min(8)),
            also_changes: co
                .iter()
                .take(limit.min(6))
                .map(|&(together, confidence, f)| crate::explain::CoPartner {
                    id: g.id(NodeRef::File(f)),
                    together,
                    confidence,
                })
                .collect(),
            also_changes_more: co.len().saturating_sub(limit.min(6)),
        },
        render,
    ))
}

fn render(r: &Impact) -> String {
    let mut names: Vec<String> = r.targets.iter().map(|t| short(t)).collect();
    if r.targets_more > 0 {
        names.push(format!("+{} more", r.targets_more));
    }
    let total_tests = r.tests.len() + r.tests_more;
    let types = if r.type_only > 0 {
        format!(", {} through types only", r.type_only)
    } else {
        String::new()
    };
    let mut s = format!(
        "impact of {} ({}): {} depend on it ({} directly{types}) in {} folders, plus {} test files\n",
        clip(&names.join(", "), 300),
        files(r.target_files),
        files(r.dependents),
        r.direct,
        r.folders.len() + r.folders_more,
        total_tests
    );
    if !r.files.is_empty() {
        s.push_str("dependents (hops to the change):\n");
        for f in &r.files {
            let types = if f.type_only { " (type)" } else { "" };
            s.push_str(&format!(
                "  {} {}{types}\n",
                clip(&short(&f.id), 140),
                f.hops
            ));
        }
    } else if !r.folders.is_empty() {
        s.push_str("by folder (dependents, direct):\n");
        for f in &r.folders {
            let types = if f.type_only > 0 {
                format!(" ({} type)", f.type_only)
            } else {
                String::new()
            };
            s.push_str(&format!(
                "  {} {} {}{types}\n",
                clip(&short(&f.dir), 120),
                f.files,
                f.direct
            ));
        }
        if r.folders_more > 0 {
            s.push_str(&format!("  +{} more folders\n", r.folders_more));
        }
    }
    if total_tests == 0 {
        s.push_str("tests to run: none reach it (no test covers it through imports)\n");
    } else {
        let share = if r.tests_in_repo > 0 {
            format!(
                " of {} in the repo ({}%)",
                r.tests_in_repo,
                total_tests * 100 / r.tests_in_repo
            )
        } else {
            String::new()
        };
        s.push_str(&format!(
            "tests to run: {total_tests}{share}, {} import it directly (hops to the change):\n",
            r.tests_direct
        ));
        for t in &r.tests {
            let types = if t.type_only { " (type)" } else { "" };
            s.push_str(&format!(
                "  {} {}{types}\n",
                clip(&short(&t.id), 140),
                t.hops
            ));
        }
        if r.tests_more > 0 {
            s.push_str(&format!("  +{} more\n", r.tests_more));
        }
        let dirs: Vec<String> = r
            .test_folders
            .iter()
            .map(|(d, n)| format!("{} {n}", short(d)))
            .collect();
        s.push_str(&format!(
            "test folders: {}{}\n",
            dirs.join(", "),
            crate::budget::more(r.test_folders_more)
        ));
    }
    if !r.entries.is_empty() {
        let list: Vec<String> = r.entries.iter().map(|e| short(e)).collect();
        s.push_str(&format!(
            "entry points reached: {}{}\n",
            list.join(", "),
            crate::budget::more(r.entries_more)
        ));
    }
    if !r.also_changes.is_empty() {
        let list: Vec<String> = r
            .also_changes
            .iter()
            .map(|p| {
                format!(
                    "{} {}× ({}%)",
                    clip(&short(&p.id), 100),
                    p.together,
                    p.confidence
                )
            })
            .collect();
        s.push_str(&format!(
            "also usually changes with (git; not a dependent): {}{}\n",
            list.join(", "),
            crate::budget::more(r.also_changes_more)
        ));
    }
    s
}
