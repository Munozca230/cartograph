//! Hidden coupling: two files that keep changing in the same commits (temporal coupling,
//! from git) although neither depends on the other, not even through other files. Such a
//! pair shares a contract the dependency graph does not show (a message format, a channel
//! name, duplicated logic), so changing one without the other is a likely bug.
//!
//! Only code files with import extraction take part (a lockfile or a stylesheet can never
//! have a dependency path, so it would always look "hidden"), never tests or ignored files.
//! In Go, Java and C#, files of one folder use each other without imports (one package or
//! namespace), so a pair inside one folder of those languages is left out.
//!
//! Thresholds (all in `[thresholds]`): shared commits at least the repo's Tukey fence over
//! every candidate pair and `coupling_min_shared`; coupling degree (shared commits over the
//! mean of both files' commits, code-maat's measure) at least `coupling_min_degree`; at most
//! `max_coupling` findings, strongest first.

use super::{Ctx, Finding, FindingKind, Severity};
use crate::algo::upper_fence;
use crate::graph::{FileIx, NodeRef};
use crate::history::{GitTable, date};
use crate::kind::FileKind;
use std::collections::HashMap;

/// Languages whose imports are extracted: only there can a missing path mean something.
const IMPORT_LANGS: &[&str] = &[
    "typescript",
    "tsx",
    "javascript",
    "jsx",
    "python",
    "rust",
    "csharp",
    "go",
    "java",
    "c",
    "cpp",
    "gdscript",
    "godot-scene",
    "godot-resource",
];
/// Languages where the files of one folder see each other without importing.
const SAME_FOLDER_LANGS: &[&str] = &["go", "java", "csharp"];

fn eligible(cx: &Ctx<'_>, f: usize) -> bool {
    let file = &cx.g.files[f];
    cx.active[f]
        && file.kind == FileKind::Code
        && file
            .lang
            .as_deref()
            .is_some_and(|l| IMPORT_LANGS.contains(&l))
}

/// Files reachable from `from` over every dependency (tests and type-only imports too).
fn reach(cx: &Ctx<'_>, from: FileIx) -> Vec<bool> {
    let g = cx.g;
    let mut seen = vec![false; g.files.len()];
    let mut stack = vec![from];
    seen[from as usize] = true;
    while let Some(u) = stack.pop() {
        for d in &g.out[u as usize] {
            if !seen[d.file as usize] {
                seen[d.file as usize] = true;
                stack.push(d.file);
            }
        }
    }
    seen
}

pub(super) fn hidden_coupling(cx: &Ctx<'_>, git: &GitTable) -> Vec<Finding> {
    let g = cx.g;
    let t = &cx.cfg.thresholds;
    if t.max_coupling == 0 || git.partners.len() != g.files.len() {
        return Vec::new();
    }
    // Every pair of eligible files with a shared commit, once.
    let mut cands: Vec<(FileIx, FileIx, u32, i64)> = Vec::new();
    for a in 0..g.files.len() {
        if !eligible(cx, a) {
            continue;
        }
        for p in &git.partners[a] {
            let b = p.file as usize;
            if b <= a || !eligible(cx, b) {
                continue;
            }
            let (fa, fb) = (&g.files[a], &g.files[b]);
            if fa.dir == fb.dir
                && fa.lang == fb.lang
                && fa
                    .lang
                    .as_deref()
                    .is_some_and(|l| SAME_FOLDER_LANGS.contains(&l))
            {
                continue;
            }
            cands.push((a as FileIx, p.file, p.together, p.last_time));
        }
    }
    let shared: Vec<f64> = cands.iter().map(|c| f64::from(c.2)).collect();
    let min_shared = upper_fence(&shared, t.fence).max(t.coupling_min_shared as f64);
    let commits = |f: FileIx| git.of(f).map_or(0, |h| h.co_commits);
    let degree = |a: FileIx, b: FileIx, n: u32| {
        let mean = (f64::from(commits(a)) + f64::from(commits(b))) / 2.0;
        if mean > 0.0 { f64::from(n) / mean } else { 0.0 }
    };
    let mut strong: Vec<(f64, FileIx, FileIx, u32, i64)> = cands
        .into_iter()
        .filter(|&(a, b, n, _)| {
            f64::from(n) >= min_shared && degree(a, b, n) >= t.coupling_min_degree
        })
        .map(|(a, b, n, last)| (degree(a, b, n), a, b, n, last))
        .collect();
    // Strongest first: more shared commits, then a higher degree.
    strong.sort_by(|x, y| {
        y.3.cmp(&x.3)
            .then(y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal))
            .then((x.1, x.2).cmp(&(y.1, y.2)))
    });
    let mut reached: HashMap<FileIx, Vec<bool>> = HashMap::new();
    let mut connected = |a: FileIx, b: FileIx| {
        reached.entry(a).or_insert_with(|| reach(cx, a))[b as usize]
            || reached.entry(b).or_insert_with(|| reach(cx, b))[a as usize]
    };
    let mut out = Vec::new();
    for (deg, a, b, n, last) in strong {
        if out.len() >= t.max_coupling {
            break;
        }
        if connected(a, b) {
            continue;
        }
        // Ids sorted by path, so the id does not depend on file order.
        let (a, b) = if g.path(a) <= g.path(b) {
            (a, b)
        } else {
            (b, a)
        };
        let (ia, ib) = (g.id(NodeRef::File(a)), g.id(NodeRef::File(b)));
        let (ca, cb) = (commits(a), commits(b));
        let pct = |x: u32| {
            if x == 0 {
                0.0
            } else {
                (f64::from(n) * 100.0 / f64::from(x)).round()
            }
        };
        let mut x = Finding::new(
            FindingKind::HiddenCoupling,
            Severity::Info,
            &format!("{ia}->{ib}"),
            format!(
                "hidden coupling: {} and {} changed together in {n} commits, no dependency either way",
                g.path(a),
                g.path(b)
            ),
        );
        x.node(ia);
        x.node(ib);
        x.line(format!(
            "{:.0}% of the {ca} commits of {} also change {}",
            pct(ca),
            g.path(a),
            g.files[b as usize].name
        ));
        x.line(format!(
            "{:.0}% of the {cb} commits of {} also change {}",
            pct(cb),
            g.path(b),
            g.files[a as usize].name
        ));
        x.line(format!(
            "last changed together {}; no import path from either one to the other",
            date(last)
        ));
        x.line(format!(
            "repo thresholds: ≥ {:.0} shared commits, degree ≥ {:.2}",
            min_shared.ceil(),
            t.coupling_min_degree
        ));
        x.metric("together", f64::from(n));
        x.metric("degree", (deg * 100.0).round() / 100.0);
        x.metric("commits_a", f64::from(ca));
        x.metric("commits_b", f64::from(cb));
        out.push(x);
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::findings::{FindingKind, analyze};
    use crate::graph::RepoGraph;
    use crate::history::GitTable;
    use cartograph_core::{
        CoChange, Confidence, Edge, EdgeKind, FileHistory, GitState, GitStatus, Graph, History,
        Node, NodeKind, node_id,
    };

    fn node(kind: NodeKind, path: &str, lang: Option<&str>) -> Node {
        Node {
            id: node_id(kind, path),
            kind,
            path: path.into(),
            name: path.rsplit('/').next().unwrap_or(path).into(),
            lang: lang.map(str::to_string),
            loc: 10,
            bytes: 10,
            hash: None,
            meta: serde_json::Value::Null,
        }
    }

    fn import(a: &str, b: &str) -> Edge {
        Edge::new(
            node_id(NodeKind::File, a),
            node_id(NodeKind::File, b),
            EdgeKind::Import,
            Confidence::Resolved,
        )
    }

    fn history(paths: &[&str], pairs: &[(&str, &str)]) -> History {
        History {
            status: Some(GitStatus {
                state: GitState::Ok,
                ..GitStatus::default()
            }),
            files: paths
                .iter()
                .map(|p| FileHistory {
                    path: (*p).into(),
                    commits: 10,
                    added: 0,
                    removed: 0,
                    recent: 0,
                    first_time: 0,
                    last_time: 0,
                    co_commits: 10,
                    complexity: 0,
                    authors: Vec::new(),
                })
                .collect(),
            pairs: pairs
                .iter()
                .map(|(a, b)| CoChange {
                    a: (*a).into(),
                    b: (*b).into(),
                    together: 9,
                    last_time: 0,
                })
                .collect(),
        }
    }

    #[test]
    fn only_pairs_with_no_path_either_way_among_code_files() {
        let paths = [
            "src/a.ts",
            "src/m.ts",
            "lib/b.ts",
            "src/c.ts",
            "lib/d.ts",
            "pkg/x.go",
            "pkg/y.go",
            "other/z.go",
            "src/e.ts",
            "src/e.test.ts",
            "src/style.css",
        ];
        let mut g = Graph::default();
        g.nodes.push(node(NodeKind::Repo, "", None));
        for p in paths {
            let lang = match p.rsplit('.').next() {
                Some("go") => "go",
                Some("css") => "css",
                _ => "typescript",
            };
            g.nodes.push(node(NodeKind::File, p, Some(lang)));
        }
        // a → m → b: connected through m.
        g.edges.push(import("src/a.ts", "src/m.ts"));
        g.edges.push(import("src/m.ts", "lib/b.ts"));
        let rg = RepoGraph::build(&g);
        let h = history(
            &paths,
            &[
                // Connected (the path runs a → b; b → a is checked too).
                ("lib/b.ts", "src/a.ts"),
                // Hidden.
                ("lib/d.ts", "src/c.ts"),
                // One Go package: its files see each other without imports.
                ("pkg/x.go", "pkg/y.go"),
                // Go across packages: hidden.
                ("other/z.go", "pkg/x.go"),
                // A test and a stylesheet never take part.
                ("src/e.test.ts", "src/e.ts"),
                ("src/c.ts", "src/style.css"),
            ],
        );
        let git = GitTable::build(&rg, &h);
        let ids: Vec<String> = analyze(&rg, &Config::default(), &git)
            .into_iter()
            .filter(|f| f.kind == FindingKind::HiddenCoupling)
            .map(|f| f.id)
            .collect();
        assert_eq!(
            ids,
            vec![
                "hidden_coupling:file:lib/d.ts->file:src/c.ts",
                "hidden_coupling:file:other/z.go->file:pkg/x.go",
            ]
        );
    }
}
