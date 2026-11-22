//! Derived edges on top of the file-level import graph.
//!
//! - **Package → Dir** `Contains` (`Exact`): every package contains the Dir node of its
//!   manifest's directory. A package at the repo root has no Dir node (the root is the Repo
//!   node), so it gets no such edge. A Dir can therefore have two `Contains` parents: its
//!   parent Dir (or the Repo) and a Package.
//! - **Dir → Dir** `DependsOn`: between the *immediate* containers of the two ends of every
//!   file → file `Import` edge (a top-level file's container is the Repo node; a namespace
//!   package target is its own Dir). Ancestors are not rolled up; a viewer zoomed out sums
//!   the edges of the dirs it merges.
//! - **Package → Package** `DependsOn`: between the packages owning the two files. A file
//!   belongs to the nearest package (walking up from its dir) whose manifest matches the
//!   file's ecosystem (`package.json` for TS/JS, `pyproject.toml` / `setup.py` /
//!   `requirements.txt` for Python…); if none matches, to the nearest package of any kind;
//!   among several in one dir, the one with the smallest manifest path.
//!
//! `weight` = number of distinct file → file `Import` edges crossing; self-loops are
//! skipped. `confidence` = the most certain confidence among those import edges.

use crate::container_id;
use cartograph_core::{Confidence, Edge, EdgeKind, Graph, NodeKind, node_id};
use std::collections::{BTreeMap, HashMap};

/// Whether a manifest kind belongs to the ecosystem of a language.
fn ecosystem_matches(lang: Option<&str>, manifest: &str) -> bool {
    let kinds: &[&str] = match lang {
        Some("typescript" | "tsx" | "javascript" | "jsx") => &["npm"],
        Some("python") => &["pyproject", "setup-py", "requirements"],
        Some("rust") => &["cargo"],
        Some("go") => &["go-mod"],
        Some("java") => &["maven", "gradle"],
        Some("csharp") => &["csproj"],
        Some("gdscript" | "godot-scene" | "godot-resource" | "godot-project") => &["godot"],
        Some("c" | "cpp") => &["cmake"],
        _ => &[],
    };
    kinds.contains(&manifest)
}

/// Which package owns a file (see the module docs for the rule).
pub struct PackageOwners<'g> {
    /// dir → (manifest kind, package id), sorted by id.
    by_dir: HashMap<&'g str, Vec<(&'g str, &'g str)>>,
}

impl<'g> PackageOwners<'g> {
    /// Index the Package nodes of `graph`.
    pub fn new(graph: &'g Graph) -> PackageOwners<'g> {
        let mut by_dir: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
        for n in graph.nodes_of(NodeKind::Package) {
            let dir = n.meta["dir"].as_str().unwrap_or("");
            let kind = n.meta["manifest"].as_str().unwrap_or("");
            by_dir.entry(dir).or_default().push((kind, n.id.as_str()));
        }
        for v in by_dir.values_mut() {
            v.sort_by(|a, b| a.1.cmp(b.1));
        }
        PackageOwners { by_dir }
    }

    /// Id of the package owning the file at `path` (language `lang`), if any.
    pub fn of(&self, path: &str, lang: Option<&str>) -> Option<&'g str> {
        let mut nearest_any: Option<&'g str> = None;
        let mut dir = crate::resolve::parent(path);
        loop {
            if let Some(cands) = self.by_dir.get(dir) {
                if let Some((_, id)) = cands.iter().find(|(k, _)| ecosystem_matches(lang, k)) {
                    return Some(id);
                }
                if nearest_any.is_none() {
                    nearest_any = cands.first().map(|(_, id)| *id);
                }
            }
            if dir.is_empty() {
                return nearest_any;
            }
            dir = crate::resolve::parent(dir);
        }
    }
}

/// Add Package → Dir containment and the derived `DependsOn` edges. Expects a normalized
/// graph (duplicate import edges merged).
pub fn derive(graph: &mut Graph) {
    let mut new_edges = Vec::new();
    {
        for n in graph.nodes_of(NodeKind::Package) {
            let dir = n.meta["dir"].as_str().unwrap_or("");
            let dir_id = node_id(NodeKind::Dir, dir);
            if !dir.is_empty() && graph.node(&dir_id).is_some() {
                new_edges.push(Edge::new(
                    n.id.clone(),
                    dir_id,
                    EdgeKind::Contains,
                    Confidence::Exact,
                ));
            }
        }
        let packages = PackageOwners::new(graph);
        let langs: HashMap<&str, Option<&str>> = graph
            .nodes_of(NodeKind::File)
            .map(|n| (n.path.as_str(), n.lang.as_deref()))
            .collect();

        let mut dirs: BTreeMap<(String, String), (u64, Confidence)> = BTreeMap::new();
        let mut pkgs: BTreeMap<(String, String), (u64, Confidence)> = BTreeMap::new();
        let bump = |map: &mut BTreeMap<(String, String), (u64, Confidence)>,
                    a: String,
                    b: String,
                    c: Confidence| {
            if a != b {
                let e = map.entry((a, b)).or_insert((0, c));
                e.0 += 1;
                e.1 = e.1.min(c);
            }
        };
        for e in graph.edges.iter().filter(|e| e.kind == EdgeKind::Import) {
            let Some(src) = e.src.strip_prefix("file:") else {
                continue;
            };
            let src_lang = langs.get(src).copied().flatten();
            let (dst_container, dst_path, dst_lang) = if let Some(f) = e.dst.strip_prefix("file:") {
                (container_id(f), f, langs.get(f).copied().flatten())
            } else if let Some(d) = e.dst.strip_prefix("dir:") {
                // A namespace package: the dir itself, in the importer's ecosystem. The
                // package lookup gets a path inside the dir so it starts at the dir.
                (e.dst.clone(), d, src_lang)
            } else {
                continue;
            };
            bump(&mut dirs, container_id(src), dst_container, e.confidence);
            let dst_probe = if e.dst.starts_with("dir:") {
                format!("{dst_path}/_")
            } else {
                dst_path.to_string()
            };
            if let (Some(a), Some(b)) = (
                packages.of(src, src_lang),
                packages.of(&dst_probe, dst_lang),
            ) {
                bump(&mut pkgs, a.to_string(), b.to_string(), e.confidence);
            }
        }
        for ((a, b), (w, c)) in dirs.into_iter().chain(pkgs) {
            new_edges.push(Edge {
                weight: w,
                ..Edge::new(a, b, EdgeKind::DependsOn, c)
            });
        }
    }
    graph.edges.extend(new_edges);
    graph.normalize();
}
