//! Spring dependency injection as queries see it (see [`cartograph_core::spring`]): which
//! beans implement a type, which of them an injection point receives (all of them for a
//! list, a set, a map; else the one a qualifier, `@Primary` or the member's name picks),
//! the bean methods a call to an interface method may run, and the collections or tables a
//! method reads or writes.

use cartograph_core::{
    DataSink, JavaSite, SpringBean, SpringInjection, SymbolCall, Symbols, split_symbol_id,
};
use std::collections::{HashMap, HashSet};

/// Hops of supertypes walked from a bean's type (as `hierarchy`).
const SUPER_HOPS: usize = 6;

/// What bean matching reads of the symbol layer (the symbol table implements it, so this
/// module does not depend on the one that shows its answers).
pub trait Types {
    fn spring(&self) -> &SpringTable;
    /// Ids of the direct supertypes of type `id`, those in the repo.
    fn supertypes(&self, id: &str) -> Vec<String>;
    /// Whether a symbol has this id.
    fn exists(&self, id: &str) -> bool;
    /// Whether type `id` is an interface.
    fn is_interface(&self, id: &str) -> bool;
    /// Types defined in a file, as `(name, id)`.
    fn types_in(&self, file: &str) -> Vec<(String, String)>;
}

/// Beans, injection points and data sinks, indexed.
#[derive(Debug, Default)]
pub struct SpringTable {
    pub beans: Vec<SpringBean>,
    pub injections: Vec<SpringInjection>,
    pub sinks: Vec<DataSink>,
    /// Injected type → injection points.
    by_ty: HashMap<String, Vec<usize>>,
    /// Receiving class → injection points.
    by_owner: HashMap<String, Vec<usize>>,
    /// Method (or type) → its sinks.
    sinks_of: HashMap<String, Vec<usize>>,
    /// Each bean's type and its supertypes (see [`SpringTable::link`]).
    ancestors: Vec<HashSet<String>>,
    /// Type → the beans of it and below (tests' too).
    below: HashMap<String, Vec<usize>>,
    /// `sym:path#Leaf`, as method ids name it → the type, for the types some bean is below.
    declarers: HashMap<String, String>,
    /// Method → what is known of it (its overloads together).
    methods: HashMap<String, Method>,
    /// File → line → calls whose receiver says more than their target.
    sites: HashMap<String, HashMap<u32, Vec<JavaSite>>>,
    /// File → line → injection points whose beans a call there is made on, and whether it
    /// reads every element.
    uses: HashMap<String, HashMap<u32, Vec<(usize, bool)>>>,
    /// Files of beans and injection points that lie in a test, fixture or example tree:
    /// their beans are injected only into the injection points of such files.
    tests: HashSet<String>,
}

/// A Java method, its overloads together.
#[derive(Debug, Default)]
pub struct Method {
    /// Every overload is.
    pub private: bool,
    pub is_static: bool,
    pub is_abstract: bool,
    /// The numbers of parameters (-1: any).
    arities: Vec<i32>,
}

impl Method {
    /// One may override the other.
    fn overlaps(&self, o: &Method) -> bool {
        self.arities.contains(&-1)
            || o.arities.contains(&-1)
            || self.arities.iter().any(|a| o.arities.contains(a))
    }
}

impl SpringTable {
    pub fn build(s: &Symbols) -> SpringTable {
        let mut t = SpringTable {
            beans: s.beans.clone(),
            injections: s.injections.clone(),
            sinks: s.sinks.clone(),
            ..SpringTable::default()
        };
        for m in &s.java_methods {
            let e = t.methods.entry(m.id.clone()).or_insert_with(|| Method {
                private: true,
                is_static: true,
                is_abstract: true,
                arities: Vec::new(),
            });
            e.private &= m.private;
            e.is_static &= m.is_static;
            e.is_abstract &= m.is_abstract;
            e.arities.push(m.arity);
        }
        for c in &s.java_sites {
            t.sites
                .entry(c.file.clone())
                .or_default()
                .entry(c.line)
                .or_default()
                .push(c.clone());
        }
        for (i, x) in t.injections.iter().enumerate() {
            for u in &x.uses {
                t.uses
                    .entry(x.file.clone())
                    .or_default()
                    .entry(u.line)
                    .or_default()
                    .push((i, u.each));
            }
            t.by_ty.entry(x.ty.clone()).or_default().push(i);
            t.by_owner.entry(x.owner.clone()).or_default().push(i);
        }
        for (i, x) in t.sinks.iter().enumerate() {
            if let Some(sym) = &x.symbol {
                t.sinks_of.entry(sym.clone()).or_default().push(i);
            }
        }
        t
    }

    /// Read the beans' supertypes from the symbol table and mark the files `in_tests`
    /// says are tests; once, when the table is built.
    pub fn link<T: Types>(
        t: &T,
        in_tests: impl Fn(&str) -> bool,
    ) -> (Vec<HashSet<String>>, HashSet<String>) {
        let ancestors = t
            .spring()
            .beans
            .iter()
            .map(|b| ancestors(t, &b.ty))
            .collect();
        let tests = t
            .spring()
            .beans
            .iter()
            .map(|b| &b.file)
            .chain(t.spring().injections.iter().map(|i| &i.file))
            .filter(|f| in_tests(f))
            .cloned()
            .collect();
        (ancestors, tests)
    }

    /// Set what [`SpringTable::link`] read.
    pub fn set_links(&mut self, links: (Vec<HashSet<String>>, HashSet<String>)) {
        (self.ancestors, self.tests) = links;
        for (i, types) in self.ancestors.iter().enumerate() {
            for ty in types {
                let v = self.below.entry(ty.clone()).or_default();
                if v.is_empty()
                    && let Some((p, _)) = split_symbol_id(ty)
                {
                    self.declarers
                        .insert(format!("sym:{p}#{}", leaf(ty)), ty.clone());
                }
                v.push(i);
            }
        }
    }

    /// The type that declares method `id`, when some bean is of it or below.
    pub fn declarer(&self, id: &str) -> Option<&str> {
        let (ty, _) = id.rsplit_once('.')?;
        self.declarers.get(ty).map(String::as_str)
    }

    /// The application's beans of type `ty` and below.
    pub fn beans_below(&self, ty: &str) -> impl Iterator<Item = &SpringBean> {
        self.below
            .get(ty)
            .into_iter()
            .flatten()
            .map(|&i| &self.beans[i])
            .filter(|b| !self.tests.contains(&b.file))
    }

    /// Method `a` may override method `b` (or the other way): neither is known to be
    /// private or static, and they share a number of parameters.
    pub fn overrides(&self, a: &str, b: &str) -> bool {
        match (self.method(a), self.method(b)) {
            (Some(x), Some(y)) => {
                !x.private && !y.private && !x.is_static && !y.is_static && x.overlaps(y)
            }
            _ => true,
        }
    }

    /// What is known of a Java method.
    pub fn method(&self, id: &str) -> Option<&Method> {
        self.methods.get(id)
    }

    /// The calls to a call's target on its line, when they say more than it.
    pub fn sites(&self, c: &SymbolCall) -> Option<Vec<&JavaSite>> {
        let v: Vec<&JavaSite> = self
            .sites
            .get(&c.file)?
            .get(&c.line)?
            .iter()
            .filter(|s| s.target == c.target)
            .collect();
        (!v.is_empty()).then_some(v)
    }

    /// The injection points whose beans a call at `file:line` is made on, each with whether
    /// it reads every element.
    pub fn injected_at(
        &self,
        file: &str,
        line: u32,
    ) -> impl Iterator<Item = (&SpringInjection, bool)> {
        self.uses
            .get(file)
            .and_then(|m| m.get(&line))
            .into_iter()
            .flatten()
            .map(|&(i, each)| (&self.injections[i], each))
    }

    /// Whether a bean's or injection point's file lies in a test tree.
    pub fn in_tests(&self, file: &str) -> bool {
        self.tests.contains(file)
    }

    pub fn is_empty(&self) -> bool {
        self.beans.is_empty() && self.injections.is_empty() && self.sinks.is_empty()
    }

    /// Injection points of type `ty`.
    pub fn injections_of(&self, ty: &str) -> impl Iterator<Item = &SpringInjection> {
        self.by_ty
            .get(ty)
            .into_iter()
            .flatten()
            .map(|&i| &self.injections[i])
    }

    /// Injection points of class `owner`.
    pub fn injections_into(&self, owner: &str) -> impl Iterator<Item = &SpringInjection> {
        self.by_owner
            .get(owner)
            .into_iter()
            .flatten()
            .map(|&i| &self.injections[i])
    }

    /// The collections or tables method `id` reads or writes itself (or, for a repository
    /// or an entity, the one it maps).
    pub fn sinks_of(&self, id: &str) -> impl Iterator<Item = &DataSink> {
        self.sinks_of
            .get(id)
            .into_iter()
            .flatten()
            .map(|&i| &self.sinks[i])
    }
}

/// The last name of a symbol id (`CardGateway` of `sym:a/CardGateway.java#CardGateway`).
pub fn leaf(id: &str) -> &str {
    split_symbol_id(id)
        .map(|(_, n)| n.rsplit('.').next().unwrap_or(n))
        .unwrap_or(id)
}

/// A bean as people read it: its class, or `name (@Bean)` for one a method makes.
pub fn bean_label(b: &SpringBean) -> String {
    match &b.factory {
        // One whose name is not known: the method that makes it.
        Some(f) if b.name.is_empty() => format!("{} (@Bean)", leaf(f)),
        Some(_) => format!("{} (@Bean)", b.name),
        None => leaf(&b.ty).to_string(),
    }
}

/// Type `ty` and its supertypes up to [`SUPER_HOPS`] hops.
fn ancestors<T: Types>(t: &T, ty: &str) -> HashSet<String> {
    let mut seen: HashSet<String> = HashSet::from([ty.to_string()]);
    let mut level = vec![ty.to_string()];
    for _ in 0..SUPER_HOPS {
        let mut next = Vec::new();
        for cur in &level {
            for x in t.supertypes(cur) {
                if seen.insert(x.clone()) {
                    next.push(x);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        level = next;
    }
    seen
}

/// Injection points of a supertype of `ty` that receive its bean.
pub fn points_above<'t, T: Types>(t: &'t T, ty: &str) -> Vec<&'t SpringInjection> {
    let mut above: Vec<String> = ancestors(t, ty).into_iter().filter(|x| x != ty).collect();
    above.sort();
    above
        .iter()
        .flat_map(|a| t.spring().injections_of(a))
        .filter(|i| receives(i, &beans_for(t, i)).0.iter().any(|b| b.ty == ty))
        .collect()
}

/// Whether `ty` is a subtype of `of`.
pub fn is_subtype<T: Types>(t: &T, ty: &str, of: &str) -> bool {
    ty != of && ancestors(t, ty).contains(of)
}

/// The beans of type `ty` the application has: its own beans and those of its subtypes
/// (not those declared in a test tree), by label.
pub fn beans_of<'t, T: Types>(t: &'t T, ty: &str) -> Vec<&'t SpringBean> {
    beans_where(t, ty, false)
}

/// The beans an injection point may receive: [`beans_of`] its type, and for one in a test
/// tree the beans the tests declare too.
pub fn beans_for<'t, T: Types>(t: &'t T, inj: &SpringInjection) -> Vec<&'t SpringBean> {
    beans_where(t, &inj.ty, t.spring().tests.contains(&inj.file))
}

fn beans_where<'t, T: Types>(t: &'t T, ty: &str, tests: bool) -> Vec<&'t SpringBean> {
    let sp = t.spring();
    let mut v: Vec<&SpringBean> = if sp.ancestors.len() == sp.beans.len() {
        sp.below
            .get(ty)
            .into_iter()
            .flatten()
            .map(|&i| &sp.beans[i])
            .collect()
    } else {
        // Not linked (a table built by hand).
        sp.beans
            .iter()
            .filter(|b| ancestors(t, &b.ty).contains(ty))
            .collect()
    };
    v.retain(|b| tests || !sp.tests.contains(&b.file));
    v.sort_by_key(|b| (bean_label(b), b.file.clone(), b.line));
    // Two names not known are not one.
    v.dedup_by(|a, b| a.ty == b.ty && a.name == b.name && !a.name.is_empty());
    v
}

/// How Spring chose the beans of an injection point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// Every bean of the type (a collection, a map).
    All,
    /// The beans a `@Qualifier` (or `@Resource(name)`) names.
    Qualifier,
    /// The only bean of the type.
    Only,
    /// The `@Primary` one of several.
    Primary,
    /// The one named like the field or parameter, or as a `@Resource` asks.
    Name,
    /// Several fit and nothing picks one.
    Ambiguous,
    /// Qualified by a name that is not known: any of them.
    Unknown,
    /// A `@Resource` whose name, or the name of a bean that fits, is not known: any of them.
    UnknownName,
    /// A `@Resource` names a bean that is not in the repo.
    NoName,
    /// No bean of the type in the repo.
    None,
}

/// Every name of a bean is known.
fn named(b: &SpringBean) -> bool {
    !b.name.is_empty() && b.aliases.iter().all(|a| !a.is_empty())
}

/// The beans an injection point receives (`all`: the beans of its type), as one list: see
/// [`pick`].
pub fn receives<'b>(inj: &SpringInjection, all: &[&'b SpringBean]) -> (Vec<&'b SpringBean>, Why) {
    pick(inj, all, inj.uses.iter().any(|u| u.each))
}

/// The beans a call on what an injection point receives may be made on: `each`, when it
/// reads every element (an `ObjectProvider` gives one bean, or all of them streamed).
pub fn pick<'b>(
    inj: &SpringInjection,
    all: &[&'b SpringBean],
    each: bool,
) -> (Vec<&'b SpringBean>, Why) {
    let mut v: Vec<&SpringBean> = all.to_vec();
    let mut why = Why::All;
    // `@Resource`: by name first; the default name falls back to the type.
    if let Some(r) = &inj.resource
        && !v.is_empty()
    {
        if r.name.is_empty() || v.iter().any(|b| !named(b)) {
            return (v, Why::UnknownName);
        }
        let hit: Vec<&SpringBean> = v
            .iter()
            .copied()
            .filter(|b| b.name == r.name || b.aliases.contains(&r.name))
            .collect();
        if !hit.is_empty() {
            return (hit, Why::Name);
        }
        if !r.default {
            return (Vec::new(), Why::NoName);
        }
    }
    if let Some(q) = &inj.qualifier {
        if q.is_empty() && !v.is_empty() {
            return (v, Why::Unknown);
        }
        let fits =
            |b: &&SpringBean| b.name == *q || b.aliases.contains(q) || b.qualifiers.contains(q);
        // With no bean of that name, one whose own name or qualifier is not known may be.
        if !v.iter().any(fits) {
            v.retain(|b| !named(b) || b.qualifiers.iter().any(String::is_empty));
            let why = if v.is_empty() {
                Why::None
            } else {
                Why::Unknown
            };
            return (v, why);
        }
        v.retain(fits);
        why = Why::Qualifier;
    }
    if v.is_empty() {
        return (v, Why::None);
    }
    let all_of_them = match inj.wrapper.as_str() {
        "ObjectProvider" => each,
        _ => inj.takes_all(),
    };
    if all_of_them {
        return (v, why);
    }
    if v.len() == 1 {
        return (
            v,
            if why == Why::Qualifier {
                why
            } else {
                Why::Only
            },
        );
    }
    let primary: Vec<&SpringBean> = v.iter().copied().filter(|b| b.primary).collect();
    if primary.len() == 1 {
        return (primary, Why::Primary);
    }
    let like: Vec<&SpringBean> = v
        .iter()
        .copied()
        .filter(|b| b.name == inj.member || b.aliases.contains(&inj.member))
        .collect();
    if like.len() == 1 {
        return (like, Why::Name);
    }
    (v, Why::Ambiguous)
}

/// The id of the type that declares method `id`, as [`SymbolTable::symbol`] knows it: a
/// type of that name in the method's file.
pub fn type_of_method<T: Types>(t: &T, id: &str) -> Option<String> {
    let (path, q) = split_symbol_id(id)?;
    let (ty, _) = q.rsplit_once('.')?;
    let leaf = ty.rsplit('.').next().unwrap_or(ty);
    t.types_in(path)
        .into_iter()
        .find(|(name, _)| name == leaf)
        .map(|(_, id)| id)
}

/// What a bean of class `ty` runs for method `target`: the method its class has or
/// inherits under that name and one of its numbers of parameters, a class's before an
/// interface's default. `None` when it is not known: only a method without a body is
/// found, or one of a class outside the repo.
pub fn method_on<T: Types>(t: &T, ty: &str, target: &str) -> Option<String> {
    let sp = t.spring();
    let m = split_symbol_id(target)?.1.rsplit('.').next()?;
    let asked = sp.method(target);
    let mut level = vec![ty.to_string()];
    let mut seen: HashSet<String> = HashSet::new();
    let mut default: Option<String> = None;
    for _ in 0..=SUPER_HOPS {
        let mut next = Vec::new();
        for cur in &level {
            if !seen.insert(cur.clone()) {
                continue;
            }
            if let Some((p, _)) = split_symbol_id(cur) {
                let id = format!("sym:{p}#{}.{m}", leaf(cur));
                let known = sp.method(&id);
                let runs = t.exists(&id)
                    && known.is_none_or(|x| {
                        id == target
                            || !x.private
                                && !x.is_static
                                && asked.is_none_or(|asked| asked.overlaps(x))
                    });
                let body = known.map_or(!t.is_interface(cur), |x| !x.is_abstract);
                if runs && body {
                    if !t.is_interface(cur) {
                        return Some(id);
                    }
                    default.get_or_insert(id);
                }
            }
            next.extend(t.supertypes(cur));
        }
        if next.is_empty() {
            break;
        }
        level = next;
    }
    default
}

/// The class around a caller id: `sym:p#C` of `sym:p#C.m`; for a call outside any method
/// (`None`, a Java constructor or initializer), the classes of the calling file.
pub fn caller_classes<T: Types>(t: &T, caller: Option<&str>, file: &str) -> Vec<String> {
    if let Some(c) = caller
        && let Some(ty) = type_of_method(t, c)
    {
        return vec![ty];
    }
    t.types_in(file).into_iter().map(|(_, id)| id).collect()
}

/// `List<PriceRule>`, `PaymentGateway`: an injection point's declared type.
pub fn declared(inj: &SpringInjection) -> String {
    let ty = leaf(&inj.ty);
    match inj.wrapper.as_str() {
        "" => ty.to_string(),
        "[]" => format!("{ty}[]"),
        w if cartograph_core::spring::MAPS.contains(&w) => format!("{w}<String, {ty}>"),
        w => format!("{w}<{ty}>"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bean(ty: &str, name: &str, primary: bool) -> SpringBean {
        SpringBean {
            ty: format!("sym:a/{ty}.java#{ty}"),
            name: name.into(),
            aliases: Vec::new(),
            stereotype: "Service".into(),
            file: format!("a/{ty}.java"),
            line: 1,
            factory: None,
            primary,
            qualifiers: Vec::new(),
        }
    }

    fn point(wrapper: &str, member: &str, qualifier: Option<&str>) -> SpringInjection {
        SpringInjection {
            owner: "sym:a/C.java#C".into(),
            how: "field".into(),
            member: member.into(),
            wrapper: wrapper.into(),
            ty: "sym:a/I.java#I".into(),
            qualifier: qualifier.map(str::to_string),
            resource: None,
            file: "a/C.java".into(),
            line: 3,
            uses: Vec::new(),
        }
    }

    #[test]
    fn spring_picks_like_spring() {
        let (a, b, c) = (
            bean("A", "a", false),
            bean("B", "wire", true),
            bean("C", "c", false),
        );
        let all = [&a, &b, &c];
        let names = |v: Vec<&SpringBean>| v.iter().map(|x| x.name.clone()).collect::<Vec<_>>();
        let (v, why) = receives(&point("List", "xs", None), &all);
        assert_eq!(
            (names(v), why),
            (vec!["a".into(), "wire".into(), "c".into()], Why::All)
        );
        let (v, why) = receives(&point("", "x", None), &all);
        assert_eq!((names(v), why), (vec!["wire".to_string()], Why::Primary));
        let (v, why) = receives(&point("", "x", Some("c")), &all);
        assert_eq!((names(v), why), (vec!["c".to_string()], Why::Qualifier));
        let (v, why) = receives(&point("", "c", None), &[&a, &c]);
        assert_eq!((names(v), why), (vec!["c".to_string()], Why::Name));
        let (v, why) = receives(&point("", "x", None), &[&a, &c]);
        assert_eq!((v.len(), why), (2, Why::Ambiguous));
        let (v, why) = receives(&point("", "x", None), &[&a]);
        assert_eq!((v.len(), why), (1, Why::Only));
        let (v, why) = receives(&point("", "x", Some("nope")), &all);
        assert_eq!((v.len(), why), (0, Why::None));
        // A qualifier whose value is not known narrows nothing, not even to the primary.
        let (v, why) = receives(&point("", "x", Some("")), &all);
        assert_eq!((v.len(), why), (3, Why::Unknown));
        // `@Resource`: its name before the primary; only a default name falls back.
        let asks = |name: &str, default| SpringInjection {
            resource: Some(cartograph_core::ResourceName {
                name: name.into(),
                default,
            }),
            ..point("", "x", None)
        };
        let (v, why) = receives(&asks("c", true), &all);
        assert_eq!((names(v), why), (vec!["c".to_string()], Why::Name));
        let (v, why) = receives(&asks("x", true), &all);
        assert_eq!((names(v), why), (vec!["wire".to_string()], Why::Primary));
        let (v, why) = receives(&asks("x", false), &all);
        assert_eq!((v.len(), why), (0, Why::NoName));
        let (v, why) = receives(&asks("", false), &all);
        assert_eq!((v.len(), why), (3, Why::UnknownName));
        // An alias, and a bean whose own name is not known.
        let mut d = bean("D", "", false);
        let (v, why) = receives(&point("", "x", Some("nope")), &[&a, &d]);
        assert_eq!((names(v), why), (vec![String::new()], Why::Unknown));
        d.name = "d".into();
        d.aliases = vec!["nope".into()];
        let (v, why) = receives(&point("", "x", Some("nope")), &[&a, &d]);
        assert_eq!((names(v), why), (vec!["d".to_string()], Why::Qualifier));
    }
}
