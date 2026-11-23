//! `path(a, b)`: why A depends on B, within [`limits::PATH`]. The shortest dependency path
//! plus up to two alternatives, each step with its import line. When A and B are folders,
//! the imports that cross from A straight into B come first, grouped by target file (that
//! is usually the whole answer to "why does src/ui depend on src/server").

use crate::algo::{Adj, bfs_path};
use crate::budget::{Answer, clip, fit, limits};
use crate::graph::{FileIx, NodeRef};
use crate::text::short;
use crate::{QueryError, Repo};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub id: String,
    /// Line of the import that leads to the next step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// The step to the next file is a type-only import (TypeScript `import type`…).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub type_only: bool,
    /// The step to the next file is a bridge, not an import: `ipc invoke users:get`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridge: Option<String>,
}

/// Direct imports from A into one file of B.
#[derive(Debug, Clone, Serialize)]
pub struct Crossing {
    pub target: String,
    /// Importing files in A.
    pub count: usize,
    /// Some of them, as `path:line`.
    pub from: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PathResult {
    pub from: String,
    pub to: String,
    /// File pairs A → B that are direct imports.
    pub direct: usize,
    pub crossings: Vec<Crossing>,
    pub crossings_more: usize,
    /// Shortest path first; at most 3.
    pub paths: Vec<Vec<Step>>,
    #[serde(skip_serializing_if = "is_zero")]
    pub paths_more: usize,
    /// When A does not reach B: a path from B to A, if there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reverse: Option<Vec<Step>>,
    /// When B calls out over HTTP or IPC: the calls from A's functions to B's, continued
    /// past B down to the data (see [`crate::slice`]); those shown in full first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub slice: Vec<crate::slice::Chain>,
    /// Of `slice`, how many are shown in full (the rest in one line each).
    #[serde(skip_serializing_if = "is_zero")]
    pub slice_full: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub slice_more: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// How much of a path with a slice an answer shows: alternative paths, crossings, chains
/// in full, chains in one line, continuations per chain.
const SLICE_STEPS: [(usize, usize, usize, usize, usize); 8] = [
    (3, 10, 3, 6, 2),
    (1, 6, 3, 6, 2),
    (1, 4, 2, 6, 2),
    (1, 3, 2, 4, 1),
    (1, 3, 1, 4, 1),
    (1, 2, 1, 2, 1),
    (1, 1, 1, 1, 1),
    (1, 1, 1, 0, 1),
];

/// An importing file and the line of its import.
type Source = (FileIx, Option<u32>, bool);

fn steps(repo: &Repo, p: &[FileIx]) -> Vec<Step> {
    steps_along(repo, p, &[])
}

/// [`steps`], each bridge step by a bridge of `prefer` (step labels) when one fits: the
/// bridges a slice crosses, so the path and the slice tell one story.
fn steps_along(repo: &Repo, p: &[FileIx], prefer: &[String]) -> Vec<Step> {
    let g = &repo.graph;
    let pick = |f: FileIx, n: FileIx| {
        let mut all = repo.symbols.bridges.between(g.path(f), g.path(n));
        let first = all.next()?;
        Some(
            std::iter::once(first)
                .chain(all)
                .find(|b| prefer.contains(&crate::bridges::step_label(b)))
                .unwrap_or(first),
        )
    };
    p.iter()
        .enumerate()
        .map(|(i, &f)| {
            let next = p.get(i + 1).copied();
            // A bridge only where no dependency explains the step.
            let crossed = next
                .filter(|&n| g.dep(f, n).is_none())
                .and_then(|n| pick(f, n));
            let bridge = match (crossed, next) {
                (Some(b), _) => Some(crate::bridges::step_label(b)),
                (None, Some(n)) if g.dep(f, n).is_none() => {
                    g.bridge(f, n).map(|_| "bridge".to_string())
                }
                _ => None,
            };
            Step {
                id: g.id(NodeRef::File(f)),
                line: match (&bridge, crossed) {
                    (Some(_), Some(b)) => Some(b.src_line),
                    (Some(_), None) => None,
                    (None, _) => next.and_then(|n| g.site(f, n)).map(|s| s.line),
                },
                type_only: next.is_some_and(|n| g.type_only(f, n)),
                bridge,
            }
        })
        .collect()
}

/// Up to `k` shortest distinct paths (the first, then the best found by banning one edge of
/// the paths so far at a time; a simplified Yen).
fn k_paths(
    adj: &Adj,
    src: &[FileIx],
    dst: &BTreeSet<FileIx>,
    src_set: &BTreeSet<FileIx>,
    k: usize,
) -> Vec<Vec<FileIx>> {
    let is_t = |v: u32| dst.contains(&v);
    // Paths leave A once: other files of A are not stepping stones.
    let blocked = |v: u32| src_set.contains(&v) && !dst.contains(&v);
    let Some(first) = bfs_path(adj, src, &is_t, &blocked, &[]) else {
        return Vec::new();
    };
    let mut found = vec![first];
    let mut banned: Vec<(u32, u32)> = Vec::new();
    while found.len() < k {
        let mut best: Option<Vec<FileIx>> = None;
        let last = found[found.len() - 1].clone();
        for w in last.windows(2) {
            let mut b = banned.clone();
            b.push((w[0], w[1]));
            if let Some(p) = bfs_path(adj, src, &is_t, &blocked, &b)
                && !found.contains(&p)
                && best.as_ref().is_none_or(|x| p.len() < x.len())
            {
                best = Some(p);
            }
        }
        match best {
            Some(p) => {
                // Keep the banned edge that produced it, so the next one differs again.
                if let Some(w) = last.windows(2).find(|w| !p.windows(2).any(|x| x == *w)) {
                    banned.push((w[0], w[1]));
                }
                found.push(p);
            }
            None => break,
        }
    }
    found
}

pub fn path(repo: &Repo, a: &str, b: &str) -> Result<Answer<PathResult>, QueryError> {
    path_with(repo, a, b, true)
}

/// [`path`]; with `bridges` the paths may cross bridges (an IPC channel, an exposed preload
/// function, an HTTP route) as well as dependencies.
pub fn path_with(
    repo: &Repo,
    a: &str,
    b: &str,
    bridges: bool,
) -> Result<Answer<PathResult>, QueryError> {
    let g = &repo.graph;
    let na = repo.resolve(a)?;
    let nb = repo.resolve(b)?;
    let fa = g.files_of(na);
    let fb = match nb {
        NodeRef::Ext(_) => {
            return Err(QueryError::BadArgument(format!(
                "{} is a library; use `explain {}` to see who imports it",
                g.label(nb),
                g.label(nb)
            )));
        }
        _ => g.files_of(nb),
    };
    if fa.is_empty() {
        return Err(QueryError::BadArgument(format!(
            "{} has no files",
            g.label(na)
        )));
    }
    let sa: BTreeSet<FileIx> = fa.iter().copied().collect();
    let sb: BTreeSet<FileIx> = fb.iter().copied().collect();
    if sa.intersection(&sb).next().is_some() {
        return Err(QueryError::BadArgument(format!(
            "{} and {} overlap; pick two separate nodes",
            g.label(na),
            g.label(nb)
        )));
    }
    let adj: Adj = g
        .out
        .iter()
        .zip(&g.bridge_out)
        .map(|(d, b)| {
            let mut v: Vec<FileIx> = d.iter().map(|x| x.file).collect();
            if bridges {
                v.extend(
                    b.iter()
                        .map(|x| x.file)
                        .filter(|f| !d.iter().any(|x| x.file == *f)),
                );
            }
            v
        })
        .collect();
    let paths = k_paths(&adj, &fa, &sb, &sa, 3);
    let reverse = if paths.is_empty() {
        k_paths(&adj, &fb, &sa, &sb, 1).into_iter().next()
    } else {
        None
    };
    // Direct crossings, by target.
    let mut by_target: BTreeMap<FileIx, Vec<Source>> = BTreeMap::new();
    for &f in &fa {
        for d in &g.out[f as usize] {
            if sb.contains(&d.file) {
                by_target.entry(d.file).or_default().push((
                    f,
                    g.site(f, d.file).map(|s| s.line),
                    d.type_only,
                ));
            }
        }
    }
    let direct: usize = by_target.values().map(Vec::len).sum();
    let mut crossings: Vec<(FileIx, Vec<Source>)> = by_target.into_iter().collect();
    crossings.sort_by(|x, y| y.1.len().cmp(&x.1.len()).then(x.0.cmp(&y.0)));

    // B calls out over a bridge: the slice past it.
    let to_paths: BTreeSet<String> = fb.iter().map(|&f| g.path(f).to_string()).collect();
    let chains = if bridges && !paths.is_empty() && crate::slice::crosses(&repo.symbols, &to_paths)
    {
        let from_paths: BTreeSet<String> = fa.iter().map(|&f| g.path(f).to_string()).collect();
        crate::slice::chains(&repo.symbols, &from_paths, &to_paths)
    } else {
        Vec::new()
    };
    let base = |limit: usize| PathResult {
        from: g.id(na),
        to: g.id(nb),
        direct,
        crossings: crossings
            .iter()
            .take(limit)
            .map(|(t, srcs)| Crossing {
                target: g.id(NodeRef::File(*t)),
                count: srcs.len(),
                from: srcs
                    .iter()
                    .take((limit / 3).max(1))
                    .map(|&(s, line, type_only)| {
                        let at = match line {
                            Some(l) => format!("{}:{l}", g.path(s)),
                            None => g.path(s).to_string(),
                        };
                        if type_only {
                            format!("{at} (type)")
                        } else {
                            at
                        }
                    })
                    .collect(),
            })
            .collect(),
        crossings_more: crossings.len().saturating_sub(limit),
        paths: paths
            .iter()
            .take(if limit >= 3 { 3 } else { 1 })
            .map(|p| steps(repo, p))
            .collect(),
        paths_more: 0,
        reverse: reverse.as_ref().map(|p| steps(repo, p)),
        slice: Vec::new(),
        slice_full: 0,
        slice_more: 0,
    };
    if !chains.is_empty() {
        let prefer: Vec<String> = chains[0]
            .hops
            .iter()
            .filter_map(|h| h.via.clone())
            .collect();
        let idx: Vec<usize> = (0..SLICE_STEPS.len()).collect();
        return Ok(fit(
            limits::SLICE,
            &idx,
            |k| {
                let (alts, limit, full, brief, conts) = SLICE_STEPS[k];
                PathResult {
                    paths: paths
                        .iter()
                        .take(alts)
                        .map(|p| steps_along(repo, p, &prefer))
                        .collect(),
                    paths_more: paths.len().saturating_sub(alts),
                    slice: chains
                        .iter()
                        .take(full + brief)
                        .enumerate()
                        .map(|(i, c)| crate::slice::Chain {
                            hops: c.hops.clone(),
                            continues: c
                                .continues
                                .iter()
                                .take(if i < full { conts } else { 1 })
                                .cloned()
                                .collect(),
                        })
                        .collect(),
                    slice_full: full.min(chains.len()),
                    slice_more: chains.len().saturating_sub(full + brief),
                    ..base(limit)
                }
            },
            render,
        ));
    }
    Ok(fit(limits::PATH, &[10, 8, 6, 5, 4, 3, 2, 1], base, render))
}

fn render_path(s: &mut String, p: &[Step]) {
    let parts: Vec<String> = p
        .iter()
        .map(|x| {
            let at = match x.line {
                Some(l) => format!("{}:{l}", clip(&short(&x.id), 120)),
                None => clip(&short(&x.id), 120),
            };
            match (&x.bridge, x.type_only) {
                (Some(b), _) => format!("{at} ({})", clip(b, 100)),
                (None, true) => format!("{at} (type)"),
                (None, false) => at,
            }
        })
        .collect();
    s.push_str(&format!("  {}\n", parts.join(" → ")));
}

fn render(r: &PathResult) -> String {
    let (a, b) = (short(&r.from), short(&r.to));
    let mut s = String::new();
    if r.paths.is_empty() {
        s.push_str(&format!("{a} does not depend on {b}\n"));
        if let Some(rev) = &r.reverse {
            s.push_str(&format!("but {b} depends on {a}:\n"));
            render_path(&mut s, rev);
        }
        return s;
    }
    let hops = r.paths[0].len() - 1;
    s.push_str(&format!(
        "{a} → {b}: shortest path {hops} {} (path:line = the import)\n",
        if hops == 1 { "step" } else { "steps" }
    ));
    let listed = r.direct > 0 && (r.crossings.len() > 1 || r.crossings.iter().any(|c| c.count > 1));
    if listed {
        s.push_str(&format!("{} direct imports, by target:\n", r.direct));
        for c in &r.crossings {
            let more = c.count.saturating_sub(c.from.len());
            s.push_str(&format!(
                "  {} ← {}: {}{}\n",
                clip(&short(&c.target), 120),
                c.count,
                clip(&c.from.join(", "), 240),
                if more > 0 {
                    format!(" +{more}")
                } else {
                    String::new()
                }
            ));
        }
        if r.crossings_more > 0 {
            s.push_str(&format!("  +{} more targets\n", r.crossings_more));
        }
    }
    // One-step paths repeat the direct imports just listed.
    let longer: Vec<&Vec<Step>> = r
        .paths
        .iter()
        .filter(|p| !(listed && p.len() == 2))
        .collect();
    if !longer.is_empty() {
        s.push_str("paths:\n");
        for p in longer {
            render_path(&mut s, p);
        }
    }
    if r.paths_more > 0 {
        s.push_str(&format!("  +{} more paths\n", r.paths_more));
    }
    render_slice(&mut s, r, &b);
    s
}

/// The chains of a slice: the first ones a step per line (the file only when it changes),
/// the rest in one line each.
fn render_slice(s: &mut String, r: &PathResult, b: &str) {
    use crate::slice::{Continuation, Hop};
    if r.slice.is_empty() {
        return;
    }
    let to_name = b.rsplit('/').next().unwrap_or(b);
    let step = |s: &mut String, h: &Hop, last: &mut String, first: bool| {
        let at = if h.file == *last {
            format!(":{}", h.line)
        } else {
            format!("{}:{}", clip(&h.file, 160), h.line)
        };
        *last = h.file.clone();
        let via = h
            .via
            .as_deref()
            .map_or(String::new(), |v| format!(" ({})", clip(v, 100)));
        let arrow = if first { "" } else { "→ " };
        s.push_str(&format!("    {arrow}{at} {}{via}\n", clip(&h.symbol, 100)));
    };
    let sink_text = |c: &Continuation| {
        c.sink.as_ref().map(|k| {
            let via = k
                .entity
                .as_deref()
                .map_or(String::new(), |e| format!(", via {e}"));
            let what = if k.store == "Mongo" {
                "collection"
            } else {
                "table"
            };
            format!("{what} {} ({}{via})", clip(&k.name, 80), k.store)
        })
    };
    s.push_str(&format!(
        "calls from {} into {to_name} and past it (path:line = where the next step is called):\n",
        short(&r.from)
    ));
    for (i, c) in r.slice.iter().enumerate() {
        if i < r.slice_full {
            s.push_str(&format!("  {}.\n", i + 1));
            let mut last = String::new();
            for (j, h) in c.hops.iter().enumerate() {
                step(s, h, &mut last, j == 0);
            }
            for k in &c.continues {
                s.push_str(&format!(
                    "    continues past {to_name} over {}:\n",
                    clip(&k.key, 100)
                ));
                for h in &k.hops {
                    step(s, h, &mut last, false);
                }
                if let Some(t) = sink_text(k) {
                    s.push_str(&format!("    → {t}\n"));
                }
                if !k.other_sinks.is_empty() {
                    let more = if k.other_sinks_more > 0 {
                        format!(", +{} more", k.other_sinks_more)
                    } else {
                        String::new()
                    };
                    s.push_str(&format!(
                        "    also reaches: {}{more}\n",
                        clip(&k.other_sinks.join(", "), 200)
                    ));
                }
            }
            continue;
        }
        // One line: the functions only.
        let mut names: Vec<String> = c.hops.iter().map(|h| clip(&h.symbol, 60)).collect();
        for k in c.continues.iter().take(1) {
            names.push(clip(&k.key, 60));
            names.extend(k.hops.iter().map(|h| clip(&h.symbol, 60)));
            if let Some(t) = sink_text(k) {
                names.push(t);
            }
        }
        s.push_str(&format!("  {}. {}\n", i + 1, names.join(" → ")));
    }
    if r.slice_more > 0 {
        s.push_str(&format!("  +{} more chains\n", r.slice_more));
    }
}
