//! Call resolution: every definition becomes a [`Symbol`]; every call site resolves to one
//! symbol with a confidence, or is counted as ambiguous or unresolved. Precision comes
//! first: a wrong caller is worse than a missing one.
//!
//! Rules, in order; the first that names a definition decides:
//! 1. **exact**: a definition in the same file (`f()`, `this.f()`, `self.f()`, `Type::f()`,
//!    a method of the enclosing class for languages with an implicit `this`, `obj.f()` of a
//!    JS object literal `obj` with that member).
//! 2. **resolved**: through an import that resolved to a file defining the name (named,
//!    default and namespace imports, re-exports followed up to three hops, glob imports,
//!    `obj.f()` of an imported JS object literal (`export const obj = {…}`,
//!    `export default {…}`),
//!    a Go package, Java's package, C/C++ includes and the header's source file, Godot
//!    global class names and autoloads), or, for a Rust call through a path that no rule
//!    above resolves (`krate::m::f()`, `crate::m::f()`, `super::f()`, `krate::Type::new()`),
//!    through that path resolved like a `use` of it (workspace crates and re-exports
//!    included; see [`super::RustCallPaths`]). `self.field.m()` (`this.field.m()`)
//!    in a method of a type whose definition in the same file declares `field` with one
//!    named type ([`crate::symbols::FieldType`]): `m` of that type, when the type resolves
//!    through the file itself or its imports (`heuristic` when only its unique name in
//!    the repo finds it). When that type is a class, struct or enum of the repo that
//!    neither defines nor inherits such a method (see below), the call is unresolved: the
//!    method may come from a base outside the repo, and a same-named method elsewhere would
//!    be a guess. When it is an interface, a trait, an alias, a TS abstract or ambient
//!    (`declare`) class, or not a type of the repo, the other rules decide.
//!    A Rust type's method may be in an `impl` block of another file: one in the type's
//!    folder or below, else anywhere in its crate (`heuristic`); with none, a default method
//!    of a trait the type implements; else the other rules decide (a trait outside the
//!    repo, `Deref`).
//!
//! **Inheritance** ([`hier`]): a method a type does not define is looked up in its
//! supertypes (base classes, interfaces, implemented traits, supertraits, Go embedded
//! types, the script a GDScript file extends), in the language's lookup order, at most five
//! hops: for `this.m()` / `self.m()` (and `m()` where `this` is implicit), `super.m()` /
//! `super().m()` / `base.M()` (the supertypes only), `self.field.m()`, `Type.m()` /
//! `Type::m()` and a Go receiver's `r.m()`. The first supertype that defines it decides
//! (an interface or trait method that is a symbol is that declaration); a supertype outside
//! the repo ends the walk unless it only declares methods (an interface, a trait). An
//! inherited method is `resolved` when every hop resolved through the file or its imports,
//! else `heuristic`; never `exact`.
//! 3. **heuristic**: a name unique in the repo, only where a name can be in scope without a
//!    visible import (C/C++ functions, Java/C#/C++ type names, JS files that are not ES
//!    modules), and `obj.m()` when `m` is the only method of that name in the language and
//!    its file is the caller's file or one it imports (members of JS object literals are
//!    left out of this count: [`Def::object`]).
//!
//! A name bound by an import that did not resolve to a definition (a library, a missing
//! file) stops there: it is never matched by name elsewhere. Several candidates with
//! different ids are *ambiguous*; nothing is guessed.

mod hier;

use super::RustCallPaths;
use super::{BindKind, Call, Code, Def, Form};
use cartograph_core::{
    CallStats, Confidence, EdgeKind, EntryKind, EntryPoint, Graph, NodeKind, Symbol, SymbolCall,
    SymbolKind, Symbols, symbol_id,
};
use hier::{Hier, TKey, caller_key, inherited, inherited_of_one, rust_impls};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

/// One file's extraction, as input to [`resolve`].
pub struct FileCode<'a> {
    pub path: &'a str,
    pub lang: &'a str,
    pub code: &'a Code,
}

/// Languages that share one namespace of definitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Fam {
    Js,
    Py,
    Rust,
    Go,
    Java,
    Cs,
    C,
    Gd,
}

fn family(lang: &str) -> Option<Fam> {
    Some(match lang {
        "typescript" | "tsx" | "javascript" | "jsx" => Fam::Js,
        "python" => Fam::Py,
        "rust" => Fam::Rust,
        "go" => Fam::Go,
        "java" => Fam::Java,
        "csharp" => Fam::Cs,
        "c" | "cpp" => Fam::C,
        "gdscript" => Fam::Gd,
        _ => return None,
    })
}

/// Method names too common to match by name alone (`obj.get()` is a Map as often as not).
const STOP: &[&str] = &[
    "add",
    "all",
    "any",
    "append",
    "apply",
    "assert",
    "bind",
    "call",
    "catch",
    "clear",
    "clone",
    "close",
    "concat",
    "connect",
    "contains",
    "copy",
    "count",
    "create",
    "debug",
    "decode",
    "default",
    "delete",
    "destroy",
    "disconnect",
    "dispatch",
    "dispose",
    "done",
    "emit",
    "encode",
    "end",
    "entries",
    "equals",
    "error",
    "every",
    "exec",
    "execute",
    "exists",
    "fetch",
    "filter",
    "finally",
    "find",
    "first",
    "flush",
    "for_each",
    "forEach",
    "format",
    "free",
    "from",
    "get",
    "handle",
    "has",
    "hashCode",
    "includes",
    "index",
    "indexOf",
    "info",
    "init",
    "insert",
    "into",
    "invoke",
    "is_empty",
    "isEmpty",
    "iter",
    "join",
    "json",
    "keys",
    "last",
    "len",
    "length",
    "listen",
    "load",
    "lock",
    "log",
    "map",
    "match",
    "max",
    "min",
    "new",
    "next",
    "notify",
    "off",
    "on",
    "once",
    "open",
    "parse",
    "pop",
    "print",
    "process",
    "push",
    "put",
    "query",
    "read",
    "reduce",
    "reject",
    "remove",
    "render",
    "replace",
    "request",
    "reset",
    "resolve",
    "reverse",
    "run",
    "save",
    "send",
    "set",
    "shift",
    "size",
    "slice",
    "some",
    "sort",
    "splice",
    "split",
    "start",
    "stop",
    "subscribe",
    "test",
    "text",
    "then",
    "toJSON",
    "toString",
    "to_string",
    "trace",
    "trim",
    "unlock",
    "unshift",
    "unsubscribe",
    "unwrap",
    "update",
    "value",
    "valueOf",
    "values",
    "wait",
    "warn",
    "write",
];

type Sid = u32;

/// One file with code, and its definitions by name.
struct FileIx<'a> {
    path: &'a str,
    fam: Fam,
    code: &'a Code,
    /// Sid of `code.defs[0]`; defs map to consecutive sids.
    first: Sid,
    top: HashMap<&'a str, Vec<Sid>>,
    members: HashMap<&'a str, HashMap<&'a str, Vec<Sid>>>,
    /// Type definitions (class, struct…) by name, at any nesting.
    types: HashMap<&'a str, Vec<Sid>>,
    dir: &'a str,
    /// Rust: the folder of the file's crate (its nearest `Cargo.toml`), "" with none.
    krate: &'a str,
}

/// What a local name is bound to.
struct Bound<'a> {
    kind: BindKind,
    imported: Option<&'a str>,
    container: Option<&'a str>,
    files: Vec<usize>,
    /// Bound to a library, a file without code or an import that did not resolve.
    external: bool,
}

#[derive(Default)]
struct Bindings<'a> {
    by_name: HashMap<&'a str, Bound<'a>>,
    /// Files whose names are all in scope (glob imports, includes).
    globs: Vec<usize>,
    /// Files of the same package (Go) or folder (Java, C#).
    scope: Vec<usize>,
    /// Files this file depends on (imports, dir imports expanded).
    deps: HashSet<usize>,
}

struct Ctx<'a> {
    files: Vec<FileIx<'a>>,
    by_path: HashMap<&'a str, usize>,
    symbols: Vec<Symbol>,
    ids: Vec<String>,
    /// Sid → file index.
    owner: Vec<usize>,
    exported_top: HashMap<(Fam, &'a str), Vec<Sid>>,
    /// JS files that are not ES modules: their top-level definitions are globals.
    script_top: HashMap<&'a str, Vec<Sid>>,
    exported_types: HashMap<(Fam, &'a str), Vec<Sid>>,
    methods: HashMap<(Fam, &'a str), Vec<Sid>>,
    members_global: HashMap<(Fam, &'a str, &'a str), Vec<Sid>>,
    /// Methods that implement a trait or interface method (see [`Def::overrides`]).
    overrides: HashSet<Sid>,
    /// Sid → the first sid with the same id (one symbol, see [`Symbol::id`]).
    canon: Vec<Sid>,
}

/// How a name is reached in the file that defines it (see [`Ctx::visible`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    /// The file's own name (its default export).
    Local,
    /// A named or default import, a namespace or module qualifier (`ns.f`, `mod.f`).
    Named,
    /// A glob import or re-export (`export *`, `from m import *`, `use m::*`).
    Glob,
}

enum Out {
    Hit(Sid, Confidence),
    Ambiguous,
    Unresolved,
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// The folder of the nearest manifest (`manifests`: folders holding a `Cargo.toml`) at or
/// above `path`'s folder, or "" when there is none.
fn crate_dir<'p>(path: &'p str, manifests: &HashSet<&str>) -> &'p str {
    let mut d = dir_of(path);
    while !d.is_empty() && !manifests.contains(d) {
        d = dir_of(d);
    }
    d
}

fn stem_of(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.split('.').next().unwrap_or(name)
}

impl<'a> Ctx<'a> {
    fn build(files: &[FileCode<'a>], manifests: &HashSet<&str>) -> Ctx<'a> {
        let mut ctx = Ctx {
            files: Vec::new(),
            by_path: HashMap::new(),
            symbols: Vec::new(),
            ids: Vec::new(),
            owner: Vec::new(),
            exported_top: HashMap::new(),
            script_top: HashMap::new(),
            exported_types: HashMap::new(),
            methods: HashMap::new(),
            members_global: HashMap::new(),
            overrides: HashSet::new(),
            canon: Vec::new(),
        };
        let mut sorted: Vec<&FileCode<'a>> = files.iter().collect();
        sorted.sort_by(|a, b| a.path.cmp(b.path));
        for fc in sorted {
            let Some(fam) = family(fc.lang) else { continue };
            let fi = ctx.files.len();
            let first = ctx.symbols.len() as Sid;
            let mut ix = FileIx {
                path: fc.path,
                fam,
                code: fc.code,
                first,
                top: HashMap::new(),
                members: HashMap::new(),
                types: HashMap::new(),
                dir: dir_of(fc.path),
                krate: if fam == Fam::Rust {
                    crate_dir(fc.path, manifests)
                } else {
                    ""
                },
            };
            let mut first_of: HashMap<String, Sid> = HashMap::new();
            for (i, d) in fc.code.defs.iter().enumerate() {
                let sid = first + i as Sid;
                let sym = Symbol {
                    file: fc.path.to_string(),
                    name: d.name.clone(),
                    container: d.container.clone(),
                    kind: d.kind,
                    line: d.line,
                    end_line: d.end,
                    exported: d.exported,
                };
                let id = sym.id();
                ctx.canon.push(*first_of.entry(id.clone()).or_insert(sid));
                ctx.ids.push(id);
                ctx.symbols.push(sym);
                ctx.owner.push(fi);
                let name = d.name.as_str();
                match d.container.as_deref() {
                    None => {
                        ix.top.entry(name).or_default().push(sid);
                        // A GDScript file is a class: its functions are called as methods.
                        if fam == Fam::Gd && d.kind == SymbolKind::Function {
                            ctx.methods.entry((fam, name)).or_default().push(sid);
                        }
                        if d.exported {
                            ctx.exported_top.entry((fam, name)).or_default().push(sid);
                            if fam == Fam::Js && !fc.code.module {
                                ctx.script_top.entry(name).or_default().push(sid);
                            }
                            if d.kind.is_type() {
                                ctx.exported_types.entry((fam, name)).or_default().push(sid);
                            }
                        }
                    }
                    Some(c) => {
                        ix.members
                            .entry(c)
                            .or_default()
                            .entry(name)
                            .or_default()
                            .push(sid);
                        ctx.members_global
                            .entry((fam, c, name))
                            .or_default()
                            .push(sid);
                        // Every method counts for uniqueness; implementations of a trait or
                        // an interface are never the unique match themselves (see below).
                        if d.kind == SymbolKind::Method && !d.object {
                            ctx.methods.entry((fam, name)).or_default().push(sid);
                            if d.overrides {
                                ctx.overrides.insert(sid);
                            }
                        }
                    }
                }
                if d.kind.is_type() {
                    ix.types.entry(name).or_default().push(sid);
                }
            }
            ctx.by_path.insert(fc.path, fi);
            ctx.files.push(ix);
        }
        ctx
    }

    /// One outcome for a set of candidates: a hit when they all share one id.
    fn decide(&self, v: &[Sid], conf: Confidence) -> Option<Out> {
        let first = *v.first()?;
        if v.iter()
            .all(|&s| self.ids[s as usize] == self.ids[first as usize])
        {
            Some(Out::Hit(first, conf))
        } else {
            Some(Out::Ambiguous)
        }
    }

    fn top(&self, fi: usize, name: &str) -> &[Sid] {
        self.files[fi].top.get(name).map_or(&[], Vec::as_slice)
    }

    fn members(&self, fi: usize, container: &str, name: &str) -> &[Sid] {
        self.files[fi]
            .members
            .get(container)
            .and_then(|m| m.get(name))
            .map_or(&[], Vec::as_slice)
    }

    fn top_types(&self, fi: usize, name: &str) -> Vec<Sid> {
        self.files[fi]
            .types
            .get(name)
            .map_or(Vec::new(), |v| v.clone())
    }

    /// Whether `file` is the module an import names (`from pkg import mod`, `use a::b`).
    fn is_module_named(&self, fi: usize, name: &str) -> bool {
        let p = self.files[fi].path;
        let stem = stem_of(p);
        stem == name
            || (matches!(stem, "mod" | "__init__" | "index" | "lib")
                && dir_of(p).rsplit('/').next() == Some(name))
    }

    /// Whether top-level definition `s` of file `fi` is reached through an import `via`:
    /// a JS/TS module shows only its exports; a Python star import skips what the module
    /// does not export (`_names`, or names left out of `__all__`), while a named import or
    /// `mod.name` may reach any name; Rust (`use super::*` sees private items) and the
    /// other languages are not filtered here. Names a module only imported are filtered in
    /// [`Ctx::export_lookup`] ([`Ctx::star_skips`]).
    fn visible(&self, fi: usize, s: Sid, via: Via) -> bool {
        let exported = self.symbols[s as usize].exported;
        match (self.files[fi].fam, via) {
            (_, Via::Local) => true,
            (Fam::Js, _) => exported,
            (Fam::Py, Via::Glob) => exported,
            _ => true,
        }
    }

    /// Definitions a file exports under `name`, following its re-exports (bindings and
    /// globs) up to `depth` hops. `via` is how the name is reached (see [`Ctx::visible`]).
    fn export_lookup(
        &self,
        binds: &[Bindings<'a>],
        fi: usize,
        name: &str,
        via: Via,
        depth: u8,
        visited: &mut Vec<usize>,
    ) -> Vec<Sid> {
        if visited.contains(&fi) {
            return Vec::new();
        }
        visited.push(fi);
        let own: Vec<Sid> = self
            .top(fi, name)
            .iter()
            .copied()
            .filter(|&s| self.visible(fi, s, via))
            .collect();
        if !own.is_empty() {
            return own;
        }
        if self.star_skips(fi, name, via) {
            return Vec::new();
        }
        if depth == 0 {
            return Vec::new();
        }
        let b = &binds[fi];
        if let Some(bd) = b.by_name.get(name) {
            if bd.external {
                return Vec::new();
            }
            let target = bd.imported.unwrap_or(name);
            let mut out = Vec::new();
            for &f in &bd.files {
                match bd.kind {
                    BindKind::Named => out.extend(self.export_lookup(
                        binds,
                        f,
                        target,
                        Via::Named,
                        depth - 1,
                        visited,
                    )),
                    BindKind::Default => out.extend(self.default_of(binds, f, depth - 1)),
                    _ => {}
                }
            }
            return out;
        }
        if !matches!(self.files[fi].fam, Fam::Js | Fam::Py | Fam::Rust) {
            return Vec::new();
        }
        let mut out = Vec::new();
        for &g in &b.globs {
            out.extend(self.export_lookup(binds, g, name, Via::Glob, depth - 1, visited));
        }
        out
    }

    /// Whether a Python star import (`via` [`Via::Glob`]) of file `fi` leaves out `name`,
    /// which the file does not define: with an explicit `__all__`, every name not listed
    /// (a listed `_name` the module imported comes along); without one, `_names`.
    fn star_skips(&self, fi: usize, name: &str, via: Via) -> bool {
        let f = &self.files[fi];
        if via != Via::Glob || f.fam != Fam::Py {
            return false;
        }
        match &f.code.all {
            Some(all) => all.binary_search_by(|n| n.as_str().cmp(name)).is_err(),
            None => name.starts_with('_'),
        }
    }

    /// A JS/TS file's default export.
    fn default_of(&self, binds: &[Bindings<'a>], fi: usize, depth: u8) -> Vec<Sid> {
        match &self.files[fi].code.default_export {
            Some(n) => self.export_lookup(binds, fi, n, Via::Local, depth, &mut Vec::new()),
            None => Vec::new(),
        }
    }

    fn bind_file(&self, fi: usize, g: &GraphIx<'a>) -> Bindings<'a> {
        let f = &self.files[fi];
        let mut out = Bindings::default();
        // Files a target id stands for (a dir import reaches the dir's files of this family).
        let targets = |t: &str| -> Option<Vec<usize>> {
            if let Some(p) = t.strip_prefix("file:") {
                return Some(self.by_path.get(p).map_or(Vec::new(), |&i| vec![i]));
            }
            if let Some(d) = t.strip_prefix("dir:") {
                return Some(
                    g.dir_files
                        .get(d)
                        .into_iter()
                        .flatten()
                        .filter_map(|p| self.by_path.get(p).copied())
                        .filter(|&i| self.files[i].fam == f.fam)
                        .collect(),
                );
            }
            None
        };
        for s in g.sites_of.get(f.path).into_iter().flatten() {
            if let Some(v) = targets(s.1) {
                out.deps.extend(v.into_iter().filter(|&i| i != fi));
            }
        }
        for b in &f.code.bindings {
            let at: Vec<(&str, &str)> = g
                .sites
                .get(&(f.path, b.line))
                .map(|v| v.to_vec())
                .unwrap_or_default();
            let chosen = choose_sites(f.fam, b, &at);
            let (mut files, mut external) = (Vec::new(), false);
            for (_, t) in &chosen {
                match targets(t) {
                    Some(v) => files.extend(v),
                    None => external = true,
                }
            }
            files.sort_unstable();
            files.dedup();
            if files.is_empty() {
                external = true;
            }
            match b.kind {
                BindKind::Glob => out.globs.extend(files),
                kind => {
                    out.by_name.entry(b.local.as_str()).or_insert(Bound {
                        kind,
                        imported: b.imported.as_deref(),
                        container: b.container.as_deref(),
                        files,
                        external,
                    });
                }
            }
        }
        match f.fam {
            Fam::Go | Fam::Java | Fam::Cs => {
                out.scope = g
                    .dir_files
                    .get(f.dir)
                    .into_iter()
                    .flatten()
                    .filter_map(|p| self.by_path.get(p).copied())
                    .filter(|&i| i != fi && self.files[i].fam == f.fam)
                    .collect();
            }
            Fam::C => {
                // Included files and the source file next to each included header.
                let mut globs = Vec::new();
                for s in g.sites_of.get(f.path).into_iter().flatten() {
                    let Some(p) = s.1.strip_prefix("file:") else {
                        continue;
                    };
                    let Some(&hi) = self.by_path.get(p) else {
                        continue;
                    };
                    globs.push(hi);
                    let (dir, stem) = (dir_of(p), stem_of(p));
                    for sib in g.dir_files.get(dir).into_iter().flatten() {
                        if sib != &p
                            && stem_of(sib) == stem
                            && let Some(&si) = self.by_path.get(sib)
                            && si != fi
                        {
                            globs.push(si);
                        }
                    }
                }
                out.globs.extend(globs);
            }
            Fam::Gd => {
                // Global class names and autoloads: sites whose specifier is a bare name.
                for s in g.sites_of.get(f.path).into_iter().flatten() {
                    if s.0.contains(' ') || s.0.contains('/') {
                        continue;
                    }
                    let files = targets(s.1).unwrap_or_default();
                    out.by_name.entry(s.0).or_insert(Bound {
                        kind: BindKind::Namespace,
                        imported: None,
                        container: None,
                        external: files.is_empty(),
                        files,
                    });
                }
            }
            _ => {}
        }
        out.globs.sort_unstable();
        out.globs.dedup();
        out
    }
}

/// The import sites a binding stands for, among those on its line.
fn choose_sites<'s>(
    fam: Fam,
    b: &super::Binding,
    at: &[(&'s str, &'s str)],
) -> Vec<(&'s str, &'s str)> {
    if at.len() <= 1 {
        return at.to_vec();
    }
    let by_spec: Vec<(&str, &str)> = match &b.spec {
        Some(s) => at.iter().copied().filter(|(sp, _)| sp == s).collect(),
        None => Vec::new(),
    };
    if !by_spec.is_empty() {
        return by_spec;
    }
    if let Some(imp) = &b.imported {
        let tail: Vec<(&str, &str)> = at
            .iter()
            .copied()
            .filter(|(sp, t)| match fam {
                Fam::Rust => sp.ends_with(&format!("::{imp}")) || sp == imp,
                Fam::Py => stem_of(t) == imp.as_str() || sp.ends_with(&format!(".{imp}")),
                Fam::Java => sp.ends_with(&format!(".{imp}")),
                _ => false,
            })
            .collect();
        if !tail.is_empty() {
            return tail;
        }
    }
    at.to_vec()
}

/// Lookups from the stored graph.
struct GraphIx<'a> {
    /// `(file, line)` → `(specifier, target id)`.
    sites: HashMap<(&'a str, u32), Vec<(&'a str, &'a str)>>,
    /// Every import site of a file.
    sites_of: HashMap<&'a str, Vec<(&'a str, &'a str)>>,
    dir_files: HashMap<&'a str, Vec<&'a str>>,
}

impl<'a> GraphIx<'a> {
    fn build(graph: &'a Graph) -> GraphIx<'a> {
        let mut g = GraphIx {
            sites: HashMap::new(),
            sites_of: HashMap::new(),
            dir_files: HashMap::new(),
        };
        for s in &graph.imports {
            g.sites
                .entry((s.file.as_str(), s.line))
                .or_default()
                .push((s.specifier.as_str(), s.target.as_str()));
            g.sites_of
                .entry(s.file.as_str())
                .or_default()
                .push((s.specifier.as_str(), s.target.as_str()));
        }
        for n in graph.nodes_of(NodeKind::File) {
            g.dir_files
                .entry(dir_of(&n.path))
                .or_default()
                .push(n.path.as_str());
        }
        g
    }

    /// Package names (`express`, `net/http`, `axum`) of the libraries a file imports.
    fn libraries(&self, file: &str) -> Vec<&'a str> {
        self.sites_of
            .get(file)
            .into_iter()
            .flatten()
            .filter_map(|(_, t)| t.strip_prefix("external:"))
            .map(|e| e.split_once(':').map_or(e, |(_, name)| name))
            .collect()
    }
}

/// Libraries that make a route or command pattern trustworthy, per gate.
fn gate_libraries(gate: &str) -> &'static [&'static str] {
    match gate {
        "js-http" => &[
            "express",
            "fastify",
            "koa",
            "@koa/router",
            "koa-router",
            "hono",
            "restify",
            "polka",
            "h3",
            "elysia",
        ],
        "js-cli" => &["commander", "yargs", "cac", "sade"],
        "js-ipc" => &["electron"],
        "js-nest" => &["@nestjs/common"],
        "py-http" => &["flask", "fastapi", "starlette", "quart", "sanic", "bottle"],
        "py-cli" => &["click", "typer"],
        "py-argparse" => &["argparse"],
        "rust-http" => &["axum"],
        "go-http" => &[
            "net/http",
            "github.com/gin-gonic/gin",
            "github.com/labstack/echo",
            "github.com/go-chi/chi",
            "github.com/gofiber/fiber",
            "github.com/gorilla/mux",
        ],
        _ => &[],
    }
}

/// Whether a file importing `libs` passes `gate`.
pub fn gate_matches(gate: &str, libs: &[&str]) -> bool {
    let want = gate_libraries(gate);
    libs.iter().any(|l| {
        want.iter()
            .any(|w| l == w || l.starts_with(&format!("{w}/")) || l.starts_with(&format!("{w}@")))
    })
}

struct FileResult {
    calls: Vec<SymbolCall>,
    stats: CallStats,
    entries: Vec<EntryPoint>,
}

/// Resolve every file's calls and entry points. `files` are the extractions of the files
/// of `graph` (import sites and edges must already be resolved).
pub fn resolve(graph: &Graph, files: &[FileCode<'_>], rust_paths: &RustCallPaths) -> Symbols {
    let manifests: HashSet<&str> = graph
        .nodes_of(NodeKind::File)
        .filter(|n| n.path.rsplit('/').next() == Some("Cargo.toml"))
        .map(|n| dir_of(&n.path))
        .collect();
    let ctx = Ctx::build(files, &manifests);
    let g = GraphIx::build(graph);
    let binds: Vec<Bindings<'_>> = (0..ctx.files.len())
        .into_par_iter()
        .map(|fi| ctx.bind_file(fi, &g))
        .collect();
    let hier = Hier::build(&ctx, &binds, &g);
    let results: Vec<FileResult> = (0..ctx.files.len())
        .into_par_iter()
        .map(|fi| resolve_file(&ctx, &binds, &hier, &g, rust_paths, fi))
        .collect();
    let mut out = Symbols {
        symbols: ctx.symbols.clone(),
        supers: hier.rows(&ctx),
        indexed: true,
        ..Symbols::default()
    };
    for r in results {
        out.calls.extend(r.calls);
        if r.stats.total() > 0 {
            out.stats.push(r.stats);
        }
        out.entries.extend(r.entries);
    }
    out.entries.extend(godot_entries(graph));
    out.entries.extend(npm_main_entries(graph));
    out.normalize();
    out
}

/// The innermost function or method of a file whose lines hold `line`.
fn caller_of<'c>(callables: &[(u32, u32, &'c Def, Sid)], line: u32) -> Option<(&'c Def, Sid)> {
    let i = callables.partition_point(|c| c.0 <= line);
    callables[..i]
        .iter()
        .rev()
        .find(|c| c.1 >= line)
        .map(|c| (c.2, c.3))
}

fn resolve_file(
    ctx: &Ctx<'_>,
    binds: &[Bindings<'_>],
    h: &Hier<'_>,
    g: &GraphIx<'_>,
    rust_paths: &RustCallPaths,
    fi: usize,
) -> FileResult {
    let f = &ctx.files[fi];
    let mut callables: Vec<(u32, u32, &Def, Sid)> = f
        .code
        .defs
        .iter()
        .enumerate()
        .filter(|(_, d)| d.kind.is_callable())
        .map(|(i, d)| (d.start, d.end, d, f.first + i as Sid))
        .collect();
    callables.sort_by_key(|c| (c.0, std::cmp::Reverse(c.1)));
    let mut stats = CallStats {
        file: f.path.to_string(),
        ..CallStats::default()
    };
    let mut calls = Vec::new();
    for c in &f.code.calls {
        let caller = caller_of(&callables, c.line);
        match resolve_call(ctx, binds, h, rust_paths, fi, c, caller.map(|x| x.0)) {
            Out::Hit(sid, conf) => {
                match conf {
                    Confidence::Exact => stats.exact += 1,
                    Confidence::Resolved => stats.resolved += 1,
                    Confidence::Heuristic => stats.heuristic += 1,
                }
                calls.push(SymbolCall {
                    file: f.path.to_string(),
                    line: c.line,
                    caller: caller.map(|(_, s)| ctx.ids[s as usize].clone()),
                    target: ctx.ids[sid as usize].clone(),
                    confidence: conf,
                });
            }
            Out::Ambiguous => stats.ambiguous += 1,
            Out::Unresolved => stats.unresolved += 1,
        }
    }
    // Entry points.
    let libs = g.libraries(f.path);
    let mut entries = Vec::new();
    for e in &f.code.entries {
        if let Some(gate) = &e.gate
            && !gate_matches(gate, &libs)
        {
            continue;
        }
        let target = if let Some(d) = &e.def {
            // A definition of this file (a scan of the file's own definitions only).
            f.code
                .defs
                .iter()
                .any(|x| x.qualified() == *d)
                .then(|| symbol_id(f.path, d))
        } else if let Some(handler) = &e.handler {
            match resolve_call(ctx, binds, h, rust_paths, fi, handler, None) {
                Out::Hit(sid, _) => Some(ctx.ids[sid as usize].clone()),
                _ => None,
            }
        } else if e.kind == EntryKind::Main || e.kind == EntryKind::Ipc {
            // A `main` of a whole file; an IPC handler written inline at module level.
            Some(format!("file:{}", f.path))
        } else {
            None
        };
        entries.push(EntryPoint {
            file: f.path.to_string(),
            line: e.line,
            kind: e.kind,
            name: e.name.clone(),
            target,
        });
    }
    FileResult {
        calls,
        stats,
        entries,
    }
}

fn resolve_call(
    ctx: &Ctx<'_>,
    binds: &[Bindings<'_>],
    h: &Hier<'_>,
    rust_paths: &RustCallPaths,
    fi: usize,
    c: &Call,
    caller: Option<&Def>,
) -> Out {
    match resolve_call_by_rules(ctx, binds, h, fi, c, caller) {
        Out::Unresolved if c.form == Form::Path && !c.path.is_empty() => {
            via_rust_path(ctx, binds, h, rust_paths, fi, c).unwrap_or(Out::Unresolved)
        }
        out => out,
    }
}

/// A Rust `path::name()` through the file its path leads to ([`RustCallPaths`]): `name`
/// as that file exports it (re-exports followed), or, when the path ends in a type,
/// `Type::name` defined with that type.
fn via_rust_path(
    ctx: &Ctx<'_>,
    binds: &[Bindings<'_>],
    h: &Hier<'_>,
    rust_paths: &RustCallPaths,
    fi: usize,
    c: &Call,
) -> Option<Out> {
    let f = &ctx.files[fi];
    if f.fam != Fam::Rust {
        return None;
    }
    let key = (f.path.to_string(), c.scope.clone(), c.path.clone());
    let &t = ctx.by_path.get(rust_paths.get(&key)?.as_str())?;
    if ctx.files[t].fam != Fam::Rust {
        return None;
    }
    let last = c.path.last()?.as_str();
    let mut v = Vec::new();
    if starts_upper(last) {
        let mut types = Vec::new();
        for s in ctx.export_lookup(binds, t, last, Via::Named, 3, &mut Vec::new()) {
            let ty = &ctx.symbols[s as usize];
            if ty.kind.is_type() {
                // The type's own name: `last` may be an alias (`pub use a::Foo as Bar`).
                let tf = ctx.owner[s as usize];
                v.extend(ctx.members(tf, &ty.name, &c.name).iter().copied());
                types.push(s);
            }
        }
        if v.is_empty()
            && let Some(o) = inherited_of_one(ctx, h, &types, &c.name, Confidence::Resolved)
        {
            return Some(o);
        }
    } else {
        v = ctx.export_lookup(binds, t, &c.name, Via::Named, 3, &mut Vec::new());
    }
    ctx.decide(&v, Confidence::Resolved)
}

/// The rules of the module docs, in order (see [`resolve_call`] for the Rust path rule).
fn resolve_call_by_rules(
    ctx: &Ctx<'_>,
    binds: &[Bindings<'_>],
    h: &Hier<'_>,
    fi: usize,
    c: &Call,
    caller: Option<&Def>,
) -> Out {
    let f = &ctx.files[fi];
    let b = &binds[fi];
    let fam = f.fam;
    let name = c.name.as_str();
    let container = caller.and_then(|d| d.container.as_deref());
    match c.form {
        Form::Super => {
            // Python `super(Class, self)` naming another class than the caller's.
            if c.qualifier.is_some() && c.qualifier.as_deref() != container {
                return Out::Unresolved;
            }
            caller_key(ctx, binds, fi, fam, container)
                .and_then(|(k, conf)| inherited(ctx, h, k, name, conf))
                .unwrap_or(Out::Unresolved)
        }
        Form::SelfCall => {
            if fam == Fam::Gd && container.is_none() {
                return ctx
                    .decide(ctx.top(fi, name), Confidence::Exact)
                    .or_else(|| inherited(ctx, h, TKey::Script(fi), name, Confidence::Exact))
                    .unwrap_or(Out::Unresolved);
            }
            let Some(cont) = container else {
                return Out::Unresolved;
            };
            if let Some(o) = ctx.decide(ctx.members(fi, cont, name), Confidence::Exact) {
                return o;
            }
            // An impl block or an out-of-class definition in another file.
            if matches!(fam, Fam::Rust | Fam::C)
                && let Some(v) = ctx.members_global.get(&(fam, cont, name))
            {
                return ctx
                    .decide(v, Confidence::Heuristic)
                    .unwrap_or(Out::Unresolved);
            }
            caller_key(ctx, binds, fi, fam, container)
                .and_then(|(k, conf)| inherited(ctx, h, k, name, conf))
                .unwrap_or(Out::Unresolved)
        }
        Form::Plain => {
            if matches!(fam, Fam::Java | Fam::Cs | Fam::C | Fam::Gd)
                && let Some(cont) = container
            {
                if let Some(o) = ctx.decide(ctx.members(fi, cont, name), Confidence::Exact) {
                    return o;
                }
                // The class's other files: a C++ header, a C# partial class.
                let v: Vec<Sid> = b
                    .scope
                    .iter()
                    .chain(b.globs.iter())
                    .flat_map(|&s| ctx.members(s, cont, name).iter().copied())
                    .collect();
                if let Some(o) = ctx.decide(&v, Confidence::Resolved) {
                    return o;
                }
                // A method the class inherits.
                if let Some(o) = caller_key(ctx, binds, fi, fam, container)
                    .and_then(|(k, conf)| inherited(ctx, h, k, name, conf))
                {
                    return o;
                }
            }
            if let Some(o) = ctx.decide(ctx.top(fi, name), Confidence::Exact) {
                return o;
            }
            // GDScript: a function of the script the file extends.
            if fam == Fam::Gd
                && container.is_none()
                && let Some(o) = inherited(ctx, h, TKey::Script(fi), name, Confidence::Exact)
            {
                return o;
            }
            if let Some(bd) = b.by_name.get(name) {
                return via_binding(ctx, binds, bd, name);
            }
            if fam == Fam::Go {
                let v: Vec<Sid> = b
                    .scope
                    .iter()
                    .flat_map(|&s| ctx.top(s, name).iter().copied())
                    .collect();
                if let Some(o) = ctx.decide(&v, Confidence::Resolved) {
                    return o;
                }
            }
            let v = glob_lookup(ctx, binds, fi, name, false);
            if let Some(o) = ctx.decide(&v, Confidence::Resolved) {
                return o;
            }
            match fam {
                Fam::C => ctx
                    .exported_top
                    .get(&(fam, name))
                    .and_then(|v| ctx.decide(v, Confidence::Heuristic))
                    .unwrap_or(Out::Unresolved),
                Fam::Js if !f.code.module => ctx
                    .script_top
                    .get(name)
                    .and_then(|v| ctx.decide(v, Confidence::Heuristic))
                    .unwrap_or(Out::Unresolved),
                _ => Out::Unresolved,
            }
        }
        Form::New => {
            if let Some(q) = &c.qualifier {
                return match b.by_name.get(q.as_str()) {
                    Some(bd) if !bd.external => {
                        let mut v = Vec::new();
                        for &t in &bd.files {
                            v.extend(ctx.export_lookup(
                                binds,
                                t,
                                name,
                                Via::Named,
                                3,
                                &mut Vec::new(),
                            ));
                        }
                        v.retain(|&s| ctx.symbols[s as usize].kind.is_type());
                        ctx.decide(&v, Confidence::Resolved)
                            .unwrap_or(Out::Unresolved)
                    }
                    _ => Out::Unresolved,
                };
            }
            resolve_type(ctx, binds, fi, name).unwrap_or(Out::Unresolved)
        }
        Form::Member | Form::Path => {
            if let Some(o) = via_field(ctx, binds, h, fi, c, container) {
                return o;
            }
            let Some(q) = c.qualifier.as_deref() else {
                return if c.form == Form::Member {
                    member_heuristic(ctx, binds, fi, name)
                } else {
                    Out::Unresolved
                };
            };
            // Go: the method's receiver variable is its `self`.
            if fam == Fam::Go
                && let Some(d) = caller
                && d.receiver.as_deref() == Some(q)
                && let Some(cont) = d.container.as_deref()
            {
                if let Some(o) = ctx.decide(ctx.members(fi, cont, name), Confidence::Exact) {
                    return o;
                }
                let v: Vec<Sid> = b
                    .scope
                    .iter()
                    .flat_map(|&s| ctx.members(s, cont, name).iter().copied())
                    .collect();
                // A method of an embedded type.
                return ctx
                    .decide(&v, Confidence::Resolved)
                    .or_else(|| {
                        caller_key(ctx, binds, fi, fam, Some(cont))
                            .and_then(|(k, conf)| inherited(ctx, h, k, name, conf))
                    })
                    .unwrap_or(Out::Unresolved);
            }
            if let Some(bd) = b.by_name.get(q) {
                return via_qualifier(ctx, binds, h, bd, q, name);
            }
            // A JS object literal of this file: `api.start()` of `const api = {…}`.
            if fam == Fam::Js
                && let Some(o) = ctx.decide(ctx.members(fi, q, name), Confidence::Exact)
            {
                return o;
            }
            // A type of this file: `Type.f()` / `Type::f()`.
            let local = ctx.top_types(fi, q);
            if !local.is_empty() {
                if let Some(o) = ctx.decide(ctx.members(fi, q, name), Confidence::Exact) {
                    return o;
                }
                if matches!(fam, Fam::Rust | Fam::C)
                    && let Some(v) = ctx.members_global.get(&(fam, q, name))
                {
                    return ctx
                        .decide(v, Confidence::Heuristic)
                        .unwrap_or(Out::Unresolved);
                }
                return inherited_of_one(ctx, h, &local, name, Confidence::Exact)
                    .unwrap_or(Out::Unresolved);
            }
            if matches!(fam, Fam::Java | Fam::Cs | Fam::C) {
                // A type of the package / an included file / a used namespace. Its
                // methods may be defined in another of those files (a C++ class declared
                // in a header, defined in the source file next to it).
                let set: Vec<usize> = b.scope.iter().chain(b.globs.iter()).copied().collect();
                let mut v = Vec::new();
                let types: Vec<Sid> = set.iter().flat_map(|&s| ctx.top_types(s, q)).collect();
                if !types.is_empty() {
                    for &s in &set {
                        v.extend(ctx.members(s, q, name).iter().copied());
                    }
                }
                if let Some(o) = ctx.decide(&v, Confidence::Resolved) {
                    return o;
                }
                if let Some(o) = inherited_of_one(ctx, h, &types, name, Confidence::Resolved) {
                    return o;
                }
                if let Some(types) = ctx.exported_types.get(&(fam, q))
                    && let Some(Out::Hit(t, _)) = ctx.decide(types, Confidence::Heuristic)
                {
                    let tf = ctx.owner[t as usize];
                    return ctx
                        .decide(ctx.members(tf, q, name), Confidence::Heuristic)
                        .or_else(|| inherited(ctx, h, ctx.key_of(t), name, Confidence::Heuristic))
                        .unwrap_or(Out::Unresolved);
                }
            }
            if c.form == Form::Path || (fam == Fam::Gd && starts_upper(q)) {
                return Out::Unresolved;
            }
            member_heuristic(ctx, binds, fi, name)
        }
    }
}

/// `self.field.name()` through the declared type of `field` in `container`, the caller's
/// type (see the module docs). `None` when the field or its type is unknown here, or the
/// type is an interface, a trait, an alias or a TS abstract or ambient class that defines
/// no such method, or a Rust type with no `impl` of it in its crate: the other rules
/// decide. Another class, struct or enum without it: unresolved.
fn via_field(
    ctx: &Ctx<'_>,
    binds: &[Bindings<'_>],
    h: &Hier<'_>,
    fi: usize,
    c: &Call,
    container: Option<&str>,
) -> Option<Out> {
    let field = c.field.as_deref()?;
    let container = container?;
    let f = &ctx.files[fi];
    let ty = f
        .code
        .fields
        .iter()
        .find(|x| x.container == container && x.name == field)?
        .ty
        .as_str();
    let (t, conf) = match resolve_type(ctx, binds, fi, ty)? {
        Out::Hit(t, conf) => (t, conf),
        Out::Ambiguous => return Some(Out::Ambiguous),
        Out::Unresolved => return None,
    };
    let sym = &ctx.symbols[t as usize];
    let tf = ctx.owner[t as usize];
    let conf = match conf {
        Confidence::Heuristic => Confidence::Heuristic,
        Confidence::Exact | Confidence::Resolved => Confidence::Resolved,
    };
    let type_conf = conf;
    let mut conf = conf;
    let mut v: Vec<Sid> = ctx.members(tf, &sym.name, &c.name).to_vec();
    // Rust: an `impl` block of the type in another file, in its folder or below, else
    // anywhere in its crate (an `impl` may live in any module of the crate). With none, a
    // default method of a trait the type implements; else the method may come from a
    // trait that is not in the repo or `Deref`: the other rules decide.
    if v.is_empty() && ctx.files[tf].fam == Fam::Rust {
        conf = Confidence::Heuristic;
        v = rust_impls(ctx, t, &c.name);
        if v.is_empty() {
            return inherited(ctx, h, ctx.key_of(t), &c.name, type_conf);
        }
    }
    if v.is_empty() {
        // A method the type inherits: its supertypes in lookup order.
        if let Some(o) = inherited(ctx, h, ctx.key_of(t), &c.name, conf) {
            return Some(o);
        }
        // A wrong caller is worse than a missing one. A concrete type of the repo that
        // lacks the method stops here (Rust never gets here, see above): no supertype of
        // the repo defines it (it may come from a base outside the repo, or a base that
        // did not resolve), or it is in another `partial` part of a C# class (not looked
        // up, as for `this.m()`), and a same-named method the other rules find would be a
        // guess. An interface or trait (or an alias), or a TS abstract or ambient class,
        // only declares what an implementation defines: the other rules decide.
        let declares = ctx.files[tf].code.defs[(t - ctx.files[tf].first) as usize].declares;
        return match sym.kind {
            SymbolKind::Class | SymbolKind::Struct | SymbolKind::Enum if !declares => {
                Some(Out::Unresolved)
            }
            _ => None,
        };
    }
    ctx.decide(&v, conf)
}

fn starts_upper(s: &str) -> bool {
    s.chars().next().is_some_and(char::is_uppercase)
}

/// A plain name through its import binding.
fn via_binding(ctx: &Ctx<'_>, binds: &[Bindings<'_>], bd: &Bound<'_>, name: &str) -> Out {
    if bd.external {
        return Out::Unresolved;
    }
    let mut v = Vec::new();
    for &t in &bd.files {
        match bd.kind {
            BindKind::Named => {
                let n = bd.imported.unwrap_or(name);
                match bd.container {
                    Some(c) => v.extend(ctx.members(t, c, n).iter().copied()),
                    None => {
                        v.extend(ctx.export_lookup(binds, t, n, Via::Named, 3, &mut Vec::new()))
                    }
                }
            }
            BindKind::Default => v.extend(ctx.default_of(binds, t, 3)),
            _ => {}
        }
    }
    ctx.decide(&v, Confidence::Resolved)
        .unwrap_or(Out::Unresolved)
}

/// `q.name()` where `q` is bound by an import: a module, a package or a class.
fn via_qualifier(
    ctx: &Ctx<'_>,
    binds: &[Bindings<'_>],
    h: &Hier<'_>,
    bd: &Bound<'_>,
    q: &str,
    name: &str,
) -> Out {
    if bd.external {
        return Out::Unresolved;
    }
    let mut v = Vec::new();
    for &t in &bd.files {
        let fam = ctx.files[t].fam;
        match bd.kind {
            BindKind::Namespace | BindKind::Glob => {
                if fam == Fam::Gd && name == "new" {
                    // `Class.new()` constructs the script's class.
                    v.extend(ctx.files[t].top.values().flatten().copied().filter(|&s| {
                        ctx.files[t].code.defs[(s - ctx.files[t].first) as usize].class_name
                    }));
                } else {
                    v.extend(ctx.export_lookup(binds, t, name, Via::Named, 3, &mut Vec::new()));
                }
            }
            BindKind::Named | BindKind::Default => {
                let tname = match bd.kind {
                    BindKind::Default => ctx.files[t].code.default_export.as_deref(),
                    _ => bd.imported,
                }
                .unwrap_or(q);
                let types: Vec<Sid> = ctx
                    .export_lookup(binds, t, tname, Via::Named, 3, &mut Vec::new())
                    .into_iter()
                    .filter(|&s| ctx.symbols[s as usize].kind.is_type())
                    .collect();
                if !types.is_empty() {
                    let before = v.len();
                    for &s in &types {
                        let tf = ctx.owner[s as usize];
                        v.extend(ctx.members(tf, tname, name).iter().copied());
                    }
                    // A static or class method the type inherits (`Child.create()`).
                    if v.len() == before
                        && let Some(o) =
                            inherited_of_one(ctx, h, &types, name, Confidence::Resolved)
                    {
                        return o;
                    }
                } else {
                    let before = v.len();
                    if fam == Fam::Js {
                        // A member of an exported object literal: `api.start()` of
                        // `export const api = {…}`, `plugin.run()` of `export default {…}`.
                        for s in ctx.export_lookup(binds, t, tname, Via::Named, 3, &mut Vec::new())
                        {
                            let obj = &ctx.symbols[s as usize].name;
                            v.extend(
                                ctx.members(ctx.owner[s as usize], obj, name)
                                    .iter()
                                    .copied(),
                            );
                        }
                        if bd.kind == BindKind::Default
                            && ctx.files[t].code.default_export.is_none()
                        {
                            v.extend(ctx.members(t, "default", name).iter().copied());
                        }
                    }
                    if v.len() == before && ctx.is_module_named(t, tname) {
                        v.extend(ctx.export_lookup(binds, t, name, Via::Named, 3, &mut Vec::new()));
                    }
                }
            }
        }
    }
    ctx.decide(&v, Confidence::Resolved)
        .unwrap_or(Out::Unresolved)
}

/// Names in scope through glob imports (and, for `types`, only type definitions).
fn glob_lookup(
    ctx: &Ctx<'_>,
    binds: &[Bindings<'_>],
    fi: usize,
    name: &str,
    types: bool,
) -> Vec<Sid> {
    let mut v = Vec::new();
    for &g in &binds[fi].globs {
        if matches!(ctx.files[g].fam, Fam::Js | Fam::Py | Fam::Rust) {
            v.extend(ctx.export_lookup(binds, g, name, Via::Glob, 3, &mut Vec::new()));
        } else {
            // A C `static` function is not visible through an include.
            v.extend(
                ctx.top(g, name)
                    .iter()
                    .copied()
                    .filter(|&s| ctx.symbols[s as usize].exported),
            );
        }
    }
    if types {
        v.retain(|&s| ctx.symbols[s as usize].kind.is_type());
    }
    v
}

/// A type named in `new T()` (or a type used as a call target).
fn resolve_type(ctx: &Ctx<'_>, binds: &[Bindings<'_>], fi: usize, name: &str) -> Option<Out> {
    let fam = ctx.files[fi].fam;
    let b = &binds[fi];
    let local = ctx.top_types(fi, name);
    if let Some(o) = ctx.decide(&local, Confidence::Exact) {
        return Some(o);
    }
    if let Some(bd) = b.by_name.get(name) {
        return Some(match via_binding(ctx, binds, bd, name) {
            Out::Hit(s, c) if ctx.symbols[s as usize].kind.is_type() => Out::Hit(s, c),
            Out::Ambiguous => Out::Ambiguous,
            _ => Out::Unresolved,
        });
    }
    let scoped: Vec<Sid> = b
        .scope
        .iter()
        .flat_map(|&s| ctx.top_types(s, name))
        .collect();
    if let Some(o) = ctx.decide(&scoped, Confidence::Resolved) {
        return Some(o);
    }
    let globbed = glob_lookup(ctx, binds, fi, name, true);
    if let Some(o) = ctx.decide(&globbed, Confidence::Resolved) {
        return Some(o);
    }
    let heuristic = match fam {
        Fam::Java | Fam::Cs | Fam::C => ctx.exported_types.get(&(fam, name)),
        Fam::Js if !ctx.files[fi].code.module => ctx.script_top.get(name),
        _ => None,
    };
    heuristic.and_then(|v| ctx.decide(v, Confidence::Heuristic))
}

/// `obj.m()` with an unknown receiver: only when `m` is the single method of that name in
/// the language, not a common name, and defined in this file or one it imports (or, for
/// Go, Java and C#, its package).
fn member_heuristic(ctx: &Ctx<'_>, binds: &[Bindings<'_>], fi: usize, name: &str) -> Out {
    let fam = ctx.files[fi].fam;
    if name.len() < 3 || name.starts_with("__") || STOP.contains(&name) {
        return Out::Unresolved;
    }
    let Some(v) = ctx.methods.get(&(fam, name)) else {
        return Out::Unresolved;
    };
    match ctx.decide(v, Confidence::Heuristic) {
        Some(Out::Hit(..)) if v.iter().any(|x| ctx.overrides.contains(x)) => Out::Unresolved,
        Some(Out::Hit(s, conf)) => {
            let tf = ctx.owner[s as usize];
            let b = &binds[fi];
            if tf == fi || b.deps.contains(&tf) || b.scope.contains(&tf) || b.globs.contains(&tf) {
                Out::Hit(s, conf)
            } else {
                Out::Unresolved
            }
        }
        Some(Out::Ambiguous) => Out::Ambiguous,
        _ => Out::Unresolved,
    }
}

/// Godot main scenes and autoloads, from the `project.godot` metadata.
/// The program a `package.json` `main` runs (an Electron main process, a Node app), as a
/// `main` entry on its source file: the file itself, or, for a build output that is not
/// indexed (`dist/electron/main.js`), the source it is compiled from
/// ([`cartograph_core::manifest_entry::entry_files`]). A `main` that other files import (tests aside)
/// is a library's entry, not where a program starts, and is left out.
fn npm_main_entries(graph: &Graph) -> Vec<EntryPoint> {
    let files: HashSet<&str> = graph
        .nodes_of(NodeKind::File)
        .map(|n| n.path.as_str())
        .collect();
    let imported: HashSet<&str> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import && !looks_like_test(&e.src))
        .map(|e| e.dst.as_str())
        .collect();
    let mut out = Vec::new();
    for n in graph.nodes_of(NodeKind::Package) {
        let (Some(dir), Some(main)) = (n.meta["dir"].as_str(), n.meta["details"]["main"].as_str())
        else {
            continue;
        };
        for path in cartograph_core::manifest_entry::entry_files(dir, main, |p| files.contains(p)) {
            let id = format!("file:{path}");
            if imported.contains(id.as_str()) {
                continue;
            }
            out.push(EntryPoint {
                file: path,
                line: 0,
                kind: EntryKind::Main,
                name: "main".to_string(),
                target: Some(id),
            });
        }
    }
    out
}

/// Extensions of a script an HTML page loads that make it the app's code (with
/// `type="module"`, any script counts).
const PAGE_SCRIPT_EXTS: &[&str] = &[".ts", ".tsx", ".jsx", ".mts"];

/// Where a browser app starts: the module scripts an `index.html` loads (Vite's
/// `<script type="module" src="/src/main.tsx">`), as `main` entries named by the page, on
/// the script's file. `src` is relative to the page's folder (a leading `/` too: the folder
/// of the page is the dev server's root). `pages` holds `(path, text)` of the pages; a
/// script that is not an indexed file is left out.
pub fn page_entries(pages: &[(&str, &str)], is_file: impl Fn(&str) -> bool) -> Vec<EntryPoint> {
    let mut out = Vec::new();
    for (page, text) in pages {
        let dir = dir_of(page);
        let lower = text.to_ascii_lowercase();
        let mut at = 0;
        while let Some(i) = lower[at..].find("<script") {
            let start = at + i;
            let end = lower[start..].find('>').map_or(lower.len(), |e| start + e);
            at = end;
            let tag = &text[start..end];
            let tag_lower = &lower[start..end];
            let module =
                tag_lower.contains("type=\"module\"") || tag_lower.contains("type='module'");
            let Some(src) = attr(tag, tag_lower, "src") else {
                continue;
            };
            let src = src.split(['?', '#']).next().unwrap_or("");
            if src.contains("://") || src.starts_with("//") || src.is_empty() {
                continue;
            }
            if !module && !PAGE_SCRIPT_EXTS.iter().any(|e| src.ends_with(e)) {
                continue;
            }
            let rel = src.trim_start_matches('/');
            let path = if dir.is_empty() {
                rel.to_string()
            } else {
                format!("{dir}/{rel}")
            };
            if !is_file(&path) {
                continue;
            }
            out.push(EntryPoint {
                target: Some(format!("file:{path}")),
                file: path,
                line: 0,
                kind: EntryKind::Main,
                name: (*page).to_string(),
            });
        }
    }
    out
}

/// The value of attribute `name` in an HTML tag (quoted with `"` or `'`).
fn attr<'t>(tag: &'t str, lower: &str, name: &str) -> Option<&'t str> {
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let k = from + i;
        from = k + name.len();
        let before = lower[..k].chars().last();
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let rest = lower[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            continue;
        }
        let open = lower.len() - rest.len() + 1;
        let close = lower[open..].find(quote)? + open;
        return tag.get(open..close);
    }
    None
}

/// A test file by its path alone (a `test`/`tests`/`__tests__`/`e2e`/`spec` folder, a
/// `.test.`/`.spec.` name): what a test imports says nothing about who runs a file.
fn looks_like_test(id: &str) -> bool {
    let path = id.strip_prefix("file:").unwrap_or(id);
    let (dirs, name) = path.rsplit_once('/').unwrap_or(("", path));
    dirs.split('/').any(|d| {
        matches!(
            d,
            "test" | "tests" | "__tests__" | "__mocks__" | "e2e" | "spec" | "specs"
        )
    }) || name.contains(".test.")
        || name.contains(".spec.")
}

fn godot_entries(graph: &Graph) -> Vec<EntryPoint> {
    let mut out = Vec::new();
    for n in graph.nodes_of(NodeKind::File) {
        let Some(list) = n.meta["godot"]["entry_points"].as_array() else {
            continue;
        };
        for e in list {
            let (kind, name) = match e["kind"].as_str() {
                Some("autoload") => (
                    EntryKind::GodotAutoload,
                    e["name"].as_str().unwrap_or("autoload").to_string(),
                ),
                _ => (EntryKind::GodotMainScene, "main scene".to_string()),
            };
            out.push(EntryPoint {
                file: n.path.clone(),
                line: e["line"].as_u64().map_or(0, |l| l as u32),
                kind,
                name,
                target: e["target"].as_str().map(str::to_string),
            });
        }
    }
    out
}
