//! `infer`: propose layers from the dependency graph and render them as a `cartograph.toml`.
//!
//! 1. Nodes are modules (see [`units`]: a folder small enough or a collection of peers like
//!    `components/` taken whole, bigger folders split); an undirected edge's weight is the
//!    number of code file pairs that depend on each other across two modules.
//! 2. Louvain community detection (modularity, deterministic: nodes in path order, ties to
//!    the smallest community) groups modules that mostly talk to each other.
//! 3. Each community becomes a layer whose paths are the smallest set of `dir/**` (a whole
//!    subtree in one community) and `dir/*` (one folder's own files) globs.
//! 4. Layers are ordered from the most dependent (top) to the most depended on (bottom), by
//!    Martin's instability. `allow` lists every dependency that exists today between the
//!    proposed layers, counted over the same edges `check` looks at (every file the layer's
//!    globs match, config and data included), so the proposal passes `check` as is; the ones
//!    that point *up* are flagged in comments as the first candidates to remove.
//!
//! Folder names go into globs escaped (`app/[id]/` → `app/[[]id[]]/**`), and the `ignore`
//! of the current config is honoured and written back.

use crate::Repo;
use crate::budget::Answer;
use crate::config::{Config, Layer, PathSet};
use crate::findings::Ctx;
use crate::graph::{DirIx, RepoGraph};
use crate::kind::FileKind;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize)]
pub struct ProposedLayer {
    pub name: String,
    pub paths: Vec<String>,
    pub files: u64,
    pub loc: u64,
    /// Layers it depends on today.
    pub allow: Vec<String>,
    /// Of those, the ones that go against the proposed order: (layer, file pairs).
    pub upward: Vec<(String, usize)>,
    /// Martin's instability of the layer (0 = only depended on, 1 = only depends).
    pub instability: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Inferred {
    pub layers: Vec<ProposedLayer>,
    /// Folders with code but no dependency in or out, left out of every layer.
    pub unconnected_folders: usize,
    pub modularity: f64,
    pub toml: String,
}

/// Louvain on an undirected weighted graph; returns a community per node, numbered from 0
/// in order of first appearance.
pub fn louvain(n: usize, edges: &[(u32, u32, f64)]) -> Vec<u32> {
    // Current level: adjacency with self-loops (weight counted once in `adj[i]` as (i, w)).
    let mut adj: Vec<BTreeMap<u32, f64>> = vec![BTreeMap::new(); n];
    for &(a, b, w) in edges {
        if w <= 0.0 {
            continue;
        }
        *adj[a as usize].entry(b).or_default() += w;
        if a != b {
            *adj[b as usize].entry(a).or_default() += w;
        }
    }
    let mut membership: Vec<u32> = (0..n as u32).collect();
    for _level in 0..20 {
        let m = adj.len();
        let degree: Vec<f64> = (0..m)
            .map(|i| {
                adj[i]
                    .iter()
                    .map(|(&j, &w)| if j as usize == i { 2.0 * w } else { w })
                    .sum()
            })
            .collect();
        let m2: f64 = degree.iter().sum();
        if m2 <= 0.0 {
            break;
        }
        let mut comm: Vec<u32> = (0..m as u32).collect();
        let mut tot = degree.clone();
        let mut moved_any = false;
        for _pass in 0..100 {
            let mut moved = false;
            for i in 0..m {
                let ci = comm[i];
                let ki = degree[i];
                let mut neigh: BTreeMap<u32, f64> = BTreeMap::new();
                for (&j, &w) in &adj[i] {
                    if j as usize != i {
                        *neigh.entry(comm[j as usize]).or_default() += w;
                    }
                }
                tot[ci as usize] -= ki;
                let mut best = ci;
                let mut best_gain =
                    neigh.get(&ci).copied().unwrap_or(0.0) - tot[ci as usize] * ki / m2;
                for (&c, &w) in &neigh {
                    let gain = w - tot[c as usize] * ki / m2;
                    if gain > best_gain + 1e-12 {
                        best = c;
                        best_gain = gain;
                    }
                }
                tot[best as usize] += ki;
                if best != ci {
                    comm[i] = best;
                    moved = true;
                    moved_any = true;
                }
            }
            if !moved {
                break;
            }
        }
        // Renumber in order of first appearance.
        let mut renum: BTreeMap<u32, u32> = BTreeMap::new();
        let mut order: Vec<u32> = Vec::new();
        for &c in &comm {
            if let std::collections::btree_map::Entry::Vacant(e) = renum.entry(c) {
                e.insert(order.len() as u32);
                order.push(c);
            }
        }
        let comm: Vec<u32> = comm.iter().map(|c| renum[c]).collect();
        for x in &mut membership {
            *x = comm[*x as usize];
        }
        if !moved_any || order.len() == m {
            break;
        }
        // Aggregate.
        let mut next: Vec<BTreeMap<u32, f64>> = vec![BTreeMap::new(); order.len()];
        for i in 0..m {
            for (&j, &w) in &adj[i] {
                let (a, b) = (comm[i], comm[j as usize]);
                if a == b {
                    // Each internal edge is seen from both ends (a self-loop once).
                    let share = if j as usize == i { w } else { w / 2.0 };
                    *next[a as usize].entry(a).or_default() += share;
                } else {
                    *next[a as usize].entry(b).or_default() += w;
                }
            }
        }
        adj = next;
    }
    membership
}

/// Modularity of a partition.
fn modularity(n: usize, edges: &[(u32, u32, f64)], comm: &[u32]) -> f64 {
    let mut deg = vec![0.0; n];
    let mut m = 0.0;
    let mut inside = 0.0;
    for &(a, b, w) in edges {
        deg[a as usize] += w;
        deg[b as usize] += w;
        m += w;
        if comm[a as usize] == comm[b as usize] {
            inside += w;
        }
    }
    if m == 0.0 {
        return 0.0;
    }
    let mut tot: BTreeMap<u32, f64> = BTreeMap::new();
    for i in 0..n {
        *tot.entry(comm[i]).or_default() += deg[i];
    }
    inside / m - tot.values().map(|t| (t / (2.0 * m)).powi(2)).sum::<f64>()
}

fn sanitize(s: &str) -> String {
    let out: String = s
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let t = out.trim_matches('-').to_string();
    if t.is_empty() { "root".to_string() } else { t }
}

/// A literal path made safe for a glob: each of `*?[]{}` becomes a one-character class
/// (`[` → `[[]`), so `app/[id]` matches the folder `app/[id]` and nothing else. No
/// backslashes: config paths turn them into `/`.
pub fn glob_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '{' | '}') {
            out.push('[');
            out.push(c);
            out.push(']');
        } else {
            out.push(c);
        }
    }
    out
}

fn toml_str(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Folders that hold peers (one subfolder per component, view, page…): one module each.
const COLLECTIONS: &[&str] = &[
    "components",
    "views",
    "pages",
    "screens",
    "widgets",
    "modules",
    "features",
    "dialogs",
    "routes",
    "plugins",
    "containers",
    "layouts",
    "icons",
    "stories",
    "controls",
    "panels",
];

/// A module: a folder taken whole (`dir/**`) or only its own files (`dir/*`).
struct Unit {
    dir: DirIx,

    files: usize,
}

/// Split the tree into modules, top-down: a folder small enough (at most 5 % of the repo's
/// code files, and never fewer than 8) or without subfolders with code is one module;
/// so is a collection of peers (`components/`, `views/`… holding small folders); a bigger one
/// splits into its own files and its subfolders. Returns the modules and the module of
/// every folder with code.
fn units(
    g: &RepoGraph,
    code_files: &BTreeMap<DirIx, usize>,
) -> (Vec<Unit>, BTreeMap<DirIx, usize>) {
    let mut under = vec![0usize; g.dirs.len()];
    let mut by_depth: Vec<DirIx> = (0..g.dirs.len() as DirIx).collect();
    by_depth.sort_by(|a, b| {
        g.dirs[*b as usize]
            .depth
            .cmp(&g.dirs[*a as usize].depth)
            .then(a.cmp(b))
    });
    for &d in &by_depth {
        under[d as usize] += code_files.get(&d).copied().unwrap_or(0);
        if let Some(p) = g.dirs[d as usize].parent {
            under[p as usize] += under[d as usize];
        }
    }
    let total = under[crate::graph::ROOT as usize];
    let small = (total / 20).max(8);
    let mut list: Vec<Unit> = Vec::new();
    let mut unit_of: BTreeMap<DirIx, usize> = BTreeMap::new();
    let mut stack = vec![crate::graph::ROOT];
    while let Some(d) = stack.pop() {
        if under[d as usize] == 0 {
            continue;
        }
        let kids: Vec<DirIx> = g.dirs[d as usize]
            .dirs
            .iter()
            .copied()
            .filter(|&k| under[k as usize] > 0)
            .collect();
        // A collection of many small folders (components/, views/, modules/…) is one module:
        // splitting it would make every component a layer.
        let mut sizes: Vec<usize> = kids.iter().map(|&k| under[k as usize]).collect();
        sizes.sort_unstable();
        let collection = COLLECTIONS
            .contains(&g.dirs[d as usize].name.to_ascii_lowercase().as_str())
            && sizes.len() >= 4
            && sizes[sizes.len() / 2] * 2 < small;
        if kids.is_empty()
            || ((under[d as usize] <= small || collection) && d != crate::graph::ROOT)
        {
            let u = list.len();
            list.push(Unit {
                dir: d,
                files: under[d as usize],
            });
            let mut sub = vec![d];
            while let Some(x) = sub.pop() {
                if code_files.contains_key(&x) {
                    unit_of.insert(x, u);
                }
                sub.extend(g.dirs[x as usize].dirs.iter().copied());
            }
            continue;
        }
        if let Some(&own) = code_files.get(&d) {
            unit_of.insert(d, list.len());
            list.push(Unit { dir: d, files: own });
        }
        stack.extend(kids.into_iter().rev());
    }
    (list, unit_of)
}

pub fn infer(repo: &Repo) -> Answer<Inferred> {
    let g = &repo.graph;
    // Folders with code files (tests, scripts and config are not layered).
    let mut code_files: BTreeMap<DirIx, usize> = BTreeMap::new();
    for f in &g.files {
        if f.kind == FileKind::Code && !repo.config.ignore.matches(&f.path) {
            *code_files.entry(f.dir).or_default() += 1;
        }
    }
    let (units, unit_of) = units(g, &code_files);
    // Module → module dependencies, in file pairs.
    let mut directed: BTreeMap<(u32, u32), usize> = BTreeMap::new();
    for (a, deps) in g.out.iter().enumerate() {
        let fa = &g.files[a];
        if fa.kind != FileKind::Code || repo.config.ignore.matches(&fa.path) {
            continue;
        }
        let Some(&ua) = unit_of.get(&fa.dir) else {
            continue;
        };
        for d in deps {
            let fb = &g.files[d.file as usize];
            if fb.kind != FileKind::Code || repo.config.ignore.matches(&fb.path) {
                continue;
            }
            let Some(&ub) = unit_of.get(&fb.dir) else {
                continue;
            };
            if ua != ub {
                *directed.entry((ua as u32, ub as u32)).or_default() += 1;
            }
        }
    }
    let mut undirected: BTreeMap<(u32, u32), f64> = BTreeMap::new();
    for (&(a, b), &w) in &directed {
        *undirected.entry((a.min(b), a.max(b))).or_default() += w as f64;
    }
    let edges: Vec<(u32, u32, f64)> = undirected.iter().map(|(&(a, b), &w)| (a, b, w)).collect();
    let connected: BTreeSet<u32> = edges.iter().flat_map(|&(a, b, _)| [a, b]).collect();
    let comm = louvain(units.len(), &edges);
    let q = modularity(units.len(), &edges, &comm);

    // Communities of connected modules.
    let mut groups: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (u, &c) in comm.iter().enumerate() {
        if connected.contains(&(u as u32)) {
            groups.entry(c).or_default().push(u);
        }
    }
    let unconnected: usize = (0..units.len())
        .filter(|u| !connected.contains(&(*u as u32)))
        .count();
    let comm_of_dir: BTreeMap<DirIx, u32> = unit_of
        .iter()
        .filter(|(_, u)| connected.contains(&(**u as u32)))
        .map(|(&d, &u)| (d, comm[u]))
        .collect();

    // Paths: top-down, a subtree whose code folders are all in one community is `dir/**`.
    let mut under: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); g.dirs.len()];
    let mut by_depth: Vec<DirIx> = (0..g.dirs.len() as DirIx).collect();
    by_depth.sort_by(|a, b| {
        g.dirs[*b as usize]
            .depth
            .cmp(&g.dirs[*a as usize].depth)
            .then(a.cmp(b))
    });
    for &d in &by_depth {
        if let Some(&c) = comm_of_dir.get(&d) {
            under[d as usize].insert(c);
        }
        if let Some(p) = g.dirs[d as usize].parent {
            let s = under[d as usize].clone();
            under[p as usize].extend(s);
        }
    }
    let mut paths: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    let mut stack = vec![crate::graph::ROOT];
    while let Some(d) = stack.pop() {
        let dir = &g.dirs[d as usize];
        let set = &under[d as usize];
        // A subtree whose layered folders are all in one community goes whole (folders
        // without dependencies inside it come along).
        if set.len() == 1 {
            let c = *set.iter().next().unwrap_or(&0);
            let glob = if dir.path.is_empty() {
                "**".to_string()
            } else {
                format!("{}/**", glob_escape(&dir.path))
            };
            paths.entry(c).or_default().push(glob);
            continue;
        }
        if let Some(&c) = comm_of_dir.get(&d) {
            let glob = if dir.path.is_empty() {
                "*".to_string()
            } else {
                format!("{}/*", glob_escape(&dir.path))
            };
            paths.entry(c).or_default().push(glob);
        }
        for &k in dir.dirs.iter().rev() {
            if !under[k as usize].is_empty() {
                stack.push(k);
            }
        }
    }

    // Layer metrics and order.
    let mut out_w: BTreeMap<(u32, u32), usize> = BTreeMap::new();
    for (&(a, b), &w) in &directed {
        let (ca, cb) = (comm[a as usize], comm[b as usize]);
        if ca != cb {
            *out_w.entry((ca, cb)).or_default() += w;
        }
    }
    let instability = |c: u32| -> f64 {
        let ce: usize = out_w
            .iter()
            .filter(|((a, _), _)| *a == c)
            .map(|(_, w)| w)
            .sum();
        let ca: usize = out_w
            .iter()
            .filter(|((_, b), _)| *b == c)
            .map(|(_, w)| w)
            .sum();
        if ca + ce == 0 {
            0.0
        } else {
            ce as f64 / (ca + ce) as f64
        }
    };
    let mut order: Vec<u32> = groups.keys().copied().collect();
    order.sort_by(|a, b| {
        instability(*b)
            .partial_cmp(&instability(*a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(b))
    });

    // Names: the deepest folder holding most (60 %) of the layer's code, unless that folder
    // holds as much of other layers (`src/`); then the folder of its biggest module. On a
    // clash, with the parent's name.
    let mut code_under = vec![0usize; g.dirs.len()];
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut names: BTreeMap<u32, String> = BTreeMap::new();
    for &c in &order {
        code_under.iter_mut().for_each(|x| *x = 0);
        let mut layer_files = 0;
        for (&d, &n) in &code_files {
            if comm_of_dir.get(&d) != Some(&c) {
                continue;
            }
            layer_files += n;
            let mut x = Some(d);
            while let Some(y) = x {
                code_under[y as usize] += n;
                x = g.dirs[y as usize].parent;
            }
        }
        let holder = (0..g.dirs.len())
            .filter(|&d| code_under[d] * 10 >= layer_files * 6)
            .max_by_key(|&d| (g.dirs[d].depth, std::cmp::Reverse(d)))
            .map(|d| d as DirIx)
            .filter(|&d| {
                let all: usize = code_files
                    .iter()
                    .filter(|(x, _)| g.dir_within(**x, d))
                    .map(|(_, n)| n)
                    .sum();
                // Not a folder that mostly holds other layers too (`src/`).
                d != crate::graph::ROOT && all * 2 < layer_files * 3
            });
        let big = holder.unwrap_or_else(|| {
            groups[&c]
                .iter()
                .copied()
                .max_by(|a, b| units[*a].files.cmp(&units[*b].files).then(b.cmp(a)))
                .map(|u| units[u].dir)
                .unwrap_or(crate::graph::ROOT)
        });
        let dir = &g.dirs[big as usize];
        let own = if dir.path.is_empty() {
            g.repo_name.as_str()
        } else {
            dir.name.as_str()
        };
        let base = sanitize(own);
        let parent = dir
            .parent
            .filter(|&p| p != crate::graph::ROOT)
            .map(|p| sanitize(&g.dirs[p as usize].name));
        let mut name = base.clone();
        if used.contains(&name)
            && let Some(p) = &parent
        {
            name = format!("{p}-{base}");
        }
        let stem = name.clone();
        let mut k = 2;
        while used.contains(&name) {
            name = format!("{stem}-{k}");
            k += 1;
        }
        used.insert(name.clone());
        names.insert(c, name);
    }

    // Dependencies between the proposed layers over the edges `check` sees: every active
    // file their globs match (config, data and scripts too), not only the code the
    // communities were made of. `allow` lists them all, so the proposal passes `check`.
    let mut proposed = Config {
        ignore: repo.config.ignore.clone(),
        ..Config::default()
    };
    for &c in &order {
        let globs = paths.get(&c).cloned().unwrap_or_default();
        proposed.layers.push(Layer {
            name: names[&c].clone(),
            // Escaped, so well-formed; a failure would only leave the layer without paths.
            paths: PathSet::new(&globs).unwrap_or_default(),
            allow: None,
            forbid: Vec::new(),
        });
    }
    let mut between: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    {
        let cx = Ctx::new(g, &proposed);
        let layer_of: Vec<Option<usize>> = g
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| {
                if cx.active[i] {
                    proposed.layer_of(&f.path)
                } else {
                    None
                }
            })
            .collect();
        for (a, deps) in cx.adj.iter().enumerate() {
            let Some(la) = layer_of[a] else { continue };
            for &b in deps {
                if let Some(lb) = layer_of[b as usize]
                    && la != lb
                {
                    *between.entry((la, lb)).or_default() += 1;
                }
            }
        }
    }

    let layers: Vec<ProposedLayer> = order
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let mut allow: Vec<(usize, String)> = Vec::new();
            let mut upward = Vec::new();
            for (&(a, b), &w) in &between {
                if a == i {
                    let to = proposed.layers[b].name.clone();
                    if b < i {
                        upward.push((to.clone(), w));
                    }
                    allow.push((b, to));
                }
            }
            allow.sort();
            let dirs: Vec<DirIx> = comm_of_dir
                .iter()
                .filter(|(_, x)| **x == c)
                .map(|(&d, _)| d)
                .collect();
            let files: u64 = dirs.iter().map(|d| code_files[d] as u64).sum();
            let loc: u64 = dirs
                .iter()
                .map(|&d| {
                    g.dirs[d as usize]
                        .files
                        .iter()
                        .filter(|&&f| g.files[f as usize].kind == FileKind::Code)
                        .map(|&f| g.files[f as usize].loc)
                        .sum::<u64>()
                })
                .sum();
            ProposedLayer {
                name: names[&c].clone(),
                paths: paths.get(&c).cloned().unwrap_or_default(),
                files,
                loc,
                allow: allow.into_iter().map(|(_, n)| n).collect(),
                upward,
                instability: (instability(c) * 100.0).round() / 100.0,
            }
        })
        .collect();
    let toml = render_toml(g, &layers, &repo.config.ignore.patterns, unconnected, q);
    let text = toml.clone();
    Answer {
        data: Inferred {
            layers,
            unconnected_folders: unconnected,
            modularity: (q * 1000.0).round() / 1000.0,
            toml,
        },
        text,
    }
}

fn render_toml(
    g: &RepoGraph,
    layers: &[ProposedLayer],
    ignore: &[String],
    unconnected: usize,
    q: f64,
) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "# cartograph.toml proposed by `cartograph infer` for {} (Louvain over folder dependencies,\n",
        if g.repo_name.is_empty() { "this repo" } else { &g.repo_name }
    ));
    s.push_str(&format!(
        "# modularity {q:.2}). Layers go from the most dependent (top) to the most depended on.\n"
    ));
    s.push_str(
        "# `allow` lists every dependency that exists today, so this passes `cartograph check`;\n",
    );
    s.push_str("# remove the ones marked \"against the order\" to start enforcing the layering.\n");
    s.push_str("# Format: docs/cartograph-toml.md\n");
    if unconnected > 0 {
        s.push_str(&format!(
            "# {unconnected} module(s) with code but no dependency in or out are in no layer.
"
        ));
    }
    if layers.len() <= 1 {
        s.push_str("# The graph does not split into separate groups; one layer only.\n");
    }
    if !ignore.is_empty() {
        // Before the first table, or TOML would make it a key of the last layer.
        s.push_str(&format!(
            "\n# Kept from the existing config.\nignore = [{}]\n",
            ignore
                .iter()
                .map(|p| toml_str(p))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for l in layers {
        s.push_str(&format!(
            "\n# {} files, {} LOC, instability {:.2}\n[[layers]]\nname = {}\npaths = [{}]\n",
            l.files,
            l.loc,
            l.instability,
            toml_str(&l.name),
            l.paths
                .iter()
                .map(|p| toml_str(p))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        if !l.allow.is_empty() {
            s.push_str(&format!(
                "allow = [{}]\n",
                l.allow
                    .iter()
                    .map(|a| toml_str(a))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        } else {
            s.push_str("allow = []\n");
        }
        for (to, w) in &l.upward {
            s.push_str(&format!(
                "# against the order: {} → {to} ({w} file pairs)\n",
                l.name
            ));
        }
    }
    s.push_str("\n[rules]\n# no_cycles = true\n# max_fan_out = 30\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn louvain_splits_two_cliques() {
        // Two triangles joined by one light edge.
        let edges = vec![
            (0, 1, 5.0),
            (1, 2, 5.0),
            (0, 2, 5.0),
            (3, 4, 5.0),
            (4, 5, 5.0),
            (3, 5, 5.0),
            (2, 3, 1.0),
        ];
        let c = louvain(6, &edges);
        assert_eq!(c[0], c[1]);
        assert_eq!(c[1], c[2]);
        assert_eq!(c[3], c[4]);
        assert_eq!(c[4], c[5]);
        assert_ne!(c[0], c[3]);
        // Deterministic.
        assert_eq!(c, louvain(6, &edges));
        assert!(modularity(6, &edges, &c) > 0.3);
    }

    #[test]
    fn louvain_handles_empty_graphs() {
        assert_eq!(louvain(3, &[]), vec![0, 1, 2]);
        assert!(louvain(0, &[]).is_empty());
    }
}
