//! Deep dependency chains: the longest paths of the dependency graph once cycles are
//! collapsed (a cycle counts as one step). A chain is deep when it is at least
//! `chain_min` files long and above the repo's fence of chain lengths.

use super::{Ctx, Finding, FindingKind, Severity};
use crate::algo::{tarjan, upper_fence};
use crate::graph::{FileIx, NodeRef};

pub(super) fn deep_chains(cx: &Ctx<'_>) -> Vec<Finding> {
    let g = cx.g;
    let t = &cx.cfg.thresholds;
    let n = cx.chain_adj.len();
    if n == 0 || t.max_chains == 0 {
        return Vec::new();
    }
    // Tarjan returns components sinks first: a component's successors are all done
    // before it, so one pass computes the longest chain from every component.
    let comps = tarjan(&cx.chain_adj);
    let mut comp_of = vec![0usize; n];
    for (c, comp) in comps.iter().enumerate() {
        for &u in comp {
            comp_of[u as usize] = c;
        }
    }
    // depth[c] = files on the longest chain starting at component c; next[c] = (file, next file).
    let mut depth = vec![0usize; comps.len()];
    let mut next: Vec<Option<(FileIx, FileIx)>> = vec![None; comps.len()];
    for (c, comp) in comps.iter().enumerate() {
        let mut best = 0;
        for &u in comp {
            for &v in &cx.chain_adj[u as usize] {
                let cv = comp_of[v as usize];
                if cv != c && depth[cv] > best {
                    best = depth[cv];
                    next[c] = Some((u, v));
                }
            }
        }
        depth[c] = best + 1;
    }
    let active_depths: Vec<f64> = (0..n)
        .filter(|&u| cx.active[u] && g.files[u].kind == crate::kind::FileKind::Code)
        .map(|u| depth[comp_of[u]] as f64)
        .collect();
    let min = upper_fence(&active_depths, t.fence).max(t.chain_min as f64);
    // Starts: files nothing depends on, deepest first.
    let mut indeg = vec![0usize; n];
    for deps in &cx.chain_adj {
        for &v in deps {
            indeg[v as usize] += 1;
        }
    }
    let mut starts: Vec<usize> = (0..n)
        .filter(|&u| {
            cx.active[u]
                && g.files[u].kind == crate::kind::FileKind::Code
                && indeg[u] == 0
                && depth[comp_of[u]] as f64 >= min
        })
        .collect();
    starts.sort_by(|&a, &b| depth[comp_of[b]].cmp(&depth[comp_of[a]]).then(a.cmp(&b)));
    let mut covered = vec![false; n];
    let mut out = Vec::new();
    for s in starts {
        if out.len() >= t.max_chains {
            break;
        }
        if covered[s] {
            continue;
        }
        // Walk the chain.
        let mut chain: Vec<FileIx> = vec![s as FileIx];
        let mut c = comp_of[s];
        while let Some((u, v)) = next[c] {
            if *chain.last().unwrap_or(&u) != u {
                chain.push(u);
            }
            chain.push(v);
            c = comp_of[v as usize];
        }
        // A chain that mostly retraces an earlier one adds nothing.
        let fresh = chain.iter().filter(|&&f| !covered[f as usize]).count();
        if fresh * 2 < chain.len() {
            continue;
        }
        for &f in &chain {
            covered[f as usize] = true;
        }
        let d = depth[comp_of[s]];
        let id = g.id(NodeRef::File(s as FileIx));
        let mut x = Finding::new(
            FindingKind::DeepChain,
            Severity::Info,
            &id,
            format!("dependency chain {d} deep from {}", g.files[s].name),
        );
        for w in chain.windows(2) {
            if cx.chain_adj[w[0] as usize].contains(&w[1]) {
                x.line(g.edge_evidence(w[0], w[1]));
                x.edge(g, w[0], w[1]);
            } else {
                // Two files of one cycle: the chain goes round it.
                x.line(format!("{} ⇢ {} (same cycle)", g.path(w[0]), g.path(w[1])));
            }
        }
        x.node(id);
        if let Some(&last) = chain.last() {
            x.node(g.id(NodeRef::File(last)));
        }
        x.metric("depth", d as f64);
        x.metric("fence", min.ceil());
        out.push(x);
    }
    out
}
