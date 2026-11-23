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
    /// When A does not reach B: a path from B to A, if there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reverse: Option<Vec<Step>>,
}

/// An importing file and the line of its import.
type Source = (FileIx, Option<u32>, bool);

fn steps(repo: &Repo, p: &[FileIx]) -> Vec<Step> {
    let g = &repo.graph;
    p.iter()
        .enumerate()
        .map(|(i, &f)| {
            let next = p.get(i + 1).copied();
            // A bridge only where no dependency explains the step.
            let bridge = next.filter(|&n| g.dep(f, n).is_none()).and_then(|n| {
                repo.symbols
                    .bridges
                    .between(g.path(f), g.path(n))
                    .next()
                    .map(crate::bridges::step_label)
                    .or_else(|| g.bridge(f, n).map(|_| "bridge".to_string()))
            });
            Step {
                id: g.id(NodeRef::File(f)),
                line: match &bridge {
                    Some(_) => repo
                        .symbols
                        .bridges
                        .between(g.path(f), next.map_or("", |n| g.path(n)))
                        .next()
                        .map(|b| b.src_line),
                    None => next.and_then(|n| g.site(f, n)).map(|s| s.line),
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

    Ok(fit(
        limits::PATH,
        &[10, 8, 6, 5, 4, 3, 2, 1],
        |limit| PathResult {
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
            reverse: reverse.as_ref().map(|p| steps(repo, p)),
        },
        render,
    ))
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
    s
}
