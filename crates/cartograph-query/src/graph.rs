//! The in-memory dependency graph every query and finding runs on, built once from a
//! stored [`Graph`].
//!
//! - The hierarchy is the directory tree, derived from paths (root = `repo:`).
//! - A *dependency* is a file → file edge of kind `import`, `extends`, `implements`, `uses`
//!   or `call` (Godot signal connections). Several kinds between the same pair merge into one
//!   [`Dep`].
//! - An import that targets a **Dir** (a Go package, a Java/C# wildcard or namespace import,
//!   a Python namespace package) reaches that Dir's direct files: the edge is expanded here,
//!   once, to every direct file of the same language family (tests excluded unless the dir
//!   holds only tests), and the [`Dep`] is marked `via_dir`. Self-loops are dropped. Every
//!   query (fan-in, orphans, impact, path, cycles) therefore sees Dir imports as file edges.

use crate::kind::{FileKind, file_kind};
use cartograph_core::{Confidence, EdgeKind, Graph, NodeKind};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub type FileIx = u32;
pub type DirIx = u32;
pub type ExtIx = u32;

/// The root directory's index.
pub const ROOT: DirIx = 0;

/// Key of a File node's meta listing the targets it imports only for types (written by the
/// indexer, `cartograph_index::resolve::TYPE_ONLY_META`).
pub const TYPE_ONLY_META: &str = "type_only_imports";

/// Bits of [`Dep::kinds`].
pub mod dep_kind {
    pub const IMPORT: u8 = 1;
    pub const EXTENDS: u8 = 2;
    pub const IMPLEMENTS: u8 = 4;
    pub const USES: u8 = 8;
    pub const CALL: u8 = 16;
}

#[derive(Debug, Clone)]
pub struct FileNode {
    pub path: String,
    pub name: String,
    pub dir: DirIx,
    pub lang: Option<String>,
    pub loc: u64,
    pub kind: FileKind,
    /// Why this file is an entry point (main, bin, autoload, route…), if it is one.
    pub entry: Option<&'static str>,
    /// Unresolved imports of this file.
    pub unresolved: u32,
}

#[derive(Debug, Clone)]
pub struct DirNode {
    /// Repo-relative path (`""` for the root).
    pub path: String,
    pub name: String,
    pub parent: Option<DirIx>,
    /// Child dirs, sorted by path.
    pub dirs: Vec<DirIx>,
    /// Direct files, sorted by path.
    pub files: Vec<FileIx>,
    /// Descendant files.
    pub file_count: u64,
    pub loc: u64,
    /// 0 for the root, 1 for top-level dirs…
    pub depth: u32,
}

#[derive(Debug, Clone)]
pub struct ExtNode {
    /// Store id: `external:npm:react`.
    pub id: String,
    /// `npm:react`.
    pub name: String,
}

/// One file → file dependency (from the point of view of its owner list: `file` is the
/// other end).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dep {
    pub file: FileIx,
    /// Bitmask of [`dep_kind`].
    pub kinds: u8,
    /// Every contributing edge was an import of the target's directory.
    pub via_dir: bool,
    /// Every contributing edge was heuristic.
    pub heuristic: bool,
    /// Every contributing edge was a type-only import (TypeScript `import type`, `export
    /// type … from`, `import("…")` in a type): erased at compile time,
    /// so not a load-time dependency. Findings about runtime structure (cycles, Stable
    /// Dependencies, hubs, god modules) leave it out; `deps`, `impact` and `path` keep it.
    pub type_only: bool,
}

/// What an import site resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    File(FileIx),
    Dir(DirIx),
    Ext(ExtIx),
    Other,
}

#[derive(Debug, Clone)]
pub struct Site {
    pub line: u32,
    pub specifier: String,
    pub target: Target,
}

/// A node of the graph, by index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeRef {
    Dir(DirIx),
    File(FileIx),
    Ext(ExtIx),
}

/// Entry-point data from manifests and Godot projects, by file path.
#[derive(Debug, Default)]
struct Entries {
    godot: HashMap<String, &'static str>,
    package_dirs: BTreeSet<String>,
    /// Folders of `package.json` files: a JS file at their top runs only if the manifest
    /// names it.
    npm_dirs: BTreeSet<String>,
    /// Files a manifest runs or exposes (`package.json` main, bin, exports, scripts).
    manifest: BTreeSet<String>,
}

pub struct RepoGraph {
    pub repo_name: String,
    pub files: Vec<FileNode>,
    pub dirs: Vec<DirNode>,
    pub externals: Vec<ExtNode>,
    /// Dependencies of each file, sorted by target.
    pub out: Vec<Vec<Dep>>,
    /// Dependents of each file, sorted by source (`Dep::file` is the source).
    pub inn: Vec<Vec<Dep>>,
    /// External packages each file imports.
    pub ext_out: Vec<Vec<ExtIx>>,
    /// Files importing each external package.
    pub ext_in: Vec<Vec<FileIx>>,
    /// Import sites of each file, by line.
    pub sites: Vec<Vec<Site>>,
    /// Number of file → dir imports expanded to file edges.
    pub dir_imports: u64,
    /// What each folder is (as the web plan says it, `web/src/model/kind.ts` `kindsOf`).
    pub dir_kind: Vec<FileKind>,
    file_ix: HashMap<String, FileIx>,
    dir_ix: HashMap<String, DirIx>,
    ext_ix: HashMap<String, ExtIx>,
    /// Manifest and Godot entry data, to work out entries again from inside a folder.
    entries: Entries,
}

fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn last_segment(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Languages that see each other through a directory import.
fn family(lang: Option<&str>) -> &str {
    match lang {
        Some("typescript" | "tsx" | "javascript" | "jsx") => "js",
        Some("java" | "kotlin") => "jvm",
        Some("c" | "cpp") => "c",
        Some(other) => other,
        None => "",
    }
}

fn dep_bit(kind: EdgeKind) -> Option<u8> {
    Some(match kind {
        EdgeKind::Import => dep_kind::IMPORT,
        EdgeKind::Extends => dep_kind::EXTENDS,
        EdgeKind::Implements => dep_kind::IMPLEMENTS,
        EdgeKind::Uses => dep_kind::USES,
        EdgeKind::Call => dep_kind::CALL,
        _ => return None,
    })
}

/// Names of the kinds in a [`Dep::kinds`] mask.
pub fn kind_names(mask: u8) -> Vec<&'static str> {
    let mut v = Vec::new();
    for (bit, name) in [
        (dep_kind::IMPORT, "import"),
        (dep_kind::EXTENDS, "extends"),
        (dep_kind::IMPLEMENTS, "implements"),
        (dep_kind::USES, "uses"),
        (dep_kind::CALL, "call"),
    ] {
        if mask & bit != 0 {
            v.push(name);
        }
    }
    v
}

impl RepoGraph {
    /// Build from a stored graph. Never panics on odd data (dangling edges are skipped).
    pub fn build(graph: &Graph) -> RepoGraph {
        let repo_name = graph
            .node("repo:")
            .map(|n| n.name.clone())
            .unwrap_or_default();

        // Entry data from Godot projects and package manifests.
        let mut entries = Entries::default();
        // `(package folder, entry as written)`.
        let mut manifest_raw: Vec<(&str, &str)> = Vec::new();
        for n in &graph.nodes {
            if n.kind == NodeKind::Package
                && let Some(d) = n.meta["dir"].as_str()
            {
                entries.package_dirs.insert(d.to_string());
                if n.meta["manifest"].as_str() == Some("npm") {
                    entries.npm_dirs.insert(d.to_string());
                }
                for e in n.meta["details"]["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    if let Some(e) = e.as_str() {
                        manifest_raw.push((d, e));
                    }
                }
            }
            if let Some(list) = n.meta["godot"]["entry_points"].as_array() {
                for e in list {
                    let reason = match e["kind"].as_str() {
                        Some("autoload") => "godot autoload",
                        _ => "godot main scene",
                    };
                    if let Some(t) = e["target"].as_str().and_then(|t| t.strip_prefix("file:")) {
                        entries.godot.insert(t.to_string(), reason);
                    }
                }
            }
        }

        // Files, sorted by path.
        let mut file_nodes: Vec<_> = graph.nodes_of(NodeKind::File).collect();
        file_nodes.sort_by(|a, b| a.path.cmp(&b.path));
        let paths: BTreeSet<&str> = file_nodes.iter().map(|n| n.path.as_str()).collect();
        entries.manifest = manifest_raw
            .iter()
            .flat_map(|(d, e)| {
                cartograph_core::manifest_entry::entry_files(d, e, |p| paths.contains(p))
            })
            .collect();
        // Dirs: every Dir node plus every ancestor of every file.
        let mut dir_paths: BTreeSet<String> = BTreeSet::new();
        dir_paths.insert(String::new());
        let add_chain = |mut p: &str, set: &mut BTreeSet<String>| {
            while !p.is_empty() && set.insert(p.to_string()) {
                p = parent_of(p);
            }
        };
        for n in graph.nodes_of(NodeKind::Dir) {
            add_chain(&n.path, &mut dir_paths);
        }
        for f in &file_nodes {
            add_chain(parent_of(&f.path), &mut dir_paths);
        }
        // Sorted by path: the root ("") comes first; parents are found through the map.
        let mut dirs: Vec<DirNode> = dir_paths
            .iter()
            .map(|p| DirNode {
                path: p.clone(),
                name: last_segment(p).to_string(),
                parent: None,
                dirs: Vec::new(),
                files: Vec::new(),
                file_count: 0,
                loc: 0,
                depth: if p.is_empty() {
                    0
                } else {
                    p.split('/').count() as u32
                },
            })
            .collect();
        let dir_ix: HashMap<String, DirIx> = dirs
            .iter()
            .enumerate()
            .map(|(i, d)| (d.path.clone(), i as DirIx))
            .collect();
        for i in 1..dirs.len() {
            let parent = dir_ix
                .get(parent_of(&dirs[i].path))
                .copied()
                .unwrap_or(ROOT);
            dirs[i].parent = Some(parent);
            dirs[parent as usize].dirs.push(i as DirIx);
        }

        let mut files = Vec::with_capacity(file_nodes.len());
        let mut file_ix = HashMap::with_capacity(file_nodes.len());
        for (i, n) in file_nodes.iter().enumerate() {
            let dir = dir_ix.get(parent_of(&n.path)).copied().unwrap_or(ROOT);
            dirs[dir as usize].files.push(i as FileIx);
            let kind = file_kind(&n.path);
            let entry = entry_reason(&n.path, n.lang.as_deref(), kind, &entries, 0);
            files.push(FileNode {
                path: n.path.clone(),
                name: last_segment(&n.path).to_string(),
                dir,
                lang: n.lang.clone(),
                loc: n.loc,
                kind,
                entry,
                unresolved: 0,
            });
            file_ix.insert(n.path.clone(), i as FileIx);
            // Totals up the chain.
            let mut d = Some(dir);
            while let Some(x) = d {
                dirs[x as usize].file_count += 1;
                dirs[x as usize].loc += n.loc;
                d = dirs[x as usize].parent;
            }
        }
        for u in &graph.unresolved {
            if let Some(&f) = file_ix.get(&u.file) {
                files[f as usize].unresolved += 1;
            }
        }

        let mut ext_nodes: Vec<_> = graph.nodes_of(NodeKind::External).collect();
        ext_nodes.sort_by(|a, b| a.id.cmp(&b.id));
        let externals: Vec<ExtNode> = ext_nodes
            .iter()
            .map(|n| ExtNode {
                id: n.id.clone(),
                name: n
                    .id
                    .strip_prefix("external:")
                    .unwrap_or(&n.path)
                    .to_string(),
            })
            .collect();
        let ext_ix: HashMap<String, ExtIx> = externals
            .iter()
            .enumerate()
            .map(|(i, e)| (e.id.clone(), i as ExtIx))
            .collect();

        let lookup_file = |id: &str| {
            id.strip_prefix("file:")
                .and_then(|p| file_ix.get(p).copied())
        };
        let lookup_dir = |id: &str| {
            if id == "repo:" {
                Some(ROOT)
            } else {
                id.strip_prefix("dir:").and_then(|p| dir_ix.get(p).copied())
            }
        };

        // Import edges the indexer marked type-only, as (source file, target id).
        let mut type_only: BTreeSet<(FileIx, &str)> = BTreeSet::new();
        for (i, n) in file_nodes.iter().enumerate() {
            for t in n.meta[TYPE_ONLY_META].as_array().into_iter().flatten() {
                if let Some(t) = t.as_str() {
                    type_only.insert((i as FileIx, t));
                }
            }
        }

        // Dependencies.
        let mut pairs: BTreeMap<(FileIx, FileIx), Dep> = BTreeMap::new();
        let mut ext_pairs: BTreeSet<(FileIx, ExtIx)> = BTreeSet::new();
        let mut dir_imports = 0u64;
        for e in &graph.edges {
            let Some(src) = lookup_file(&e.src) else {
                continue;
            };
            if e.kind == EdgeKind::ImportExternal {
                if let Some(&x) = ext_ix.get(&e.dst) {
                    ext_pairs.insert((src, x));
                }
                continue;
            }
            let Some(bit) = dep_bit(e.kind) else {
                continue;
            };
            let heuristic = e.confidence == Confidence::Heuristic;
            let typed = e.kind == EdgeKind::Import && type_only.contains(&(src, e.dst.as_str()));
            let mut add = |dst: FileIx, via_dir: bool| {
                if dst == src {
                    return;
                }
                let d = pairs.entry((src, dst)).or_insert(Dep {
                    file: dst,
                    kinds: 0,
                    via_dir: true,
                    heuristic: true,
                    type_only: true,
                });
                d.kinds |= bit;
                d.via_dir &= via_dir;
                d.heuristic &= heuristic;
                d.type_only &= typed;
            };
            if let Some(dst) = lookup_file(&e.dst) {
                add(dst, false);
            } else if let Some(dir) = lookup_dir(&e.dst) {
                dir_imports += 1;
                for dst in dir_targets(
                    &files,
                    &dirs[dir as usize],
                    files[src as usize].lang.as_deref(),
                ) {
                    add(dst, true);
                }
            }
        }
        let n = files.len();
        let mut out: Vec<Vec<Dep>> = vec![Vec::new(); n];
        let mut inn: Vec<Vec<Dep>> = vec![Vec::new(); n];
        for (&(src, dst), d) in &pairs {
            out[src as usize].push(*d);
            inn[dst as usize].push(Dep { file: src, ..*d });
        }
        for v in &mut inn {
            v.sort_by_key(|d| d.file);
        }
        let mut ext_out: Vec<Vec<ExtIx>> = vec![Vec::new(); n];
        let mut ext_in: Vec<Vec<FileIx>> = vec![Vec::new(); externals.len()];
        for &(f, x) in &ext_pairs {
            ext_out[f as usize].push(x);
            ext_in[x as usize].push(f);
        }

        let mut sites: Vec<Vec<Site>> = vec![Vec::new(); n];
        for s in &graph.imports {
            let Some(&f) = file_ix.get(&s.file) else {
                continue;
            };
            let target = if let Some(t) = lookup_file(&s.target) {
                Target::File(t)
            } else if let Some(d) = lookup_dir(&s.target) {
                Target::Dir(d)
            } else if let Some(&x) = ext_ix.get(&s.target) {
                Target::Ext(x)
            } else {
                Target::Other
            };
            sites[f as usize].push(Site {
                line: s.line,
                specifier: s.specifier.clone(),
                target,
            });
        }
        for v in &mut sites {
            v.sort_by(|a, b| (a.line, &a.specifier).cmp(&(b.line, &b.specifier)));
        }

        let dir_kind = dir_kinds(&files, &dirs);
        RepoGraph {
            repo_name,
            dir_kind,
            files,
            dirs,
            externals,
            out,
            inn,
            ext_out,
            ext_in,
            sites,
            dir_imports,
            file_ix,
            dir_ix,
            ext_ix,
            entries,
        }
    }

    /// Why `f` is an entry point when looking from inside `scope`: [`FileNode::entry`],
    /// except that docs and samples folders (`examples/`, `docs/`, `demo/`…) count only
    /// below `scope`. An overview of `examples/web-app` is about that example, whose
    /// `main.ts` is an entry there although it is sample code for the repo.
    pub fn entry_within(&self, f: FileIx, scope: DirIx) -> Option<&'static str> {
        let file = &self.files[f as usize];
        if scope == ROOT || file.entry != Some(ASSET_OR_EXAMPLE) {
            return file.entry;
        }
        let base = self.dirs[scope as usize].depth as usize;
        entry_reason(
            &file.path,
            file.lang.as_deref(),
            file.kind,
            &self.entries,
            base,
        )
    }

    /// Folders of the packages that manifests declare (`""` = the repo root).
    pub fn package_dirs(&self) -> impl Iterator<Item = &str> {
        self.entries.package_dirs.iter().map(String::as_str)
    }

    pub fn file_by_path(&self, path: &str) -> Option<FileIx> {
        self.file_ix.get(path).copied()
    }

    pub fn dir_by_path(&self, path: &str) -> Option<DirIx> {
        self.dir_ix.get(path).copied()
    }

    pub fn ext_by_id(&self, id: &str) -> Option<ExtIx> {
        self.ext_ix.get(id).copied()
    }

    /// Stable id: `file:<path>`, `dir:<path>` (`repo:` for the root), or the stored external
    /// id (`external:npm:react`).
    pub fn id(&self, n: NodeRef) -> String {
        match n {
            NodeRef::File(f) => format!("file:{}", self.files[f as usize].path),
            NodeRef::Dir(ROOT) => "repo:".to_string(),
            NodeRef::Dir(d) => format!("dir:{}", self.dirs[d as usize].path),
            NodeRef::Ext(x) => self.externals[x as usize].id.clone(),
        }
    }

    /// Compact display: a file's path, a dir's path with a trailing `/` (`./` for the root),
    /// an external's `eco:name`. Every display form is accepted back as input.
    pub fn label(&self, n: NodeRef) -> String {
        match n {
            NodeRef::File(f) => self.files[f as usize].path.clone(),
            NodeRef::Dir(ROOT) => "./".to_string(),
            NodeRef::Dir(d) => format!("{}/", self.dirs[d as usize].path),
            NodeRef::Ext(x) => self.externals[x as usize].name.clone(),
        }
    }

    pub fn path(&self, f: FileIx) -> &str {
        &self.files[f as usize].path
    }

    /// Whether dir `d` is `ancestor` or below it.
    pub fn dir_within(&self, mut d: DirIx, ancestor: DirIx) -> bool {
        loop {
            if d == ancestor {
                return true;
            }
            match self.dirs[d as usize].parent {
                Some(p) => d = p,
                None => return false,
            }
        }
    }

    /// Whether file `f` is inside dir `d` (at any depth).
    pub fn file_within(&self, f: FileIx, d: DirIx) -> bool {
        self.dir_within(self.files[f as usize].dir, d)
    }

    /// Every file under `d`, in path order.
    pub fn files_under(&self, d: DirIx) -> Vec<FileIx> {
        let mut out = Vec::new();
        let mut stack = vec![d];
        while let Some(x) = stack.pop() {
            let dir = &self.dirs[x as usize];
            out.extend_from_slice(&dir.files);
            stack.extend(dir.dirs.iter().rev());
        }
        out.sort_unstable();
        out
    }

    /// Files a node stands for: itself, or every file under a dir. Externals have none.
    pub fn files_of(&self, n: NodeRef) -> Vec<FileIx> {
        match n {
            NodeRef::File(f) => vec![f],
            NodeRef::Dir(d) => self.files_under(d),
            NodeRef::Ext(_) => Vec::new(),
        }
    }

    /// Ancestor of dir `d` at depth `depth` (or `d` itself if it is shallower).
    pub fn dir_at_depth(&self, mut d: DirIx, depth: u32) -> DirIx {
        while self.dirs[d as usize].depth > depth {
            match self.dirs[d as usize].parent {
                Some(p) => d = p,
                None => break,
            }
        }
        d
    }

    /// The import site that explains `src → dst` (a direct import of the file, or of its
    /// directory), if the edge came from an import.
    pub fn site(&self, src: FileIx, dst: FileIx) -> Option<&Site> {
        let dir = self.files[dst as usize].dir;
        self.sites[src as usize]
            .iter()
            .find(|s| s.target == Target::File(dst))
            .or_else(|| {
                self.sites[src as usize]
                    .iter()
                    .find(|s| s.target == Target::Dir(dir))
            })
    }

    /// The dependency `src → dst`, if there is one.
    pub fn dep(&self, src: FileIx, dst: FileIx) -> Option<&Dep> {
        self.out[src as usize].iter().find(|d| d.file == dst)
    }

    /// Whether `src → dst` is a type-only dependency.
    pub fn type_only(&self, src: FileIx, dst: FileIx) -> bool {
        self.dep(src, dst).is_some_and(|d| d.type_only)
    }

    /// `src:line → dst` (or `src → dst (extends)` when no import site explains it), with
    /// ` (type)` for a type-only dependency.
    pub fn edge_evidence(&self, src: FileIx, dst: FileIx) -> String {
        let e = self.edge_text(src, dst);
        if self.type_only(src, dst) {
            format!("{e} (type)")
        } else {
            e
        }
    }

    fn edge_text(&self, src: FileIx, dst: FileIx) -> String {
        let s = self.path(src);
        let d = self.path(dst);
        match self.site(src, dst) {
            Some(site) => format!("{s}:{} → {d}", site.line),
            None => {
                let kinds = self.out[src as usize]
                    .iter()
                    .find(|x| x.file == dst)
                    .map_or(Vec::new(), |x| kind_names(x.kinds));
                if kinds.is_empty() {
                    format!("{s} → {d}")
                } else {
                    format!("{s} → {d} ({})", kinds.join(","))
                }
            }
        }
    }

    /// Distinct files depending on `f`.
    pub fn fan_in(&self, f: FileIx) -> usize {
        self.inn[f as usize].len()
    }

    /// Distinct files `f` depends on.
    pub fn fan_out(&self, f: FileIx) -> usize {
        self.out[f as usize].len()
    }

    pub fn total_loc(&self) -> u64 {
        self.dirs[ROOT as usize].loc
    }
}

/// Kinds of every folder, bottom-up, as `kindsOf` in `web/src/model/kind.ts`: each child
/// (file, or folder with files) votes with its kind; ties go to the kind with more files
/// under the folder, then to the first in [`FileKind::ALL`]. Data wins only when most files
/// under the folder are data. An empty folder takes the kind a source file inside it would
/// have.
fn dir_kinds(files: &[FileNode], dirs: &[DirNode]) -> Vec<FileKind> {
    let mut kind = vec![FileKind::Code; dirs.len()];
    let mut mix = vec![[0usize; 5]; dirs.len()];
    let mut order: Vec<usize> = (0..dirs.len()).collect();
    order.sort_by(|a, b| dirs[*b].depth.cmp(&dirs[*a].depth).then(a.cmp(b)));
    for d in order {
        let mut votes = [0usize; 5];
        let mut m = [0usize; 5];
        for &f in &dirs[d].files {
            let k = files[f as usize].kind as usize;
            votes[k] += 1;
            m[k] += 1;
        }
        for &c in &dirs[d].dirs {
            let cm = mix[c as usize];
            if cm.iter().any(|&x| x > 0) {
                votes[kind[c as usize] as usize] += 1;
            }
            for i in 0..5 {
                m[i] += cm[i];
            }
        }
        // Config, lockfiles and docs pile up next to the code they serve (`web/` with a
        // dozen config files and one `src/`): data wins the vote only when most files under
        // the folder are data too.
        let total: usize = m.iter().sum();
        let data = FileKind::Data as usize;
        if votes[data] > 0
            && m[data] * 2 <= total
            && votes.iter().enumerate().any(|(k, &v)| k != data && v > 0)
        {
            votes[data] = 0;
        }
        let top = votes.iter().copied().max().unwrap_or(0);
        kind[d] = if top == 0 {
            crate::kind::file_kind(&format!("{}/_.ts", dirs[d].path))
        } else {
            let mut best: Option<FileKind> = None;
            for k in FileKind::ALL {
                if votes[k as usize] == top && best.is_none_or(|b| m[k as usize] > m[b as usize]) {
                    best = Some(k);
                }
            }
            best.unwrap_or(FileKind::Code)
        };
        mix[d] = m;
    }
    kind
}

/// Files a directory import reaches: the dir's direct files of the importer's language
/// family, without tests unless only tests match; every direct file if none matches.
fn dir_targets(files: &[FileNode], dir: &DirNode, lang: Option<&str>) -> Vec<FileIx> {
    let fam = family(lang);
    let same: Vec<FileIx> = dir
        .files
        .iter()
        .copied()
        .filter(|&f| !fam.is_empty() && family(files[f as usize].lang.as_deref()) == fam)
        .collect();
    let pool = if same.is_empty() {
        dir.files
            .iter()
            .copied()
            .filter(|&f| files[f as usize].lang.is_some())
            .collect()
    } else {
        same
    };
    let non_test: Vec<FileIx> = pool
        .iter()
        .copied()
        .filter(|&f| files[f as usize].kind != FileKind::Test)
        .collect();
    if non_test.is_empty() { pool } else { non_test }
}

/// First name parts of the files of JS tools at the top of a package
/// (`vitest.scripts.ts`, `jest.setup.js`, `gulpfile.js`), loaded by the tool.
const TOOL_STEMS: &[&str] = &[
    "vitest",
    "jest",
    "playwright",
    "cypress",
    "karma",
    "webpack",
    "rollup",
    "vite",
    "babel",
    "eslint",
    "prettier",
    "stylelint",
    "postcss",
    "tailwind",
    "tsup",
    "esbuild",
    "commitlint",
    "gulpfile",
    "gruntfile",
    "nodemon",
    "ecosystem",
];

/// Names that mark a JS/TS/Python file as an entry point when nothing imports it.
const ENTRY_STEMS: &[&str] = &[
    "main",
    "index",
    "app",
    "server",
    "cli",
    "preload",
    "worker",
    "background",
    "content",
    "entry",
    "bootstrap",
    "start",
    "manage",
    "setup",
    "wsgi",
    "asgi",
    "__main__",
    "__init__",
    "conftest",
    "middleware",
    "instrumentation",
    "sw",
    "service-worker",
];
/// Names that make a file at a package root the package's entry ("package entry", ranked
/// with crate roots). Narrower than [`ENTRY_STEMS`]: a root `setup.ts`, `conftest.py` or
/// `middleware.ts` runs, but it is not what the package exposes. `lib` and `mod` (Deno) are
/// entries only at a package root, so they are not in [`ENTRY_STEMS`].
const PACKAGE_ENTRY_STEMS: &[&str] = &[
    "index", "main", "app", "server", "cli", "lib", "mod", "entry", "__main__",
];
/// File-system routes (Next.js, Remix, SvelteKit, Nuxt…).
const ROUTE_STEMS: &[&str] = &[
    "page",
    "layout",
    "route",
    "loading",
    "error",
    "not-found",
    "template",
    "default",
    "global-error",
    "head",
    "opengraph-image",
    "sitemap",
    "robots",
    "manifest",
];
const ROUTE_DIRS: &[&str] = &["pages", "app", "routes", "api"];
/// The entry reason of served assets and of docs and sample code.
const ASSET_OR_EXAMPLE: &str = "asset or example";

/// Why a file counts as an entry point, if it does. Generic rules: entry names, file-system
/// routing, Rust crate roots and targets, C/C++ translation units, Godot autoloads and main
/// scene, package roots, declaration files, stories. Docs and samples folders count from the
/// `base`-th folder of the path on (0: from the repo root; see [`RepoGraph::entry_within`]).
fn entry_reason(
    path: &str,
    lang: Option<&str>,
    kind: FileKind,
    entries: &Entries,
    base: usize,
) -> Option<&'static str> {
    if let Some(r) = entries.godot.get(path) {
        return Some(r);
    }
    if entries.manifest.contains(path) {
        return Some("manifest entry");
    }
    if kind != FileKind::Code {
        return Some(match kind {
            FileKind::Test => "test",
            FileKind::Script => "script",
            _ => "config/data",
        });
    }
    let name = last_segment(path);
    let lower = name.to_ascii_lowercase();
    let stem = lower.split('.').next().unwrap_or("");
    let segs: Vec<&str> = path.split('/').collect();
    let dirs = &segs[..segs.len().saturating_sub(1)];
    // Served or bundled as is (a script tag, a resource loader), or sample code.
    // Docs and samples only near the top (of the repo, or of the folder looked at from):
    // `org/example/` deep in a Java tree is a package.
    if dirs.iter().enumerate().any(|(i, d)| {
        let d = d.to_ascii_lowercase();
        matches!(
            d.as_str(),
            "resources" | "static" | "public" | "wwwroot" | "vendor" | "third_party"
        ) || ((base..base + 2).contains(&i)
            && matches!(
                d.as_str(),
                "docs" | "doc" | "examples" | "example" | "samples" | "demo" | "demos"
            ))
    }) {
        return Some(ASSET_OR_EXAMPLE);
    }
    match lang {
        Some("rust") => {
            if ["main.rs", "lib.rs", "build.rs"].contains(&name) {
                return Some("crate root");
            }
            if dirs
                .iter()
                .any(|d| ["bin", "examples", "benches", "tests", "fuzz_targets"].contains(d))
            {
                return Some("cargo target");
            }
            None
        }
        Some("c" | "cpp") => {
            let header = [".h", ".hh", ".hpp", ".hxx", ".inl"]
                .iter()
                .any(|e| lower.ends_with(e));
            (!header).then_some("translation unit")
        }
        Some("typescript" | "tsx" | "javascript" | "jsx" | "python") => {
            if lower.ends_with(".d.ts") || lower.ends_with(".d.mts") || lower.ends_with(".d.cts") {
                return Some("type declarations");
            }
            if lower.contains(".stories.") || lower.contains(".story.") {
                return Some("story");
            }
            if lower.contains(".worker.") || lower.contains(".config.") {
                return Some("tool entry");
            }
            if ["preload", "worker"].iter().any(|w| stem.ends_with(w)) {
                return Some("entry name");
            }
            let parent = parent_of(path);
            let package_root = entries.package_dirs.contains(parent);
            // A package root's index/main is the package entry; its setup, conftest,
            // middleware… are not, and stay top-level scripts.
            if package_root && PACKAGE_ENTRY_STEMS.contains(&stem) {
                return Some("package entry");
            }
            let top = parent.is_empty() || package_root;
            // A tool's own file at the top (`vitest.workspace.ts`, `jest.setup.js`,
            // `gulpfile.js`): the tool loads it.
            if top && TOOL_STEMS.contains(&stem) {
                return Some("tool entry");
            }
            // A loose file at the top of a package (or the repo) is run directly; at the top
            // of an npm package, only if `package.json` names it (a "manifest entry",
            // above) or its name is an entry name (`middleware.ts`, `sw.js`): any other JS
            // file there is a one-off (`test-esm.js`, `try.mjs`).
            if top {
                let js = !matches!(lang, Some("python"));
                let one_off =
                    js && entries.npm_dirs.contains(parent) && !ENTRY_STEMS.contains(&stem);
                return (!one_off).then_some("top-level script");
            }
            // Elsewhere the name says so too (Electron main/preload, Vite main.tsx, Python
            // __main__).
            if ENTRY_STEMS.contains(&stem) {
                return Some("entry name");
            }
            let routed = dirs.iter().any(|d| ROUTE_DIRS.contains(d));
            if routed && (ROUTE_STEMS.contains(&stem) || stem.starts_with('+')) {
                return Some("route");
            }
            if dirs.iter().any(|d| *d == "pages" || *d == "routes") {
                return Some("route");
            }
            None
        }
        Some("java" | "kotlin" | "csharp") => {
            let main = stem == "main"
                || stem == "program"
                || stem == "startup"
                || (stem.ends_with("application") && stem.len() > "application".len());
            main.then_some("entry name")
        }
        Some("gdscript" | "godot-scene" | "godot-resource" | "godot-project") => {
            if name == "project.godot" || lower.ends_with(".import") {
                return Some("godot project");
            }
            None
        }
        _ => None,
    }
}

/// A short description of a dependency for JSON output.
#[derive(Debug, Clone, Serialize)]
pub struct EdgeRef {
    pub src: String,
    pub dst: String,
    /// `src:line` of the import, when one explains the edge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub via_dir: bool,
    /// A type-only dependency (see [`Dep::type_only`]).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub type_only: bool,
}

impl RepoGraph {
    pub fn edge_ref(&self, src: FileIx, dst: FileIx) -> EdgeRef {
        let dep = self.dep(src, dst);
        EdgeRef {
            src: self.id(NodeRef::File(src)),
            dst: self.id(NodeRef::File(dst)),
            line: self.site(src, dst).map(|s| s.line),
            via_dir: dep.is_some_and(|d| d.via_dir),
            type_only: dep.is_some_and(|d| d.type_only),
        }
    }
}
