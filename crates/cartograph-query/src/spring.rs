//! Spring dependency injection as queries see it (see [`cartograph_core::spring`]): which
//! beans implement a type, which of them an injection point receives (all of them for a
//! list, a set, a map; else the one a qualifier, `@Primary` or the member's name picks),
//! the bean methods a call to an interface method may run, and the collections or tables a
//! method reads or writes.

use cartograph_core::{DataSink, SpringBean, SpringInjection, Symbols, split_symbol_id};
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
    /// Files of beans and injection points that lie in a test, fixture or example tree:
    /// their beans are injected only into the injection points of such files.
    tests: HashSet<String>,
}

impl SpringTable {
    pub fn build(s: &Symbols) -> SpringTable {
        let mut t = SpringTable {
            beans: s.beans.clone(),
            injections: s.injections.clone(),
            sinks: s.sinks.clone(),
            ..SpringTable::default()
        };
        for (i, x) in t.injections.iter().enumerate() {
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
    let mut v: Vec<&SpringBean> = sp
        .beans
        .iter()
        .enumerate()
        .filter(|(_, b)| tests || !sp.tests.contains(&b.file))
        .filter(|(i, b)| {
            b.ty == ty
                || match sp.ancestors.get(*i) {
                    Some(a) => a.contains(ty),
                    None => ancestors(t, &b.ty).contains(ty),
                }
        })
        .map(|(_, b)| b)
        .collect();
    v.sort_by_key(|b| (bean_label(b), b.file.clone(), b.line));
    v.dedup_by(|a, b| a.ty == b.ty && a.name == b.name);
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
    /// The one named like the field or parameter.
    Name,
    /// Several fit and nothing picks one.
    Ambiguous,
    /// Qualified by a name that is not known: any of them.
    Unknown,
    /// No bean of the type in the repo.
    None,
}

/// The beans an injection point receives (`all`: the beans of its type).
pub fn pick<'b>(inj: &SpringInjection, all: &[&'b SpringBean]) -> (Vec<&'b SpringBean>, Why) {
    let mut v: Vec<&SpringBean> = all.to_vec();
    let mut why = Why::All;
    if let Some(q) = &inj.qualifier {
        if q.is_empty() && !v.is_empty() {
            return (v, Why::Unknown);
        }
        let fits =
            |b: &&SpringBean| b.name == *q || b.aliases.contains(q) || b.qualifiers.contains(q);
        // With no bean of that name, one whose own name or qualifier is not known may be.
        if !v.iter().any(fits) {
            v.retain(|b| b.name.is_empty() || b.qualifiers.iter().any(String::is_empty));
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
    if inj.takes_all() {
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
    let named: Vec<&SpringBean> = v.iter().copied().filter(|b| b.name == inj.member).collect();
    if named.len() == 1 {
        return (named, Why::Name);
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

/// Method `m` as type `ty` has it: its own, else the nearest supertype's, a class before
/// an interface (a class's method wins over an interface's default).
fn method_on<T: Types>(t: &T, ty: &str, m: &str) -> Option<String> {
    let mut level = vec![ty.to_string()];
    let mut seen: HashSet<String> = HashSet::new();
    let mut declared: Option<String> = None;
    for _ in 0..=SUPER_HOPS {
        let mut next = Vec::new();
        for cur in &level {
            if !seen.insert(cur.clone()) {
                continue;
            }
            if let Some((p, _)) = split_symbol_id(cur) {
                let id = format!("sym:{p}#{}.{m}", leaf(cur));
                if t.exists(&id) {
                    if !t.is_interface(cur) {
                        return Some(id);
                    }
                    declared.get_or_insert(id);
                }
            }
            next.extend(t.supertypes(cur));
        }
        if next.is_empty() {
            break;
        }
        level = next;
    }
    declared
}

/// What a call to method `decl` (declared by type `decl_ty`) runs on bean `b`: the method
/// its class has, or inherits. `None` when that is not known: the bean is known only by
/// the declaring type (a `@Bean` method that returns the interface, a generated mapper),
/// or the method is only declared (a default method, or one from outside the repo).
pub fn runs<T: Types>(t: &T, b: &SpringBean, decl_ty: &str, decl: &str) -> Option<String> {
    if b.ty == decl_ty {
        return None;
    }
    let m = split_symbol_id(decl)?.1.rsplit('.').next()?;
    method_on(t, &b.ty, m).filter(|x| type_of_method(t, x).is_none_or(|ty| !t.is_interface(&ty)))
}

/// A method, with the beans that run it.
pub type Runs<'t> = (String, Vec<&'t SpringBean>);

/// What a call to method `id` (an interface or base class method) may run through Spring:
/// each method the beans of its type have or inherit (`id` itself among them), with the
/// beans that run it. Empty when no bean has one of its own.
pub fn dispatch<'t, T: Types>(t: &'t T, id: &str) -> Vec<Runs<'t>> {
    let Some(ty) = type_of_method(t, id) else {
        return Vec::new();
    };
    let Some((_, q)) = split_symbol_id(id) else {
        return Vec::new();
    };
    let m = q.rsplit('.').next().unwrap_or(q);
    let mut out: Vec<Runs<'t>> = Vec::new();
    for b in beans_of(t, &ty) {
        // A bean known by the declaring type may run the declared method.
        let Some(x) = method_on(t, &b.ty, m) else {
            continue;
        };
        match out.iter_mut().find(|(y, _)| *y == x) {
            Some((_, beans)) => beans.push(b),
            None => out.push((x, vec![b])),
        }
    }
    if out.iter().all(|(x, _)| x == id) {
        return Vec::new();
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The beans a call at `line` (from `caller`, a method id or `None` outside any method) to
/// a method of type `ty` may reach, when it is made on what Spring injects into the calling
/// class: the field or parameter itself, a field that holds it, the variable of a loop or
/// lambda over it. `None` otherwise (another receiver, or one not known).
pub fn reaching<'t, T: Types>(
    t: &'t T,
    caller: Option<&str>,
    file: &str,
    line: u32,
    ty: &str,
) -> Option<Vec<&'t SpringBean>> {
    let mut out: Vec<&SpringBean> = Vec::new();
    let mut injected = false;
    for c in caller_classes(t, caller, file) {
        for p in t.spring().injections_into(&c).filter(|i| i.ty == ty) {
            // Of several uses on the line, one that reads every element narrows least.
            let Some(u) = p.uses.iter().filter(|u| u.line == line).max() else {
                continue;
            };
            injected = true;
            let all = beans_for(t, p);
            // A provider gives one bean, or all of them when it is read one by one.
            let got = if p.wrapper == "ObjectProvider" {
                let one = SpringInjection {
                    uses: vec![*u],
                    ..p.clone()
                };
                pick(&one, &all).0
            } else {
                pick(p, &all).0
            };
            for b in got {
                if !out.iter().any(|x| std::ptr::eq(*x, b)) {
                    out.push(b);
                }
            }
        }
    }
    injected.then_some(out)
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
        let (v, why) = pick(&point("List", "xs", None), &all);
        assert_eq!(
            (names(v), why),
            (vec!["a".into(), "wire".into(), "c".into()], Why::All)
        );
        let (v, why) = pick(&point("", "x", None), &all);
        assert_eq!((names(v), why), (vec!["wire".to_string()], Why::Primary));
        let (v, why) = pick(&point("", "x", Some("c")), &all);
        assert_eq!((names(v), why), (vec!["c".to_string()], Why::Qualifier));
        let (v, why) = pick(&point("", "c", None), &[&a, &c]);
        assert_eq!((names(v), why), (vec!["c".to_string()], Why::Name));
        let (v, why) = pick(&point("", "x", None), &[&a, &c]);
        assert_eq!((v.len(), why), (2, Why::Ambiguous));
        let (v, why) = pick(&point("", "x", None), &[&a]);
        assert_eq!((v.len(), why), (1, Why::Only));
        let (v, why) = pick(&point("", "x", Some("nope")), &all);
        assert_eq!((v.len(), why), (0, Why::None));
        // A qualifier whose value is not known narrows nothing, not even to the primary.
        let (v, why) = pick(&point("", "x", Some("")), &all);
        assert_eq!((v.len(), why), (3, Why::Unknown));
        // An alias, and a bean whose own name is not known.
        let mut d = bean("D", "", false);
        let (v, why) = pick(&point("", "x", Some("nope")), &[&a, &d]);
        assert_eq!((names(v), why), (vec![String::new()], Why::Unknown));
        d.name = "d".into();
        d.aliases = vec!["nope".into()];
        let (v, why) = pick(&point("", "x", Some("nope")), &[&a, &d]);
        assert_eq!((names(v), why), (vec!["d".to_string()], Why::Qualifier));
    }
}
