//! Cycles (Tarjan SCC) at file level and at folder level. Each strongly connected component
//! is one finding; its evidence is the smallest cycle inside it.
//!
//! Folder level compares *sibling* folders: an edge `a → b` between files counts for the two
//! children of their lowest common folder that hold them. So `src/ui/ ↔ src/server/` is a
//! folder cycle, while a parent folder and its own subfolder never are (a file directly in
//! the parent is not a sibling folder).

use super::{Ctx, Finding, FindingKind, Severity};
use crate::algo::{Adj, shortest_cycle, tarjan};
use crate::config::NoCycles;
use crate::graph::{DirIx, FileIx, NodeRef, RepoGraph};
use std::collections::BTreeMap;

/// Most start nodes tried when looking for the smallest cycle of a big component.
const MAX_CYCLE_SOURCES: usize = 400;

fn in_scope(cx: &Ctx<'_>, files: &[FileIx]) -> bool {
    match &cx.cfg.no_cycles {
        NoCycles::Off => false,
        NoCycles::All => true,
        NoCycles::Scopes(s) => files.iter().any(|&f| s.matches(cx.g.path(f))),
    }
}

pub(super) fn file_cycles(cx: &Ctx<'_>) -> Vec<Finding> {
    let g = cx.g;
    let comps = tarjan(&cx.rt_adj);
    let mut comp_of = vec![u32::MAX; cx.rt_adj.len()];
    for (c, comp) in comps.iter().enumerate() {
        for &u in comp {
            comp_of[u as usize] = c as u32;
        }
    }
    let mut out = Vec::new();
    for (c, comp) in comps.iter().enumerate() {
        if comp.len() < 2 {
            continue;
        }
        let cyc = shortest_cycle(&cx.rt_adj, comp, &comp_of, c as u32, MAX_CYCLE_SOURCES);
        if cyc.is_empty() {
            continue;
        }
        let names: Vec<&str> = cyc
            .iter()
            .map(|&f| g.files[f as usize].name.as_str())
            .collect();
        let title = if comp.len() == 2 {
            format!(
                "{} and {} import each other",
                g.path(comp[0]),
                g.path(comp[1])
            )
        } else if cyc.len() == comp.len() {
            format!("cycle of {} files: {}", comp.len(), names.join(" → "))
        } else {
            format!(
                "{} files tangled in a cycle (shortest loop: {})",
                comp.len(),
                names.join(" → ")
            )
        };
        let key = g.id(NodeRef::File(comp[0]));
        let mut f = Finding::new(FindingKind::Cycle, Severity::Warning, &key, title);
        let mut via_dir = 0;
        for i in 0..cyc.len() {
            let (a, b) = (cyc[i], cyc[(i + 1) % cyc.len()]);
            f.line(g.edge_evidence(a, b));
            f.edge(g, a, b);
            if g.out[a as usize].iter().any(|d| d.file == b && d.via_dir) {
                via_dir += 1;
            }
        }
        for &u in &cyc {
            f.node(g.id(NodeRef::File(u)));
        }
        for &u in comp {
            f.node(g.id(NodeRef::File(u)));
        }
        f.metric("files", comp.len() as f64);
        f.metric("loop", cyc.len() as f64);
        if via_dir > 0 {
            f.metric("via_dir_edges", via_dir as f64);
        }
        if in_scope(cx, &cyc) {
            f.violation = true;
            f.severity = Severity::Error;
        }
        out.push(f);
    }
    out
}

/// Lowest common folder of two folders, and the children of it that hold each one (`None`
/// when the folder *is* the common one).
fn sibling_pair(g: &RepoGraph, a: DirIx, b: DirIx) -> Option<(DirIx, DirIx)> {
    let (mut x, mut y) = (a, b);
    let depth = |d: DirIx| g.dirs[d as usize].depth;
    let up = |d: DirIx| g.dirs[d as usize].parent.unwrap_or(d);
    let (mut cx, mut cy) = (None, None);
    while depth(x) > depth(y) {
        cx = Some(x);
        x = up(x);
    }
    while depth(y) > depth(x) {
        cy = Some(y);
        y = up(y);
    }
    while x != y {
        cx = Some(x);
        cy = Some(y);
        x = up(x);
        y = up(y);
    }
    match (cx, cy) {
        (Some(p), Some(q)) if p != q => Some((p, q)),
        _ => None,
    }
}

/// Dependencies between sibling folders: (from, to) → (file pairs, example pair).
pub(crate) fn sibling_edges(
    g: &RepoGraph,
    adj: &Adj,
) -> BTreeMap<(DirIx, DirIx), (usize, (FileIx, FileIx))> {
    let mut m: BTreeMap<(DirIx, DirIx), (usize, (FileIx, FileIx))> = BTreeMap::new();
    for (a, deps) in adj.iter().enumerate() {
        let da = g.files[a].dir;
        for &b in deps {
            let db = g.files[b as usize].dir;
            if da == db {
                continue;
            }
            if let Some(pair) = sibling_pair(g, da, db) {
                let e = m.entry(pair).or_insert((0, (a as FileIx, b)));
                e.0 += 1;
            }
        }
    }
    m
}

pub(super) fn folder_cycles(cx: &Ctx<'_>) -> Vec<Finding> {
    let g = cx.g;
    let edges = sibling_edges(g, &cx.rt_adj);
    let mut adj: Adj = vec![Vec::new(); g.dirs.len()];
    for &(a, b) in edges.keys() {
        adj[a as usize].push(b);
    }
    let comps = tarjan(&adj);
    let mut comp_of = vec![u32::MAX; adj.len()];
    for (c, comp) in comps.iter().enumerate() {
        for &u in comp {
            comp_of[u as usize] = c as u32;
        }
    }
    let mut out = Vec::new();
    for (c, comp) in comps.iter().enumerate() {
        if comp.len() < 2 {
            continue;
        }
        let cyc = shortest_cycle(&adj, comp, &comp_of, c as u32, MAX_CYCLE_SOURCES);
        if cyc.is_empty() {
            continue;
        }
        let label = |d: DirIx| g.label(NodeRef::Dir(d));
        let title = if cyc.len() == 2 {
            format!(
                "folders {} and {} depend on each other",
                label(cyc[0]),
                label(cyc[1])
            )
        } else {
            format!(
                "folder cycle: {}",
                cyc.iter()
                    .chain(std::iter::once(&cyc[0]))
                    .map(|&d| label(d))
                    .collect::<Vec<_>>()
                    .join(" → ")
            )
        };
        let key = g.id(NodeRef::Dir(comp[0]));
        let mut f = Finding::new(FindingKind::FolderCycle, Severity::Warning, &key, title);
        let mut lightest: Option<(usize, DirIx, DirIx)> = None;
        let mut examples = Vec::new();
        for i in 0..cyc.len() {
            let (a, b) = (cyc[i], cyc[(i + 1) % cyc.len()]);
            let Some(&(n, (fa, fb))) = edges.get(&(a, b)) else {
                continue;
            };
            examples.push(fa);
            examples.push(fb);
            let pairs = if n == 1 {
                "1 file pair".to_string()
            } else {
                format!("{n} file pairs")
            };
            f.line(format!(
                "{} → {} ({pairs}, e.g. {})",
                label(a),
                label(b),
                g.edge_evidence(fa, fb)
            ));
            f.edge(g, fa, fb);
            if lightest.is_none_or(|(m, _, _)| n < m) {
                lightest = Some((n, a, b));
            }
        }
        if let Some((n, a, b)) = lightest {
            f.line(format!(
                "lightest link to cut: {} → {} ({n})",
                label(a),
                label(b)
            ));
            // Every file pair of the lightest link, so the plan can show all of them.
            for (x, deps) in cx.rt_adj.iter().enumerate() {
                let x = x as FileIx;
                if !g.file_within(x, a) {
                    continue;
                }
                for &y in deps {
                    if g.file_within(y, b) {
                        f.edge(g, x, y);
                    }
                }
            }
        }
        for &d in comp {
            f.node(g.id(NodeRef::Dir(d)));
        }
        f.metric("folders", comp.len() as f64);
        f.metric("loop", cyc.len() as f64);
        if in_scope(cx, &examples) {
            f.violation = true;
            f.severity = Severity::Error;
        }
        out.push(f);
    }
    out
}
