//! Findings that measure the repo against itself: god modules, bottlenecks (betweenness)
//! and Stable Dependencies Principle violations. Thresholds are Tukey fences over the
//! repo's own distribution (Q3 + k × IQR) plus small floors, all overridable in
//! `[thresholds]`; no absolute magic number decides alone.

use super::{Ctx, Finding, FindingKind, Severity};
use crate::algo::{betweenness, upper_fence};
use crate::graph::{DirIx, FileIx, NodeRef};
use crate::kind::FileKind;
use std::collections::{BTreeMap, BTreeSet};

/// Betweenness is exact up to this many files, sampled above.
const BETWEENNESS_SOURCES: usize = 1500;

fn in_degree(cx: &Ctx<'_>) -> Vec<usize> {
    let mut d = vec![0usize; cx.code_adj.len()];
    for deps in &cx.code_adj {
        for &b in deps {
            d[b as usize] += 1;
        }
    }
    d
}

fn is_code(cx: &Ctx<'_>, f: usize) -> bool {
    cx.active[f] && cx.g.files[f].kind == FileKind::Code
}

/// Files judged one by one for size and centrality. Go files are left out: Go imports a
/// package, so every file of a package shares its importers and per-file fan-in or
/// betweenness says nothing (Go is judged per package: folder cycles, SDP, orphans).
fn per_file(cx: &Ctx<'_>, f: usize) -> bool {
    is_code(cx, f) && cx.g.files[f].lang.as_deref() != Some("go")
}

/// "src/ui/ 12, src/app/ 3 (+2 more)": the folders of `files`, biggest first.
fn folders(cx: &Ctx<'_>, files: impl Iterator<Item = FileIx>, max: usize) -> String {
    let mut by: BTreeMap<String, usize> = BTreeMap::new();
    for f in files {
        *by.entry(cx.g.label(NodeRef::Dir(cx.g.files[f as usize].dir)))
            .or_default() += 1;
    }
    let mut rows: Vec<_> = by.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let more = rows.len().saturating_sub(max);
    let mut s = rows
        .into_iter()
        .take(max)
        .map(|(d, n)| format!("{d} {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    if more > 0 {
        s.push_str(&format!(" (+{more} more)"));
    }
    s
}

fn importers<'a>(cx: &'a Ctx<'a>, f: FileIx) -> impl Iterator<Item = FileIx> + 'a {
    cx.g.inn[f as usize]
        .iter()
        .map(|d| d.file)
        .filter(move |&s| is_code(cx, s as usize) && cx.code_adj[s as usize].contains(&f))
}

pub(super) fn god_modules(cx: &Ctx<'_>) -> Vec<Finding> {
    let g = cx.g;
    let t = &cx.cfg.thresholds;
    let indeg = in_degree(cx);
    let code: Vec<usize> = (0..g.files.len()).filter(|&f| per_file(cx, f)).collect();
    let fan_in: Vec<f64> = code.iter().map(|&f| indeg[f] as f64).collect();
    let fan_out: Vec<f64> = code.iter().map(|&f| cx.code_adj[f].len() as f64).collect();
    let loc: Vec<f64> = code.iter().map(|&f| g.files[f].loc as f64).collect();
    let fi = upper_fence(&fan_in, t.fence).max(t.god_min_fan_in as f64);
    let fo = upper_fence(&fan_out, t.fence).max(t.god_min_fan_out as f64);
    let fl = upper_fence(&loc, t.fence).max(t.god_min_loc as f64);
    let mut out = Vec::new();
    for &f in &code {
        let (i, o, l) = (indeg[f], cx.code_adj[f].len(), g.files[f].loc);
        if (i as f64) < fi || (o as f64) < fo || (l as f64) < fl {
            continue;
        }
        let id = g.id(NodeRef::File(f as FileIx));
        let mut x = Finding::new(
            FindingKind::GodModule,
            Severity::Warning,
            &id,
            format!("god module: {l} LOC, used by {i} files, uses {o}"),
        );
        x.node(id);
        x.line(format!(
            "used by: {}",
            folders(cx, importers(cx, f as FileIx), 4)
        ));
        x.line(format!(
            "uses: {}",
            folders(cx, cx.code_adj[f].iter().copied(), 4)
        ));
        x.line(format!(
            "repo fences: fan-in ≥ {:.0}, fan-out ≥ {:.0}, LOC ≥ {:.0}",
            fi.ceil(),
            fo.ceil(),
            fl.ceil()
        ));
        x.metric("fan_in", i as f64);
        x.metric("fan_out", o as f64);
        x.metric("loc", l as f64);
        out.push(x);
    }
    out
}

pub(super) fn hubs(cx: &Ctx<'_>) -> Vec<Finding> {
    let g = cx.g;
    let t = &cx.cfg.thresholds;
    if t.max_hubs == 0 {
        return Vec::new();
    }
    let n = cx.active.iter().filter(|&&a| a).count();
    if n < 4 {
        return Vec::new();
    }
    // Share = fraction of all connected (source, target) pairs whose shortest paths pass
    // through the file: the graph is sparse, so all n² pairs would make every share tiny.
    let (b, pairs) = betweenness(&cx.code_adj, BETWEENNESS_SOURCES);
    if pairs <= 0.0 {
        return Vec::new();
    }
    let positive: Vec<f64> = b.iter().copied().filter(|&x| x > 0.0).collect();
    let fence = upper_fence(&positive, t.fence);
    let god = god_modules(cx);
    let indeg = in_degree(cx);
    let mut cands: Vec<(f64, usize)> = b
        .iter()
        .enumerate()
        .filter(|&(f, &v)| {
            // Used by at least two files: the top of the graph (a main, a screen that
            // only imports) sits on many paths but bottlenecks nothing.
            per_file(cx, f)
                && indeg[f] >= 2
                && v > fence
                && v / pairs >= t.hub_min_share
                && !god
                    .iter()
                    .any(|x| x.nodes.first() == Some(&g.id(NodeRef::File(f as FileIx))))
        })
        .map(|(f, &v)| (v, f))
        .collect();
    cands.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    cands
        .into_iter()
        .take(t.max_hubs)
        .map(|(v, f)| {
            let share = v / pairs;
            let id = g.id(NodeRef::File(f as FileIx));
            let mut x = Finding::new(
                FindingKind::Hub,
                Severity::Info,
                &id,
                format!(
                    "bottleneck: on {:.1}% of dependency paths (used by {}, uses {})",
                    share * 100.0,
                    indeg[f],
                    cx.code_adj[f].len()
                ),
            );
            x.node(id);
            x.line(format!(
                "used by: {}",
                folders(cx, importers(cx, f as FileIx), 4)
            ));
            x.line(format!(
                "uses: {}",
                folders(cx, cx.code_adj[f].iter().copied(), 4)
            ));
            x.metric("betweenness", (v * 10.0).round() / 10.0);
            x.metric("share", (share * 10000.0).round() / 10000.0);
            x.metric("fan_in", indeg[f] as f64);
            x.metric("fan_out", cx.code_adj[f].len() as f64);
            x
        })
        .collect()
}

/// Martin's instability of a folder's direct code files: I = Ce / (Ca + Ce), where Ca counts
/// distinct code files outside depending on it and Ce distinct code files outside it uses.
struct Package {
    dir: DirIx,
    ca: usize,
    ce: usize,
}

impl Package {
    fn instability(&self) -> f64 {
        if self.ca + self.ce == 0 {
            0.0
        } else {
            self.ce as f64 / (self.ca + self.ce) as f64
        }
    }
}

pub(super) fn sdp(cx: &Ctx<'_>) -> Vec<Finding> {
    let g = cx.g;
    let t = &cx.cfg.thresholds;
    // Code files per folder.
    let mut members: BTreeMap<DirIx, usize> = BTreeMap::new();
    for (f, file) in g.files.iter().enumerate() {
        if is_code(cx, f) {
            *members.entry(file.dir).or_default() += 1;
        }
    }
    let mut pkgs: BTreeMap<DirIx, Package> = members
        .iter()
        .filter(|&(_, &n)| n >= t.sdp_min_files)
        .map(|(&d, _)| {
            (
                d,
                Package {
                    dir: d,
                    ca: 0,
                    ce: 0,
                },
            )
        })
        .collect();
    let mut ca: BTreeMap<DirIx, BTreeSet<FileIx>> = BTreeMap::new();
    let mut ce: BTreeMap<DirIx, BTreeSet<FileIx>> = BTreeMap::new();
    // (from, to) → file pairs, between packages.
    let mut deps: BTreeMap<(DirIx, DirIx), Vec<(FileIx, FileIx)>> = BTreeMap::new();
    for (a, outs) in cx.rt_adj.iter().enumerate() {
        if !is_code(cx, a) {
            continue;
        }
        let da = g.files[a].dir;
        for &b in outs {
            if !is_code(cx, b as usize) {
                continue;
            }
            let db = g.files[b as usize].dir;
            if da == db {
                continue;
            }
            ce.entry(da).or_default().insert(b);
            ca.entry(db).or_default().insert(a as FileIx);
            if pkgs.contains_key(&da) && pkgs.contains_key(&db) {
                deps.entry((da, db)).or_default().push((a as FileIx, b));
            }
        }
    }
    for p in pkgs.values_mut() {
        p.ca = ca.get(&p.dir).map_or(0, |s| s.len());
        p.ce = ce.get(&p.dir).map_or(0, |s| s.len());
    }
    let mut cands: Vec<(f64, DirIx, DirIx)> = Vec::new();
    for (&(a, b), pairs) in &deps {
        let (pa, pb) = (&pkgs[&a], &pkgs[&b]);
        if pa.ca < t.sdp_min_dependents {
            continue;
        }
        let gap = pb.instability() - pa.instability();
        if gap >= t.sdp_margin {
            // Worse when the stable side is heavily used and leans on the unstable one a lot.
            cands.push((
                gap * (pa.ca as f64).ln_1p() * (pairs.len() as f64).ln_1p(),
                a,
                b,
            ));
        }
    }
    cands.sort_by(|x, y| {
        y.0.partial_cmp(&x.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then((x.1, x.2).cmp(&(y.1, y.2)))
    });
    cands
        .into_iter()
        .take(t.max_sdp)
        .map(|(_, a, b)| {
            let (pa, pb) = (&pkgs[&a], &pkgs[&b]);
            let (la, lb) = (g.label(NodeRef::Dir(a)), g.label(NodeRef::Dir(b)));
            let mut x = Finding::new(
                FindingKind::Sdp,
                Severity::Info,
                &format!("{}->{}", g.id(NodeRef::Dir(a)), g.id(NodeRef::Dir(b))),
                format!(
                    "stable {la} (I={:.2}, used by {}) depends on unstable {lb} (I={:.2})",
                    pa.instability(),
                    pa.ca,
                    pb.instability()
                ),
            );
            x.node(g.id(NodeRef::Dir(a)));
            x.node(g.id(NodeRef::Dir(b)));
            let pairs = &deps[&(a, b)];
            for &(s, d) in pairs {
                x.line(g.edge_evidence(s, d));
                x.edge(g, s, d);
            }
            x.metric(
                "instability_from",
                (pa.instability() * 100.0).round() / 100.0,
            );
            x.metric("instability_to", (pb.instability() * 100.0).round() / 100.0);
            x.metric("dependents_from", pa.ca as f64);
            x.metric("file_pairs", pairs.len() as f64);
            x
        })
        .collect()
}
