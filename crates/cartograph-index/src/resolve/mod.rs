//! Import resolution: raw imports → `Import` / `ImportExternal` edges, External nodes,
//! import sites and unresolved records. Every import ends up either as a site (with an
//! edge) or as an unresolved record; nothing is dropped.
//!
//! Resolution only looks at the set of indexed paths, never at `Path::exists`, so results
//! do not depend on the file system's case sensitivity. A consequence: an import of a file
//! that was gitignored, excluded or skipped (binary, too large) is `not_found`, with a
//! detail saying why when that is known.
//!
//! Each language family has its own resolver module: [`ts`] (TS/JS, with [`tsconfig`] and
//! [`npm`]), [`python`], [`rust`], [`cfamily`] (C/C++), [`go`], [`java`], [`csharp`]
//! and [`godot`] (GDScript and Godot scenes, resources and projects).
//!
//! An import that resolves to the importing file itself (Rust `use super::*` inside an
//! inline `mod tests`) keeps its import site but adds no self-loop edge.

pub mod builtins;
pub mod cfamily;
pub mod csharp;
pub mod go;
pub mod godot;
pub mod java;
pub mod jsonc;
pub mod npm;
pub mod python;
pub mod rust;
pub mod ts;
pub mod tsconfig;

use crate::imports::rust::RustSpec;
use crate::imports::{Family, RawImport, Spec};
pub use crate::paths::is_ancestor;
pub use crate::symbols::{RustCallPath, RustCallPaths, SAME_PACKAGE_META};
use cartograph_core::{
    Confidence, Diagnostic, Edge, EdgeKind, ImportSite, Node, NodeKind, SkipReason,
    UnresolvedImport, UnresolvedReason, node_id,
};
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

/// Lookups shared by every resolver.
pub struct Index<'a> {
    /// The mounts of the indexed tree: `(prefix, root on disk)`, one with prefix `""` for a
    /// single repo. A mount root plays the part of the repo root for the files below it.
    /// The roots are only used to read config files that are not indexed (a tsconfig
    /// `extends` into `node_modules`) and to explain `not_found` details.
    pub mounts: Vec<(&'a str, &'a Path)>,
    pub files: HashSet<&'a str>,
    pub dirs: HashSet<&'a str>,
    pub skipped: HashMap<&'a str, SkipReason>,
    /// Retained text of resolver config files, by path (see `walk::is_resolver_config`).
    pub configs: HashMap<&'a str, &'a str>,
    /// Every path looked at on disk (see [`Index::on_disk`]).
    pub disk_reads: std::sync::Mutex<BTreeSet<String>>,
}

impl<'a> Index<'a> {
    /// The index of a single repo at `root` (one mount, no prefix), with nothing in it yet.
    pub fn single(root: &'a Path) -> Index<'a> {
        Index {
            mounts: vec![("", root)],
            files: HashSet::new(),
            dirs: HashSet::new(),
            skipped: HashMap::new(),
            configs: HashMap::new(),
            disk_reads: Default::default(),
        }
    }

    /// The mount `path` lies in (the one with the longest prefix that is an ancestor of
    /// it): `(prefix, root on disk)`. `None` only for a path outside every mount.
    pub fn mount_of(&self, path: &str) -> Option<(&'a str, &'a Path)> {
        self.mounts
            .iter()
            .filter(|(p, _)| is_ancestor(p, path))
            .max_by_key(|(p, _)| p.len())
            .copied()
    }

    /// The folder of the graph that plays the repo root for `path`: the prefix of its mount
    /// (`""` for a single repo).
    pub fn mount_root(&self, path: &str) -> &'a str {
        self.mount_of(path).map_or("", |(p, _)| p)
    }

    /// Whether `dir` is the root of a mount (`""` for a single repo).
    pub fn is_mount_root(&self, dir: &str) -> bool {
        self.mounts.iter().any(|(p, _)| *p == dir)
    }

    /// `dir/rel` for a relative reference written in a file of `dir` (see [`join`]). A
    /// reference that climbs out of its mount goes through the disk: it lands in the mount
    /// whose folder holds that place (a member may sit anywhere, under any name), or
    /// nowhere (`None`, as for one that leaves a single repo).
    pub fn join(&self, dir: &str, rel: &str) -> Option<String> {
        let (prefix, root) = self.mount_of(dir)?;
        if prefix.is_empty() {
            return join(dir, rel);
        }
        let inner = crate::paths::strip_mount(prefix, dir)?;
        if let Some(p) = join(inner, rel) {
            return Some(crate::paths::under(prefix, &p));
        }
        // Out of the mount: from its folder on disk to another mount's.
        let mut abs = root.join(inner);
        for seg in rel.split('/') {
            match seg {
                "" | "." => {}
                ".." => {
                    if !abs.pop() {
                        return None;
                    }
                }
                s => abs.push(s),
            }
        }
        self.mounts
            .iter()
            .filter(|(p, _)| *p != prefix)
            .find_map(|(p, r)| {
                let rest = crate::walk::relative(r, &abs)?;
                Some(crate::paths::under(p, &rest))
            })
    }

    /// The file at `path` (a path of the graph) on disk, recorded as read: whatever the
    /// resolvers learn from the disk rather than from the indexed files goes through here,
    /// so a cached result (a git base snapshot) knows what it depends on.
    pub fn on_disk(&self, path: &str) -> std::path::PathBuf {
        if let Ok(mut reads) = self.disk_reads.lock() {
            reads.insert(path.to_string());
        }
        match self.mount_of(path) {
            Some((prefix, root)) => {
                root.join(crate::paths::strip_mount(prefix, path).unwrap_or(path))
            }
            None => std::path::PathBuf::from(path),
        }
    }

    /// The paths read through [`Index::on_disk`], sorted.
    pub fn take_disk_reads(&self) -> Vec<String> {
        self.disk_reads
            .lock()
            .map(|mut r| std::mem::take(&mut *r).into_iter().collect())
            .unwrap_or_default()
    }

    pub fn is_file(&self, path: &str) -> bool {
        self.files.contains(path)
    }

    pub fn is_dir(&self, path: &str) -> bool {
        path.is_empty() || self.dirs.contains(path)
    }

    /// Text of a config file: the retained copy, else read from disk under the root (for
    /// files that are not indexed, such as `node_modules/@tsconfig/base/tsconfig.json`).
    pub fn read_config(&self, path: &str) -> Option<String> {
        if let Some(t) = self.configs.get(path) {
            return Some((*t).to_string());
        }
        let abs = self.on_disk(path);
        let meta = std::fs::metadata(&abs).ok()?;
        if !meta.is_file() || meta.len() > crate::walk::DEFAULT_MAX_FILE_BYTES {
            return None;
        }
        std::fs::read(&abs)
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    /// Why a path that is not indexed is missing, if the walker recorded it.
    pub fn skip_reason(&self, path: &str) -> Option<SkipReason> {
        self.skipped.get(path).copied()
    }
}

/// What an import points at.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Target {
    File(String),
    /// A Python namespace package (a directory without `__init__.py`).
    Dir(String),
    /// `npm:react`, `node:fs`, `pypi:numpy`, `python:os`.
    External(String),
}

impl Target {
    pub fn node_id(&self) -> String {
        match self {
            Target::File(p) => node_id(NodeKind::File, p),
            Target::Dir(p) => node_id(NodeKind::Dir, p),
            Target::External(e) => node_id(NodeKind::External, e),
        }
    }
}

/// Result of resolving one import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Resolved {
        target: Target,
        confidence: Confidence,
    },
    Unresolved {
        reason: UnresolvedReason,
        detail: Option<String>,
    },
}

impl Outcome {
    pub fn file(path: String, confidence: Confidence) -> Outcome {
        Outcome::Resolved {
            target: Target::File(path),
            confidence,
        }
    }

    pub fn external(id: String, confidence: Confidence) -> Outcome {
        Outcome::Resolved {
            target: Target::External(id),
            confidence,
        }
    }

    pub fn not_found(detail: impl Into<String>) -> Outcome {
        Outcome::Unresolved {
            reason: UnresolvedReason::NotFound,
            detail: Some(detail.into()),
        }
    }

    pub fn ambiguous(detail: impl Into<String>) -> Outcome {
        Outcome::Unresolved {
            reason: UnresolvedReason::Ambiguous,
            detail: Some(detail.into()),
        }
    }
}

/// Imports of one file, as extracted.
pub struct FileImports<'a> {
    pub path: &'a str,
    pub lang: &'a str,
    pub imports: &'a [RawImport],
}

/// Everything resolution adds to the graph.
#[derive(Debug, Default)]
pub struct Resolution {
    pub edges: Vec<Edge>,
    pub externals: Vec<Node>,
    pub sites: Vec<ImportSite>,
    pub unresolved: Vec<UnresolvedImport>,
    /// Problems with resolver configs (unreadable tsconfig, missing `extends`…). The
    /// files are indexed; these are not skipped files.
    pub diagnostics: Vec<Diagnostic>,
    /// `(file path, meta)` to merge into File nodes' meta (Godot scene connections, entry
    /// points; [`TYPE_ONLY_META`]). A path may appear more than once; objects merge.
    pub file_meta: Vec<(String, serde_json::Value)>,
    /// The Rust call paths asked for, resolved (not import sites: no edge comes of them).
    pub rust_call_paths: RustCallPaths,
}

/// Resolve every import of every file (in parallel; the resolvers are immutable), and the
/// Rust call paths `rust_calls` with the same Rust resolver.
pub fn resolve_all(
    ix: &Index<'_>,
    files: &[FileImports<'_>],
    rust_calls: &[RustCallPath],
) -> Resolution {
    let (ts, mut diagnostics) = ts::Resolver::new(ix);
    let py = python::Resolver::new(ix);
    let (rs, mut rs_diagnostics) = rust::Resolver::new(ix, files);
    let (cf, mut cf_diagnostics) = cfamily::Resolver::new(ix);
    diagnostics.append(&mut rs_diagnostics);
    diagnostics.append(&mut cf_diagnostics);
    let (go, mut go_diags) = go::Resolver::new(ix);
    let java = java::Resolver::new(ix, files);
    let csharp = csharp::Resolver::new(ix, files);
    diagnostics.append(&mut go_diags);
    let gd = godot::Resolver::new(ix, files);

    // Per file: (line, specifier, outcome), the indexes of the type-only ones, and the
    // files of the same Java package it uses without an import.
    type Outcomes = (
        Vec<(u32, String, Outcome)>,
        HashSet<usize>,
        BTreeSet<String>,
    );
    let per_file: Vec<Outcomes> = files
        .par_iter()
        .map(|f| {
            let family = Family::of_lang(f.lang);
            let mut out = Vec::new();
            let mut types = HashSet::new();
            let mut uses = BTreeSet::new();
            // Rust paths in code, kept below for the targets no import reaches.
            let mut code_paths: Vec<(u32, String, Outcome)> = Vec::new();
            for raw in f.imports {
                match (&raw.spec, family) {
                    (Spec::Rust(spec @ RustSpec::Path { .. }), _) => {
                        for (s, outcome) in rs.resolve(f.path, spec) {
                            code_paths.push((raw.line, s, outcome));
                        }
                    }
                    (Spec::Path(s), _) => {
                        out.push((raw.line, s.clone(), ts.resolve(f.path, f.lang, s)));
                    }
                    (Spec::TypePath(s), _) => {
                        types.insert(out.len());
                        out.push((raw.line, s.clone(), ts.resolve(f.path, f.lang, s)));
                    }
                    (Spec::Python { module, names }, _) => {
                        for (spec, outcome) in py.resolve(f.path, module, names) {
                            out.push((raw.line, spec, outcome));
                        }
                    }
                    (Spec::Rust(spec), _) => {
                        for (s, outcome) in rs.resolve(f.path, spec) {
                            out.push((raw.line, s, outcome));
                        }
                    }
                    (Spec::Include { path, system }, _) => {
                        let shown = if *system {
                            format!("<{path}>")
                        } else {
                            path.clone()
                        };
                        out.push((raw.line, shown, cf.resolve(f.path, path, *system)));
                    }
                    (Spec::Godot(r), _) => {
                        for (spec, outcome) in gd.resolve(f.path, r) {
                            out.push((raw.line, spec, outcome));
                        }
                    }
                    (Spec::Dynamic(text), _) => out.push((
                        raw.line,
                        text.clone(),
                        Outcome::Unresolved {
                            reason: UnresolvedReason::Dynamic,
                            detail: None,
                        },
                    )),
                    (Spec::Go(path), _) => {
                        for (spec, outcome) in go.resolve(f.path, path) {
                            out.push((raw.line, spec, outcome));
                        }
                    }
                    (
                        Spec::Java {
                            name,
                            is_static,
                            wildcard,
                        },
                        _,
                    ) => {
                        for (spec, outcome) in java.resolve(f.path, name, *is_static, *wildcard) {
                            out.push((raw.line, spec, outcome));
                        }
                    }
                    (
                        Spec::CSharp {
                            name,
                            is_static,
                            alias,
                            scope,
                        },
                        _,
                    ) => {
                        for (spec, outcome) in
                            csharp.resolve(f.path, name, *is_static, *alias, scope)
                        {
                            out.push((raw.line, spec, outcome));
                        }
                    }
                    // Declarations feed the resolvers' indexes; they are not imports.
                    (Spec::Declares(_) | Spec::DeclaresType(_), _) => {}
                    (Spec::TypeRef(name), Some(Family::Java)) => {
                        uses.extend(java.same_package(f.path, name));
                    }
                    (Spec::TypeRef(_), _) => {}
                    (Spec::ParseError(text), _) => out.push((
                        raw.line,
                        text.clone(),
                        Outcome::Unresolved {
                            reason: UnresolvedReason::ParseError,
                            detail: None,
                        },
                    )),
                }
            }
            if !code_paths.is_empty() {
                let mut reached: HashSet<Target> = out
                    .iter()
                    .filter_map(|(_, _, o)| match o {
                        Outcome::Resolved { target, .. } => Some(target.clone()),
                        Outcome::Unresolved { .. } => None,
                    })
                    .collect();
                for (line, s, outcome) in code_paths {
                    if let Outcome::Resolved { target, .. } = &outcome
                        && reached.insert(target.clone())
                    {
                        out.push((line, s, outcome));
                    }
                }
            }
            (out, types, uses)
        })
        .collect();

    let mut res = Resolution {
        rust_call_paths: rust_calls
            .par_iter()
            .filter_map(|k| rs.path_file(&k.0, &k.1, &k.2).map(|f| (k.clone(), f)))
            .collect(),
        ..Resolution::default()
    };
    let mut externals: BTreeMap<String, Node> = BTreeMap::new();
    for (f, (outcomes, types, uses)) in files.iter().zip(per_file) {
        let src = node_id(NodeKind::File, f.path);
        // A type of the file's own package: no import, still a dependency. Marked on the
        // file ([`SAME_PACKAGE_META`]) with its package.
        if !uses.is_empty() {
            let dsts: Vec<String> = uses.iter().map(|u| node_id(NodeKind::File, u)).collect();
            for d in &dsts {
                res.edges.push(Edge::new(
                    src.clone(),
                    d.clone(),
                    EdgeKind::Uses,
                    Confidence::Resolved,
                ));
            }
            res.file_meta.push((
                f.path.to_string(),
                serde_json::json!({ SAME_PACKAGE_META: {
                    "package": java.package(f.path).unwrap_or_default(),
                    "files": dsts,
                }}),
            ));
        }
        // Repo targets of this file → whether every import of it so far is type-only.
        let mut only_types: BTreeMap<String, bool> = BTreeMap::new();
        for (i, (line, specifier, outcome)) in outcomes.into_iter().enumerate() {
            // Defensive: the repo root is never a module (it has no `dir:` node), nor is a
            // mount's root (its `dir:` node holds the whole mount).
            let outcome = match outcome {
                Outcome::Resolved {
                    target: Target::Dir(d),
                    ..
                } if ix.is_mount_root(&d) => Outcome::not_found("the repo root is not a module"),
                other => other,
            };
            match outcome {
                Outcome::Resolved { target, confidence } => {
                    let dst = target.node_id();
                    let kind = match &target {
                        Target::External(ext) => {
                            externals
                                .entry(dst.clone())
                                .or_insert_with(|| external_node(ext));
                            EdgeKind::ImportExternal
                        }
                        Target::File(_) | Target::Dir(_) => EdgeKind::Import,
                    };
                    if kind == EdgeKind::Import && dst != src {
                        *only_types.entry(dst.clone()).or_insert(true) &= types.contains(&i);
                    }
                    if dst != src {
                        res.edges
                            .push(Edge::new(src.clone(), dst.clone(), kind, confidence));
                    }
                    res.sites.push(ImportSite {
                        file: f.path.to_string(),
                        line,
                        specifier,
                        target: dst,
                    });
                }
                Outcome::Unresolved { reason, detail } => {
                    res.unresolved.push(UnresolvedImport {
                        file: f.path.to_string(),
                        line,
                        specifier,
                        reason,
                        detail,
                    });
                }
            }
        }
        let type_only: Vec<String> = only_types
            .into_iter()
            .filter_map(|(dst, all)| all.then_some(dst))
            .collect();
        if !type_only.is_empty() {
            res.file_meta.push((
                f.path.to_string(),
                serde_json::json!({ TYPE_ONLY_META: type_only }),
            ));
        }
    }
    res.externals = externals.into_values().collect();
    res.diagnostics.append(&mut diagnostics);
    let (mut gd_edges, mut gd_meta) = gd.extras(files);
    res.edges.append(&mut gd_edges);
    res.file_meta.append(&mut gd_meta);
    res.edges.extend(go_test_edges(ix));
    res
}

/// Go: a `_test.go` file sees the other files of its package without an import, and
/// `go test` runs a package's tests together. Each test file gets a `uses` edge to its
/// package as an import of it would: to its Dir (non-test files), or, for a package at the
/// repo root (no Dir node) or a mount's root, to each non-test `.go` file there. An external test package
/// (`package x_test`) imports the package too; the edge adds nothing then.
fn go_test_edges(ix: &Index<'_>) -> Vec<Edge> {
    let is_go = |f: &&&str| f.ends_with(".go");
    let mut code_dirs: HashMap<&str, Vec<&str>> = HashMap::new();
    for f in ix.files.iter().filter(is_go) {
        if !f.ends_with("_test.go") {
            code_dirs.entry(parent(f)).or_default().push(f);
        }
    }
    let mut out = Vec::new();
    for t in ix
        .files
        .iter()
        .filter(is_go)
        .filter(|f| f.ends_with("_test.go"))
    {
        let dir = parent(t);
        let Some(code) = code_dirs.get(dir) else {
            continue;
        };
        let src = node_id(NodeKind::File, t);
        let dsts: Vec<String> = if ix.is_mount_root(dir) {
            code.iter().map(|f| node_id(NodeKind::File, f)).collect()
        } else {
            vec![node_id(NodeKind::Dir, dir)]
        };
        out.extend(
            dsts.into_iter()
                .map(|d| Edge::new(src.clone(), d, EdgeKind::Uses, Confidence::Resolved)),
        );
    }
    out
}

/// Key of a File node's `meta` listing the repo targets (`file:` / `dir:` ids) that file
/// imports only for types (TypeScript `import type`, `import("…")` in a type…). The `Import`
/// edge is there as for any import; this marks it type-only without a schema change.
pub const TYPE_ONLY_META: &str = "type_only_imports";

/// External node for `ecosystem:name` (`npm:react`, `node:fs`, `pypi:numpy`, `python:os`).
fn external_node(ext: &str) -> Node {
    let (ecosystem, name) = ext.split_once(':').unwrap_or(("", ext));
    Node {
        id: node_id(NodeKind::External, ext),
        kind: NodeKind::External,
        path: ext.to_string(),
        name: name.to_string(),
        lang: None,
        loc: 0,
        bytes: 0,
        hash: None,
        meta: serde_json::json!({ "ecosystem": ecosystem }),
    }
}

/// Parent directory of a repo-relative path (`""` for top-level entries).
pub fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// `dir/rel`, normalising `.` and `..` segments and empty segments. `None` if the result
/// would leave the repo root.
pub fn join(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = if dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// Number of segments in a repo-relative dir (`""` → 0).
pub fn depth(dir: &str) -> usize {
    if dir.is_empty() {
        0
    } else {
        dir.split('/').count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_normalises_and_refuses_to_escape() {
        assert_eq!(join("a/b", "./c").as_deref(), Some("a/b/c"));
        assert_eq!(join("a/b", "../c/./d").as_deref(), Some("a/c/d"));
        assert_eq!(join("", "x").as_deref(), Some("x"));
        assert_eq!(join("a", "..").as_deref(), Some(""));
        assert_eq!(join("a", "../.."), None);
        assert_eq!(join("a", "b//c/").as_deref(), Some("a/b/c"));
    }

    #[test]
    fn a_relative_path_out_of_a_mount_lands_in_the_mount_whose_folder_holds_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (api, lib) = (
            tmp.path().join("backend-api"),
            tmp.path().join("shared-lib"),
        );
        let ix = Index {
            mounts: vec![("api", api.as_path()), ("lib", lib.as_path())],
            ..Index::single(tmp.path())
        };
        assert_eq!(ix.join("api/src", "./a").as_deref(), Some("api/src/a"));
        assert_eq!(ix.join("api/src", "..").as_deref(), Some("api"));
        assert_eq!(
            ix.join("api/src", "../../shared-lib/x/y").as_deref(),
            Some("lib/x/y")
        );
        assert_eq!(
            ix.join("api/src", "../../shared-lib").as_deref(),
            Some("lib")
        );
        assert_eq!(ix.join("api/src", "../../elsewhere/x"), None);
        assert_eq!(ix.join("api", "../../../x"), None);
        assert_eq!(ix.mount_root("lib/x/y"), "lib");
        assert!(ix.is_mount_root("api") && !ix.is_mount_root(""));
        let single = Index::single(tmp.path());
        assert_eq!(single.join("a/b", "../../c").as_deref(), Some("c"));
        assert_eq!(single.join("a", "../../c"), None);
    }

    #[test]
    fn ancestors() {
        assert!(is_ancestor("", "a/b"));
        assert!(is_ancestor("a", "a/b"));
        assert!(is_ancestor("a/b", "a/b"));
        assert!(!is_ancestor("a/b", "a/bc"));
        assert!(!is_ancestor("a/b", "a"));
        assert_eq!(depth(""), 0);
        assert_eq!(depth("a/b"), 2);
    }
}
