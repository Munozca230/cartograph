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
use cartograph_core::{Confidence, DataSink, SpringBean, SpringInjection, symbol_id};
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

/// Where an entity's documents or rows live.
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
    // Entities, by type.
    let mut entities: HashMap<Sid, Store> = HashMap::new();
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
                None if e.store == "mongo" => (
                    crate::symbols::decapitalize(&e.class),
                    Confidence::Heuristic,
                ),
                None => (snake(&e.class), Confidence::Heuristic),
            };
            let path = ctx.files[fi].path;
            sinks.push(DataSink {
                store: e.store.clone(),
                name: name.clone(),
                entity: Some(id(t)),
                symbol: Some(id(t)),
                via: if e.store == "mongo" {
                    "@Document"
                } else {
                    "@Entity"
                }
                .to_string(),
                file: path.to_string(),
                line: e.line,
                confidence,
            });
            entities.entry(t).or_insert(Store {
                store: e.store.clone(),
                name,
                confidence,
            });
        }
    }
    // Repositories, by type: their entity's store.
    let mut repos: HashMap<Sid, (Sid, String)> = HashMap::new();
    for &fi in &java {
        for r in &ctx.files[fi].code.repos {
            let (Some(t), Some(e)) = (
                own_type(ctx, fi, &r.class),
                type_in(ctx, binds, fi, &r.entity),
            ) else {
                continue;
            };
            let Some(st) = entities.get(&e) else {
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
            repos.insert(t, (e, r.base.clone()));
        }
    }
    for &fi in &java {
        let f = &ctx.files[fi];
        let path = f.path;
        for b in &f.code.beans {
            let t = match b.factory {
                None => own_type(ctx, fi, &b.ty),
                Some(_) => type_in(ctx, binds, fi, &b.ty),
            };
            let Some(t) = t else { continue };
            beans.push(SpringBean {
                ty: id(t),
                name: b.name.clone(),
                stereotype: b.stereotype.clone(),
                file: path.to_string(),
                line: b.line,
                factory: b.factory.as_ref().map(|q| symbol_id(path, q)),
                primary: b.primary,
                qualifiers: b.qualifiers.clone(),
            });
        }
        for i in &f.code.injections {
            let (Some(owner), Some(t)) =
                (own_type(ctx, fi, &i.class), type_in(ctx, binds, fi, &i.ty))
            else {
                continue;
            };
            injections.push(SpringInjection {
                owner: id(owner),
                how: i.how.clone(),
                member: i.member.clone(),
                wrapper: i.wrapper.clone(),
                ty: id(t),
                qualifier: i.qualifier.clone(),
                file: path.to_string(),
                line: i.line,
            });
        }
        let calls_of = callables(ctx, fi);
        let caller = |line: u32| caller_of(&calls_of, line).map(|(_, s)| id(s));
        for s in &f.code.sinks {
            let entity = s.entity.as_deref().and_then(|e| type_in(ctx, binds, fi, e));
            let named = s
                .name
                .as_ref()
                .map(|n| consts.eval(fi, n, 0))
                .filter(|v| v.known() && !v.key.contains('*'));
            let (name, confidence) = match (named, entity.and_then(|e| entities.get(&e))) {
                (Some(v), _) => (v.key, v.confidence),
                (None, Some(st)) => (st.name.clone(), st.confidence.max(Confidence::Resolved)),
                (None, None) => continue,
            };
            let store = entity
                .and_then(|e| entities.get(&e))
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
            let Some((e, _)) = repos.get(&t) else {
                continue;
            };
            let Some(st) = entities.get(e) else { continue };
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
