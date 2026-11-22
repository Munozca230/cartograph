//! Supertypes: what each class, struct, interface or trait extends or implements (and what a
//! GDScript script extends), and the walk that finds a method a type inherits.
//!
//! A supertype name resolves like a type name ([`resolve_type`]): the file itself, its
//! imports, its package, then (Java, C#, C/C++, JS scripts) a name unique in the repo, which
//! makes the edge `heuristic`. A qualified name (`ns.Base`, `models.Model`) goes through the
//! binding of its qualifier (a library stops it); with an unknown qualifier it resolves only
//! as a nested type of that name, or in Java (the package must be the type's folder), Rust
//! (the same crate, `heuristic`), C#, C++ and Go (`heuristic`). A name that would resolve to
//! the class itself (`class Config(Config)`) is looked up through the file's imports only.
//! Rust `impl Trait for Type` gives `Type` the supertype `Trait`; a GDScript `extends` names a
//! script, by path or by global class name, or an inner class.
//!
//! Lookup order ([`ancestors`]): depth first over the supertypes in declared order, at most
//! [`MAX_DEPTH`] hops, each type once. Every language with both lists the base class before
//! the interfaces (`extends` before `implements`, a C# base class first), so the class chain
//! is searched before any interface, as Java and C# do (a breadth-first walk would stop at an
//! interface's declaration before the base class's implementation). Python follows its MRO:
//! depth first, a type shared by several bases kept at its last place (C3 for the usual
//! hierarchies). A supertype that is not a type of the repo ends the walk: the method may be
//! defined there. Only [`Super::iface`] supertypes (interfaces, traits, `object`) are passed.

use super::{Bindings, Ctx, Fam, FileIx, GraphIx, Out, Sid, Via, resolve_type, via_binding};
use crate::symbols::Super;
use cartograph_core::{Confidence, SuperType, SymbolKind};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

/// Hops followed from a type to its supertypes.
pub(super) const MAX_DEPTH: usize = 5;

/// A type whose methods a call can reach: a type definition (the first sid of its id), or a
/// GDScript script (the file is the class: its functions have no container).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum TKey {
    Ty(Sid),
    Script(usize),
}

/// One declared supertype.
struct Edge<'a> {
    name: &'a str,
    /// The type it resolved to, and how.
    target: Option<(TKey, Confidence)>,
    iface: bool,
    file: usize,
    line: u32,
}

/// Every type's supertypes.
pub(super) struct Hier<'a> {
    supers: HashMap<TKey, Vec<Edge<'a>>>,
}

/// A GDScript `class_name` definition (the whole file, see `def.class_name`).
fn is_class_name(f: &FileIx<'_>, s: Sid) -> bool {
    f.code.defs[(s - f.first) as usize].class_name
}

impl Ctx<'_> {
    /// The type definition `s` stands for: a GDScript `class_name` is its script.
    pub(super) fn key_of(&self, s: Sid) -> TKey {
        let fi = self.owner[s as usize];
        let f = &self.files[fi];
        if f.fam == Fam::Gd && is_class_name(f, s) {
            TKey::Script(fi)
        } else {
            TKey::Ty(self.canon[s as usize])
        }
    }

    /// The id of a type: its symbol, or for a script its `class_name` (else `file:<path>`).
    fn key_id(&self, k: TKey) -> String {
        match k {
            TKey::Ty(s) => self.ids[s as usize].clone(),
            TKey::Script(fi) => {
                let f = &self.files[fi];
                (f.first..f.first + f.code.defs.len() as Sid)
                    .find(|&s| is_class_name(f, s))
                    .map_or_else(
                        || format!("file:{}", f.path),
                        |s| self.ids[s as usize].clone(),
                    )
            }
        }
    }
}

impl<'a> Hier<'a> {
    pub(super) fn build(ctx: &Ctx<'a>, binds: &[Bindings<'a>], g: &GraphIx<'_>) -> Hier<'a> {
        let per_file: Vec<Vec<(TKey, Edge<'a>)>> = (0..ctx.files.len())
            .into_par_iter()
            .map(|fi| edges_of(ctx, binds, g, fi))
            .collect();
        let mut supers: HashMap<TKey, Vec<Edge<'a>>> = HashMap::new();
        for (k, e) in per_file.into_iter().flatten() {
            let list = supers.entry(k).or_default();
            let target = e.target.map(|t| t.0);
            if !list
                .iter()
                .any(|x| x.name == e.name && x.target.map(|t| t.0) == target)
            {
                list.push(e);
            }
        }
        Hier { supers }
    }

    /// Every edge, as stored.
    pub(super) fn rows(&self, ctx: &Ctx<'_>) -> Vec<SuperType> {
        let mut out = Vec::new();
        for (&k, edges) in &self.supers {
            let sub = ctx.key_id(k);
            for e in edges {
                out.push(SuperType {
                    sub: sub.clone(),
                    name: e.name.to_string(),
                    target: e.target.map(|(t, _)| ctx.key_id(t)),
                    confidence: e.target.map(|(_, c)| c),
                    file: ctx.files[e.file].path.to_string(),
                    line: e.line,
                });
            }
        }
        out
    }
}

/// The supertypes a file declares: its definitions', its Rust trait impls', its script's.
fn edges_of<'a>(
    ctx: &Ctx<'a>,
    binds: &[Bindings<'a>],
    g: &GraphIx<'_>,
    fi: usize,
) -> Vec<(TKey, Edge<'a>)> {
    let f = &ctx.files[fi];
    let mut out = Vec::new();
    for (i, d) in f.code.defs.iter().enumerate() {
        let sub = ctx.key_of(f.first + i as Sid);
        for Super { name, iface } in &d.supers {
            let target = resolve_super(ctx, binds, g, fi, name, Some(sub));
            out.push((
                sub,
                Edge {
                    name,
                    target,
                    iface: *iface,
                    file: fi,
                    line: d.line,
                },
            ));
        }
    }
    for imp in &f.code.impls {
        let Some((sub @ TKey::Ty(_), _)) = resolve_super(ctx, binds, g, fi, &imp.ty, None) else {
            continue;
        };
        let target = resolve_super(ctx, binds, g, fi, &imp.tr, Some(sub));
        out.push((
            sub,
            Edge {
                name: &imp.tr,
                target,
                iface: true,
                file: fi,
                line: imp.line,
            },
        ));
    }
    if let Some(e) = &f.code.extends {
        let sub = TKey::Script(fi);
        out.push((
            sub,
            Edge {
                name: &e.name,
                target: gd_super(ctx, binds, g, fi, &e.name, sub),
                iface: false,
                file: fi,
                line: e.line,
            },
        ));
    }
    out
}

fn is_ident(s: &str) -> bool {
    s.starts_with(|c: char| c.is_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// The repo type a supertype name written in file `fi` stands for (see the module docs).
/// `sub` is the type that declares it.
fn resolve_super(
    ctx: &Ctx<'_>,
    binds: &[Bindings<'_>],
    g: &GraphIx<'_>,
    fi: usize,
    name: &str,
    sub: Option<TKey>,
) -> Option<(TKey, Confidence)> {
    let f = &ctx.files[fi];
    if f.fam == Fam::Gd {
        return gd_super(ctx, binds, g, fi, name, sub?);
    }
    let segs: Vec<&str> = name.split("::").flat_map(|x| x.split('.')).collect();
    if !segs.iter().all(|s| is_ident(s)) {
        return None;
    }
    let last = *segs.last()?;
    let (s, conf) = if segs.len() == 1 {
        match resolve_type(ctx, binds, fi, last)? {
            // `class Config(Config)`: the base is the imported one, not the class itself.
            Out::Hit(s, _) if Some(ctx.key_of(s)) == sub => {
                match via_binding(ctx, binds, binds[fi].by_name.get(last)?, last) {
                    Out::Hit(s, c) => (s, c),
                    _ => return None,
                }
            }
            Out::Hit(s, c) => (s, c),
            _ => return None,
        }
    } else {
        let q = segs[0];
        match binds[fi].by_name.get(q) {
            Some(bd) if bd.external => return None,
            // A module or namespace and the type it exports, or a type and its nested type.
            Some(bd) => {
                let mut v = Vec::new();
                for &t in &bd.files {
                    let found: Vec<Sid> = ctx
                        .export_lookup(binds, t, last, Via::Named, 3, &mut Vec::new())
                        .into_iter()
                        .filter(|&s| ctx.symbols[s as usize].kind.is_type())
                        .collect();
                    if found.is_empty() {
                        v.extend(ctx.top_types(t, last));
                    } else {
                        v.extend(found);
                    }
                }
                match ctx.decide(&v, Confidence::Resolved)? {
                    Out::Hit(s, c) => (s, c),
                    _ => return None,
                }
            }
            None => {
                let (s, c) = match resolve_type(ctx, binds, fi, last)? {
                    Out::Hit(s, c) => (s, c),
                    _ => return None,
                };
                let sym = &ctx.symbols[s as usize];
                let tf = &ctx.files[ctx.owner[s as usize]];
                let pkg = segs[..segs.len() - 1].join("/");
                if segs.len() == 2 && sym.container.as_deref() == Some(q) {
                    (s, c) // `Outer.Inner`
                } else {
                    match f.fam {
                        Fam::Java if tf.dir == pkg || tf.dir.ends_with(&format!("/{pkg}")) => {
                            (s, c)
                        }
                        Fam::Rust if tf.krate == f.krate => (s, Confidence::Heuristic),
                        Fam::Cs | Fam::C | Fam::Go => (s, Confidence::Heuristic),
                        _ => return None,
                    }
                }
            }
        }
    };
    if !ctx.symbols[s as usize].kind.is_type() {
        return None;
    }
    Some((ctx.key_of(s), conf))
}

/// A GDScript `extends`: a script by path (`"res://base.gd"`) or by global class name, or an
/// inner class of this file or of a global class (`Base.Inner`). Engine classes are not in
/// the repo.
fn gd_super(
    ctx: &Ctx<'_>,
    binds: &[Bindings<'_>],
    g: &GraphIx<'_>,
    fi: usize,
    name: &str,
    sub: TKey,
) -> Option<(TKey, Confidence)> {
    let f = &ctx.files[fi];
    if let Some(path) = name.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        let want = format!("extends {path}");
        let (_, t) = g.sites_of.get(f.path)?.iter().find(|(sp, _)| *sp == want)?;
        let &ti = ctx.by_path.get(t.strip_prefix("file:")?)?;
        return (ctx.files[ti].fam == Fam::Gd).then_some((TKey::Script(ti), Confidence::Resolved));
    }
    let segs: Vec<&str> = name.split('.').collect();
    if !segs.iter().all(|s| is_ident(s)) || segs.len() > 2 {
        return None;
    }
    if segs.len() == 1 {
        let local: Vec<Sid> = ctx
            .top_types(fi, name)
            .into_iter()
            .filter(|&s| ctx.key_of(s) != sub && !is_class_name(f, s))
            .collect();
        if let Some(Out::Hit(s, c)) = ctx.decide(&local, Confidence::Exact) {
            return Some((ctx.key_of(s), c));
        }
    }
    // A global class name: the site of the `extends` itself, else the name used elsewhere
    // in the file.
    let want = format!("extends {}", segs[0]);
    let site = g
        .sites_of
        .get(f.path)
        .into_iter()
        .flatten()
        .find(|(sp, _)| *sp == want);
    let t = match site {
        Some((_, t)) => *ctx.by_path.get(t.strip_prefix("file:")?)?,
        None => {
            let bd = binds[fi].by_name.get(segs[0])?;
            match bd.files[..] {
                [t] if !bd.external => t,
                _ => return None,
            }
        }
    };
    if ctx.files[t].fam != Fam::Gd {
        return None;
    }
    if segs.len() == 1 {
        return Some((TKey::Script(t), Confidence::Resolved));
    }
    let inner: Vec<Sid> = ctx
        .top_types(t, segs[1])
        .into_iter()
        .filter(|&s| !is_class_name(&ctx.files[t], s))
        .collect();
    match ctx.decide(&inner, Confidence::Resolved)? {
        Out::Hit(s, c) => Some((ctx.key_of(s), c)),
        _ => None,
    }
}

/// The type of the caller's method (`container`, or for GDScript top-level functions the
/// script): a type of this file, else one its imports or package reach.
pub(super) fn caller_key(
    ctx: &Ctx<'_>,
    binds: &[Bindings<'_>],
    fi: usize,
    fam: Fam,
    container: Option<&str>,
) -> Option<(TKey, Confidence)> {
    if fam == Fam::Gd && container.is_none() {
        return Some((TKey::Script(fi), Confidence::Exact));
    }
    let cont = container?;
    let local = ctx.top_types(fi, cont);
    let found = if local.is_empty() {
        resolve_type(ctx, binds, fi, cont)?
    } else {
        ctx.decide(&local, Confidence::Exact)?
    };
    match found {
        Out::Hit(s, c) if ctx.symbols[s as usize].kind.is_type() => Some((ctx.key_of(s), c)),
        _ => None,
    }
}

/// Rust: methods `name` of type `t` in `impl` blocks of other files, in the type's folder or
/// below, else anywhere in its crate (an `impl` may live in any module of the crate).
pub(super) fn rust_impls(ctx: &Ctx<'_>, t: Sid, name: &str) -> Vec<Sid> {
    let sym = &ctx.symbols[t as usize];
    let tf = &ctx.files[ctx.owner[t as usize]];
    let impls = ctx
        .members_global
        .get(&(Fam::Rust, sym.name.as_str(), name))
        .map_or(&[][..], Vec::as_slice);
    let file_of = |s: Sid| &ctx.files[ctx.owner[s as usize]];
    let v: Vec<Sid> = impls
        .iter()
        .copied()
        .filter(|&s| crate::paths::is_ancestor(tf.dir, file_of(s).dir))
        .collect();
    if !v.is_empty() {
        return v;
    }
    impls
        .iter()
        .copied()
        .filter(|&s| file_of(s).krate == tf.krate)
        .collect()
}

/// Methods `name` that type `k` defines itself, and how sure that is: in its definition,
/// a GDScript script's functions, a Rust `impl` block elsewhere in its crate (`heuristic`),
/// a C++ out-of-class definition in the source file of the class's header.
fn own_methods(ctx: &Ctx<'_>, k: TKey, name: &str) -> (Vec<Sid>, Confidence) {
    match k {
        TKey::Script(fi) => (
            ctx.top(fi, name)
                .iter()
                .copied()
                .filter(|&s| ctx.symbols[s as usize].kind == SymbolKind::Function)
                .collect(),
            Confidence::Resolved,
        ),
        TKey::Ty(t) => {
            let sym = &ctx.symbols[t as usize];
            let tf = ctx.owner[t as usize];
            let v = ctx.members(tf, &sym.name, name).to_vec();
            if !v.is_empty() {
                return (v, Confidence::Resolved);
            }
            match ctx.files[tf].fam {
                Fam::Rust => (rust_impls(ctx, t, name), Confidence::Heuristic),
                Fam::C => {
                    let (dir, stem) = (ctx.files[tf].dir, super::stem_of(ctx.files[tf].path));
                    let v = ctx
                        .members_global
                        .get(&(Fam::C, sym.name.as_str(), name))
                        .into_iter()
                        .flatten()
                        .copied()
                        .filter(|&s| {
                            let f = &ctx.files[ctx.owner[s as usize]];
                            f.dir == dir && super::stem_of(f.path) == stem
                        })
                        .collect();
                    (v, Confidence::Resolved)
                }
                _ => (Vec::new(), Confidence::Resolved),
            }
        }
    }
}

/// The supertypes of `k` in method-lookup order (see the module docs), each with the weakest
/// confidence on the way to it; `None` where a supertype outside the repo ends the walk.
fn ancestors(ctx: &Ctx<'_>, h: &Hier<'_>, k: TKey) -> Vec<Option<(TKey, Confidence)>> {
    let python = matches!(k, TKey::Ty(s) if ctx.files[ctx.owner[s as usize]].fam == Fam::Py);
    let mut out = Vec::new();
    if python {
        let mut seq = Vec::new();
        walk_all(h, k, k, 0, Confidence::Exact, &mut seq);
        let mut seen = HashSet::new();
        for a in seq.into_iter().rev() {
            if let Some((t, _)) = a
                && !seen.insert(t)
            {
                continue;
            }
            out.push(a);
        }
        out.reverse();
    } else {
        let mut visited = HashSet::from([k]);
        dfs(h, k, 0, Confidence::Exact, &mut visited, &mut out);
    }
    out
}

fn dfs(
    h: &Hier<'_>,
    k: TKey,
    depth: usize,
    conf: Confidence,
    visited: &mut HashSet<TKey>,
    out: &mut Vec<Option<(TKey, Confidence)>>,
) {
    if depth == MAX_DEPTH {
        return;
    }
    for e in h.supers.get(&k).into_iter().flatten() {
        match e.target {
            None if e.iface => {}
            None => out.push(None),
            Some((t, c)) => {
                if visited.insert(t) {
                    let c = conf.max(c);
                    out.push(Some((t, c)));
                    dfs(h, t, depth + 1, c, visited, out);
                }
            }
        }
    }
}

/// Python: every path, depth first, before keeping each type's last place.
fn walk_all(
    h: &Hier<'_>,
    root: TKey,
    k: TKey,
    depth: usize,
    conf: Confidence,
    out: &mut Vec<Option<(TKey, Confidence)>>,
) {
    if depth == MAX_DEPTH || out.len() > 256 {
        return;
    }
    for e in h.supers.get(&k).into_iter().flatten() {
        match e.target {
            None if e.iface => {}
            None => out.push(None),
            Some((t, _)) if t == root => {}
            Some((t, c)) => {
                let c = conf.max(c);
                out.push(Some((t, c)));
                walk_all(h, root, t, depth + 1, c, out);
            }
        }
    }
}

/// Method `name` that type `k` inherits: the first supertype in lookup order that defines
/// it. `base` is how sure the type itself is. Never `exact` (another type defines it);
/// `heuristic` when a hop or the type was. `None` when no supertype of the repo defines it
/// before the walk ends.
pub(super) fn inherited(
    ctx: &Ctx<'_>,
    h: &Hier<'_>,
    k: TKey,
    name: &str,
    base: Confidence,
) -> Option<Out> {
    for a in ancestors(ctx, h, k) {
        let (t, c) = a?;
        let (v, own) = own_methods(ctx, t, name);
        if !v.is_empty() {
            let conf = base.max(c).max(own).max(Confidence::Resolved);
            return ctx.decide(&v, conf);
        }
    }
    None
}

/// [`inherited`] for type definitions that are all one symbol (`None` otherwise).
pub(super) fn inherited_of_one(
    ctx: &Ctx<'_>,
    h: &Hier<'_>,
    types: &[Sid],
    name: &str,
    base: Confidence,
) -> Option<Out> {
    let k = ctx.key_of(*types.first()?);
    if types.iter().any(|&s| ctx.key_of(s) != k) {
        return None;
    }
    inherited(ctx, h, k, name, base)
}
