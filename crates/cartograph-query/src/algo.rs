//! Graph algorithms on plain adjacency lists (`adj[u]` = successors of `u`, sorted).
//! Everything is iterative (no recursion: Windows' main thread has a 1 MB stack) and
//! deterministic (no hash-map iteration order reaches a result).

use std::collections::VecDeque;

pub type Adj = Vec<Vec<u32>>;

/// Strongly connected components (Tarjan, iterative). Each component is sorted; components
/// come in reverse topological order of the condensation (sinks first).
pub fn tarjan(adj: &Adj) -> Vec<Vec<u32>> {
    let n = adj.len();
    const NONE: u32 = u32::MAX;
    let mut index = vec![NONE; n];
    let mut low = vec![0u32; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<u32> = Vec::new();
    let mut out = Vec::new();
    let mut next = 0u32;
    // (node, next child position)
    let mut call: Vec<(u32, usize)> = Vec::new();
    for root in 0..n as u32 {
        if index[root as usize] != NONE {
            continue;
        }
        call.push((root, 0));
        index[root as usize] = next;
        low[root as usize] = next;
        next += 1;
        stack.push(root);
        on_stack[root as usize] = true;
        while let Some(&mut (v, ref mut pos)) = call.last_mut() {
            let vs = v as usize;
            if *pos < adj[vs].len() {
                let w = adj[vs][*pos];
                *pos += 1;
                let ws = w as usize;
                if index[ws] == NONE {
                    index[ws] = next;
                    low[ws] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[ws] = true;
                    call.push((w, 0));
                } else if on_stack[ws] {
                    low[vs] = low[vs].min(index[ws]);
                }
            } else {
                call.pop();
                if let Some(&(p, _)) = call.last() {
                    low[p as usize] = low[p as usize].min(low[vs]);
                }
                if low[vs] == index[vs] {
                    let mut comp = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w as usize] = false;
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    comp.sort_unstable();
                    out.push(comp);
                }
            }
        }
    }
    out
}

/// Shortest path from any of `sources` to any of `targets` (BFS), skipping nodes for which
/// `blocked` is true and the edges in `banned`. Neighbours are visited in adjacency order,
/// so the result is deterministic. The path includes both ends.
pub fn bfs_path(
    adj: &Adj,
    sources: &[u32],
    is_target: &dyn Fn(u32) -> bool,
    blocked: &dyn Fn(u32) -> bool,
    banned: &[(u32, u32)],
) -> Option<Vec<u32>> {
    const NONE: u32 = u32::MAX;
    let mut prev = vec![NONE; adj.len()];
    let mut seen = vec![false; adj.len()];
    let mut q = VecDeque::new();
    for &s in sources {
        if !seen[s as usize] {
            seen[s as usize] = true;
            q.push_back(s);
        }
    }
    while let Some(u) = q.pop_front() {
        for &v in &adj[u as usize] {
            if seen[v as usize] || blocked(v) || banned.contains(&(u, v)) {
                continue;
            }
            seen[v as usize] = true;
            prev[v as usize] = u;
            if is_target(v) {
                let mut path = vec![v];
                let mut c = v;
                while prev[c as usize] != NONE {
                    c = prev[c as usize];
                    path.push(c);
                }
                path.reverse();
                return Some(path);
            }
            q.push_back(v);
        }
    }
    None
}

/// The shortest cycle inside one strongly connected component (`comp`, sorted; `comp_of[u]`
/// = component index of `u`, `c` = this one). Tries at most `max_sources` start nodes, in
/// order, and keeps the first shortest found. Returns the cycle without repeating the start.
pub fn shortest_cycle(
    adj: &Adj,
    comp: &[u32],
    comp_of: &[u32],
    c: u32,
    max_sources: usize,
) -> Vec<u32> {
    let mut best: Option<Vec<u32>> = None;
    let mut dist = vec![u32::MAX; adj.len()];
    let mut prev = vec![u32::MAX; adj.len()];
    for &s in comp.iter().take(max_sources) {
        // BFS from s inside the component until an edge back to s appears.
        let mut touched = vec![s];
        dist[s as usize] = 0;
        let mut q = VecDeque::from([s]);
        let mut found: Option<u32> = None;
        'bfs: while let Some(u) = q.pop_front() {
            if best
                .as_ref()
                .is_some_and(|b| dist[u as usize] as usize + 1 >= b.len())
            {
                break;
            }
            for &v in &adj[u as usize] {
                if comp_of[v as usize] != c {
                    continue;
                }
                if v == s {
                    found = Some(u);
                    break 'bfs;
                }
                if dist[v as usize] == u32::MAX {
                    dist[v as usize] = dist[u as usize] + 1;
                    prev[v as usize] = u;
                    touched.push(v);
                    q.push_back(v);
                }
            }
        }
        if let Some(last) = found {
            let mut cyc = vec![last];
            let mut x = last;
            while x != s {
                x = prev[x as usize];
                cyc.push(x);
            }
            cyc.reverse();
            if best.as_ref().is_none_or(|b| cyc.len() < b.len()) {
                best = Some(cyc);
            }
        }
        for t in touched {
            dist[t as usize] = u32::MAX;
            prev[t as usize] = u32::MAX;
        }
        if best.as_ref().is_some_and(|b| b.len() == 2) {
            break;
        }
    }
    best.unwrap_or_default()
}

/// Betweenness centrality (Brandes) on an unweighted directed graph. With more than
/// `max_sources` nodes, only `max_sources` evenly spaced sources are used and the result
/// is scaled up (an approximation). Returns raw (unnormalized) values and the number of
/// ordered (source, target) pairs with a path, scaled the same way.
pub fn betweenness(adj: &Adj, max_sources: usize) -> (Vec<f64>, f64) {
    let n = adj.len();
    let mut cb = vec![0.0f64; n];
    let mut reachable = 0.0f64;
    if n == 0 {
        return (cb, 0.0);
    }
    let sources: Vec<u32> = if n <= max_sources {
        (0..n as u32).collect()
    } else {
        (0..max_sources)
            .map(|i| ((i * n) / max_sources) as u32)
            .collect()
    };
    let scale = n as f64 / sources.len() as f64;
    let mut sigma = vec![0.0f64; n];
    let mut dist = vec![-1i64; n];
    let mut delta = vec![0.0f64; n];
    let mut preds: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut order: Vec<u32> = Vec::with_capacity(n);
    let mut q = VecDeque::new();
    for &s in &sources {
        for v in &order {
            let v = *v as usize;
            sigma[v] = 0.0;
            dist[v] = -1;
            delta[v] = 0.0;
            preds[v].clear();
        }
        order.clear();
        sigma[s as usize] = 1.0;
        dist[s as usize] = 0;
        q.push_back(s);
        while let Some(v) = q.pop_front() {
            order.push(v);
            for &w in &adj[v as usize] {
                let (vs, ws) = (v as usize, w as usize);
                if dist[ws] < 0 {
                    dist[ws] = dist[vs] + 1;
                    q.push_back(w);
                }
                if dist[ws] == dist[vs] + 1 {
                    sigma[ws] += sigma[vs];
                    preds[ws].push(v);
                }
            }
        }
        reachable += (order.len() - 1) as f64 * scale;
        for &w in order.iter().rev() {
            let ws = w as usize;
            for &v in &preds[ws] {
                let vs = v as usize;
                delta[vs] += sigma[vs] / sigma[ws] * (1.0 + delta[ws]);
            }
            if w != s {
                cb[ws] += delta[ws] * scale;
            }
        }
    }
    (cb, reachable)
}

/// First and third quartiles (linear interpolation). `(0, 0)` for no values.
pub fn quartiles(values: &[f64]) -> (f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0);
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let q = |p: f64| {
        let pos = p * (v.len() - 1) as f64;
        let lo = pos.floor() as usize;
        let hi = pos.ceil() as usize;
        v[lo] + (v[hi] - v[lo]) * (pos - lo as f64)
    };
    (q(0.25), q(0.75))
}

/// Tukey's upper fence: Q3 + k × IQR.
pub fn upper_fence(values: &[f64], k: f64) -> f64 {
    let (q1, q3) = quartiles(values);
    q3 + k * (q3 - q1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tarjan_finds_components_without_recursion() {
        // 0 → 1 → 2 → 0, 2 → 3, 3 → 4 → 3
        let adj: Adj = vec![vec![1], vec![2], vec![0, 3], vec![4], vec![3]];
        let mut comps = tarjan(&adj);
        comps.sort();
        assert_eq!(comps, vec![vec![0, 1, 2], vec![3, 4]]);
        // A long chain does not overflow the stack.
        let n = 200_000u32;
        let chain: Adj = (0..n)
            .map(|i| if i + 1 < n { vec![i + 1] } else { vec![0] })
            .collect();
        assert_eq!(tarjan(&chain).len(), 1);
    }

    #[test]
    fn shortest_cycle_prefers_the_short_one() {
        // Big cycle 0→1→2→3→4→0 plus a short one 2→4? no: 3→1.
        let adj: Adj = vec![vec![1], vec![2], vec![3], vec![4, 1], vec![0]];
        let comp = vec![0, 1, 2, 3, 4];
        let comp_of = vec![0; 5];
        let c = shortest_cycle(&adj, &comp, &comp_of, 0, 100);
        assert_eq!(c, vec![1, 2, 3]);
    }

    #[test]
    fn betweenness_of_a_bridge() {
        // 0 → 1 → 2: node 1 is on the only path 0 → 2.
        let adj: Adj = vec![vec![1], vec![2], vec![]];
        let (b, pairs) = betweenness(&adj, 100);
        assert_eq!(b, vec![0.0, 1.0, 0.0]);
        // 0→1, 0→2, 1→2.
        assert_eq!(pairs, 3.0);
    }

    #[test]
    fn fences() {
        let v: Vec<f64> = (1..=9).map(f64::from).collect();
        assert_eq!(quartiles(&v), (3.0, 7.0));
        assert_eq!(upper_fence(&v, 1.5), 13.0);
        assert_eq!(quartiles(&[]), (0.0, 0.0));
    }
}
