//! Spring beans, injection points and data sinks, resolved (see `cartograph_core::spring`).
//!
//! Types are read like a call's receiver type: the file's own types, then its imports and
//! package ([`resolve_type`]); a type only its unique name in the repo finds is left out (a
//! library type of a wildcard import). Which beans an injection point gets is decided by
//! the queries.
//!
//! Data sinks, per method: a call that names a collection or an entity class
//! (`getCollection(NAME, E.class)`, `mongoTemplate.find(q, E.class)`), and any call on a
//! Spring Data repository (`orders.save(o)`, inherited methods included). A repository and
//! an entity also get a row of their own (the method is the type itself).

use super::bridge::{Consts, callables};
use super::{Bindings, Ctx, Fam, Out, Sid, caller_of, resolve_type};
use crate::symbols::{Call, Form};
use cartograph_core::{
    Confidence, DataSink, InjectedUse, JavaMethod, JavaSite, ResourceName, SpringBean,
    SpringInjection, symbol_id,
};
use std::collections::HashMap;

/// A type named in file `fi`, when the file or its imports find it.
fn type_in(ctx: &Ctx<'_>, binds: &[Bindings<'_>], fi: usize, name: &str) -> Option<Sid> {
    match resolve_type(ctx, binds, fi, name)? {
        Out::Hit(s, Confidence::Exact | Confidence::Resolved) => Some(s),
        _ => None,
    }
}

/// A type defined in file `fi` itself (one, whatever its nesting).
fn own_type(ctx: &Ctx<'_>, fi: usize, name: &str) -> Option<Sid> {
    match ctx.decide(&ctx.top_types(fi, name), Confidence::Exact)? {
        Out::Hit(s, _) => Some(s),
        _ => None,
    }
}

/// The collection or table a store gives a class by default.
fn default_name(store: &str, class: &str) -> String {
    match store {
        "mongo" => crate::symbols::decapitalize(class),
        "cassandra" => class.to_lowercase(),
        _ => snake(class),
    }
}

/// Where an entity is kept: as its annotation for the repository's store says, else as any
/// of its annotations, else by the store's default.
fn stored(entities: &HashMap<Sid, Vec<Store>>, e: Sid, store: &str, class: &str) -> Option<Store> {
    let said = entities.get(&e);
    if let Some(v) = said
        && let Some(st) = v.iter().find(|s| s.store == store)
    {
        return Some(st.clone());
    }
    if store.is_empty() {
        return said.and_then(|v| v.first().cloned());
    }
    Some(Store {
        store: store.to_string(),
        name: default_name(store, class),
        confidence: Confidence::Heuristic,
    })
}

/// Where an entity's documents or rows live.
#[derive(Clone)]
struct Store {
    store: String,
    name: String,
    confidence: Confidence,
}

/// `OrderLine` → `order_line` (the table Spring Boot's default naming gives an entity).
fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Beans, injection points and data sinks of every Java file.
pub(super) fn run(
    ctx: &Ctx<'_>,
    binds: &[Bindings<'_>],
) -> (Vec<SpringBean>, Vec<SpringInjection>, Vec<DataSink>) {
    let (mut beans, mut injections, mut sinks) = (Vec::new(), Vec::new(), Vec::new());
    let java: Vec<usize> = (0..ctx.files.len())
        .filter(|&fi| ctx.files[fi].fam == Fam::Java)
        .collect();
    if java.iter().all(|&fi| {
        let c = ctx.files[fi].code;
        c.beans.is_empty()
            && c.injections.is_empty()
            && c.entities.is_empty()
            && c.repos.is_empty()
            && c.sinks.is_empty()
    }) {
        return (beans, injections, sinks);
    }
    let id = |s: Sid| ctx.ids[s as usize].clone();
    let consts = Consts::new(ctx, binds);
    // Entities, by type (one class may be kept in several stores).
    let mut entities: HashMap<Sid, Vec<Store>> = HashMap::new();
    for &fi in &java {
        for e in &ctx.files[fi].code.entities {
            let Some(t) = own_type(ctx, fi, &e.class) else {
                continue;
            };
            let named = e
                .name
                .as_ref()
                .map(|n| consts.eval(fi, n, 0))
                .filter(|v| v.known() && !v.key.contains('*'));
            let (name, confidence) = match named {
                Some(v) => (v.key, v.confidence),
                None => (default_name(&e.store, &e.class), Confidence::Heuristic),
            };
            let path = ctx.files[fi].path;
            sinks.push(DataSink {
                store: e.store.clone(),
                name: name.clone(),
                entity: Some(id(t)),
                symbol: Some(id(t)),
                via: match e.store.as_str() {
                    "mongo" => "@Document",
                    "jpa" => "@Entity",
                    _ => "@Table",
                }
                .to_string(),
                file: path.to_string(),
                line: e.line,
                confidence,
            });
            entities.entry(t).or_default().push(Store {
                store: e.store.clone(),
                name,
                confidence,
            });
        }
    }
    // Repositories, by type: their entity's store.
    let mut repos: HashMap<Sid, (Sid, Store)> = HashMap::new();
    for &fi in &java {
        for r in &ctx.files[fi].code.repos {
            let (Some(t), Some(e)) = (
                own_type(ctx, fi, &r.class),
                type_in(ctx, binds, fi, &r.entity),
            ) else {
                continue;
            };
            let Some(st) = stored(&entities, e, &r.store, &ctx.symbols[e as usize].name) else {
                continue;
            };
            sinks.push(DataSink {
                store: st.store.clone(),
                name: st.name.clone(),
                entity: Some(id(e)),
                symbol: Some(id(t)),
                via: format!("{}<{}>", r.base, r.entity),
                file: ctx.files[fi].path.to_string(),
                line: r.line,
                confidence: st.confidence,
            });
            repos.insert(t, (e, st));
        }
    }
    for &fi in &java {
        let f = &ctx.files[fi];
        let path = f.path;
        let calls_of = callables(ctx, fi);
        // A name written as an expression: its value, empty when it is not known.
        let value = |e: &crate::symbols::strexpr::StrExpr| {
            let v = consts.eval(fi, e, 0);
            if v.known() && !v.key.contains('*') {
                v.key
            } else {
                String::new()
            }
        };
        for b in &f.code.beans {
            let t = match b.factory {
                None => own_type(ctx, fi, &b.ty),
                Some(_) => type_in(ctx, binds, fi, &b.ty),
            };
            let Some(t) = t else { continue };
            let mut names: Vec<String> = b.names.iter().map(&value).collect();
            if names.is_empty() {
                names.push(b.default_name.clone());
            }
            let name = names.remove(0);
            beans.push(SpringBean {
                ty: id(t),
                name,
                aliases: names,
                stereotype: b.stereotype.clone(),
                file: path.to_string(),
                line: b.line,
                factory: b.factory.as_ref().map(|q| symbol_id(path, q)),
                primary: b.primary,
                qualifiers: b.qualifiers.iter().map(&value).collect(),
            });
        }
        for i in &f.code.injections {
            let (Some(owner), Some(t)) =
                (own_type(ctx, fi, &i.class), type_in(ctx, binds, fi, &i.ty))
            else {
                continue;
            };
            // Calls on what is injected here: the declaration their receiver comes from
            // is this parameter, or a field that holds it.
            let od = &f.code.defs[(owner - f.first) as usize];
            let mut uses: Vec<InjectedUse> = f
                .code
                .calls
                .iter()
                .filter(|c| c.form == Form::Member)
                .filter_map(|c| {
                    let each = match (&c.recv_decl, &c.field) {
                        (Some(d), _) => {
                            let own = i.param && d.name == i.member && d.line == i.line;
                            let held = i.fields.iter().any(|(n, l)| *n == d.name && *l == d.line);
                            (own || held).then_some(d.each)?
                        }
                        // `this.x.m()` in the receiving class itself.
                        (None, Some(x)) => {
                            let here = (od.start..=od.end).contains(&c.line)
                                && caller_of(&calls_of, c.line).is_none_or(|(d, _)| {
                                    d.container.as_deref() == Some(i.class.as_str())
                                });
                            let held = i.fields.iter().any(|(n, _)| n == x);
                            (here && held).then_some(false)?
                        }
                        _ => return None,
                    };
                    Some(InjectedUse { line: c.line, each })
                })
                .collect();
            uses.sort();
            uses.dedup();
            injections.push(SpringInjection {
                owner: id(owner),
                how: i.how.clone(),
                member: i.member.clone(),
                wrapper: i.wrapper.clone(),
                ty: id(t),
                qualifier: i.qualifier.as_ref().map(value),
                resource: i.resource.as_ref().map(|(e, default)| ResourceName {
                    name: value(e),
                    default: *default,
                }),
                file: path.to_string(),
                line: i.line,
                uses,
            });
        }
        let caller = |line: u32| caller_of(&calls_of, line).map(|(_, s)| id(s));
        for s in &f.code.sinks {
            let entity = s.entity.as_deref().and_then(|e| type_in(ctx, binds, fi, e));
            let named = s
                .name
                .as_ref()
                .map(|n| consts.eval(fi, n, 0))
                .filter(|v| v.known() && !v.key.contains('*'));
            let known = entity.and_then(|e| entities.get(&e)).and_then(|v| {
                let same = v.iter().find(|st| st.store == s.store);
                same.or(v.first()).cloned()
            });
            let (name, confidence) = match (named, &known) {
                (Some(v), _) => (v.key, v.confidence),
                (None, Some(st)) => (st.name.clone(), st.confidence.max(Confidence::Resolved)),
                (None, None) => continue,
            };
            let store = known
                .as_ref()
                .map_or(s.store.clone(), |st| st.store.clone());
            sinks.push(DataSink {
                store,
                name,
                entity: entity.map(id),
                symbol: caller(s.line),
                via: s.what.clone(),
                file: path.to_string(),
                line: s.line,
                confidence,
            });
        }
        if repos.is_empty() {
            continue;
        }
        // Calls on a repository: through a typed receiver or a field.
        for c in &f.code.calls {
            if c.form != Form::Member {
                continue;
            }
            let Some(t) = receiver(ctx, binds, fi, c, &calls_of) else {
                continue;
            };
            let Some((e, st)) = repos.get(&t) else {
                continue;
            };
            sinks.push(DataSink {
                store: st.store.clone(),
                name: st.name.clone(),
                entity: Some(id(*e)),
                symbol: caller(c.line),
                via: format!("{}.{}", ctx.symbols[t as usize].name, c.name),
                file: path.to_string(),
                line: c.line,
                confidence: st.confidence.max(Confidence::Resolved),
            });
        }
    }
    (beans, injections, sinks)
}

/// The innermost type of a file whose lines hold `line`.
fn type_at(ctx: &Ctx<'_>, fi: usize, line: u32) -> Option<Sid> {
    let f = &ctx.files[fi];
    f.code
        .defs
        .iter()
        .enumerate()
        .filter(|(_, d)| d.kind.is_type() && (d.start..=d.end).contains(&line))
        .min_by_key(|(_, d)| d.end - d.start)
        .map(|(i, _)| f.first + i as Sid)
}

/// The type of a Java call's receiver: the calling class for a call on itself, the type a
/// variable or a field is declared with.
pub(super) fn receiver_type(
    ctx: &Ctx<'_>,
    binds: &[Bindings<'_>],
    fi: usize,
    c: &Call,
) -> Option<Sid> {
    match c.form {
        Form::Plain | Form::SelfCall => type_at(ctx, fi, c.line),
        Form::Member => {
            if let Some(t) = &c.recv_type {
                return type_in(ctx, binds, fi, t);
            }
            let field = c.field.as_deref()?;
            let own = type_at(ctx, fi, c.line)?;
            let ty = ctx.files[fi]
                .code
                .fields
                .iter()
                .find(|x| x.container == ctx.symbols[own as usize].name && x.name == field)?;
            type_in(ctx, binds, fi, &ty.ty)
        }
        _ => None,
    }
}

/// The sites that say more than their target, with the others of the same line and target.
pub(super) fn notable(mut sites: Vec<JavaSite>) -> Vec<JavaSite> {
    let declared = |s: &JavaSite| {
        let ty = s.target.rsplit_once('.').map_or("", |(ty, _)| ty);
        let recv = s.recv.as_deref().and_then(|r| {
            let (p, n) = r.split_once('#')?;
            Some(format!("{p}#{}", n.rsplit('.').next().unwrap_or(n)))
        });
        recv.is_none_or(|r| r == ty)
    };
    let said: std::collections::HashSet<(u32, String)> = sites
        .iter()
        .filter(|s| s.sup || !declared(s))
        .map(|s| (s.line, s.target.clone()))
        .collect();
    sites.retain(|s| said.contains(&(s.line, s.target.clone())));
    sites
}

/// Every Java method of the repo.
pub(super) fn methods(ctx: &Ctx<'_>) -> Vec<JavaMethod> {
    let mut out = Vec::new();
    for f in ctx.files.iter().filter(|f| f.fam == Fam::Java) {
        for (i, d) in f.code.defs.iter().enumerate() {
            if let Some(m) = &d.java {
                out.push(JavaMethod {
                    id: ctx.ids[f.first as usize + i].clone(),
                    arity: m.arity,
                    is_abstract: m.is_abstract,
                    is_static: m.is_static,
                    private: m.private,
                });
            }
        }
    }
    out
}

/// The type of a call's receiver: its declared type, or the declared type of the field of
/// `this.field.m()` in the class around the call.
fn receiver(
    ctx: &Ctx<'_>,
    binds: &[Bindings<'_>],
    fi: usize,
    c: &Call,
    calls_of: &[(u32, u32, &crate::symbols::Def, u32)],
) -> Option<Sid> {
    if let Some(t) = &c.recv_type {
        return type_in(ctx, binds, fi, t);
    }
    let field = c.field.as_deref()?;
    let (def, _) = caller_of(calls_of, c.line)?;
    let container = def.container.as_deref()?;
    let ty = ctx.files[fi]
        .code
        .fields
        .iter()
        .find(|x| x.container == container && x.name == field)?;
    type_in(ctx, binds, fi, &ty.ty)
}
