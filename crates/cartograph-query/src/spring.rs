//! Spring dependency injection as queries see it (see [`cartograph_core::spring`]): which
//! beans implement a type, which of them an injection point receives (all of them for a
//! list, a set, a map; else the one a qualifier, `@Primary` or the member's name picks),
//! the bean methods a call to an interface method may run, and the collections or tables a
//! method reads or writes.

use crate::symbols::SymbolTable;
use cartograph_core::{DataSink, SpringBean, SpringInjection, Symbols, split_symbol_id};
use std::collections::{HashMap, HashSet};

/// Hops of supertypes walked from a bean's type (as `hierarchy`).
const SUPER_HOPS: usize = 6;

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
    pub fn link(
        t: &SymbolTable,
        in_tests: impl Fn(&str) -> bool,
    ) -> (Vec<HashSet<String>>, HashSet<String>) {
        let ancestors = t.spring.beans.iter().map(|b| ancestors(t, &b.ty)).collect();
        let tests = t
            .spring
            .beans
            .iter()
            .map(|b| &b.file)
            .chain(t.spring.injections.iter().map(|i| &i.file))
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
        Some(_) => format!("{} (@Bean)", b.name),
        None => leaf(&b.ty).to_string(),
    }
}

/// Type `ty` and its supertypes up to [`SUPER_HOPS`] hops.
fn ancestors(t: &SymbolTable, ty: &str) -> HashSet<String> {
    let mut seen: HashSet<String> = HashSet::from([ty.to_string()]);
    let mut level = vec![ty.to_string()];
    for _ in 0..SUPER_HOPS {
        let mut next = Vec::new();
        for cur in &level {
            for e in t.supers_of(cur) {
                if let Some(x) = &e.target
                    && seen.insert(x.clone())
                {
                    next.push(x.clone());
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

/// The beans of type `ty` the application has: its own beans and those of its subtypes
/// (not those declared in a test tree), by label.
pub fn beans_of<'t>(t: &'t SymbolTable, ty: &str) -> Vec<&'t SpringBean> {
    beans_where(t, ty, false)
}

/// The beans an injection point may receive: [`beans_of`] its type, and for one in a test
/// tree the beans the tests declare too.
pub fn beans_for<'t>(t: &'t SymbolTable, inj: &SpringInjection) -> Vec<&'t SpringBean> {
    beans_where(t, &inj.ty, t.spring.tests.contains(&inj.file))
}

fn beans_where<'t>(t: &'t SymbolTable, ty: &str, tests: bool) -> Vec<&'t SpringBean> {
    let sp = &t.spring;
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
    /// No bean of the type in the repo.
    None,
}

/// The beans an injection point receives (`all`: the beans of its type).
pub fn pick<'b>(inj: &SpringInjection, all: &[&'b SpringBean]) -> (Vec<&'b SpringBean>, Why) {
    let mut v: Vec<&SpringBean> = all.to_vec();
    let mut why = Why::All;
    if let Some(q) = &inj.qualifier {
        v.retain(|b| &b.name == q || b.qualifiers.contains(q));
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
pub fn type_of_method(t: &SymbolTable, id: &str) -> Option<String> {
    let (path, q) = split_symbol_id(id)?;
    let (ty, _) = q.rsplit_once('.')?;
    let leaf = ty.rsplit('.').next().unwrap_or(ty);
    t.of_file(path)
        .iter()
        .map(|&i| &t.symbols[i])
        .find(|s| s.kind.is_type() && s.name == leaf)
        .map(|s| s.id())
}

/// Method `m` as type `ty` has it: its own, else the nearest supertype's (not `skip`).
fn method_on(t: &SymbolTable, ty: &str, m: &str, skip: &str) -> Option<String> {
    let mut level = vec![ty.to_string()];
    let mut seen: HashSet<String> = HashSet::new();
    for _ in 0..=SUPER_HOPS {
        let mut next = Vec::new();
        for cur in &level {
            if !seen.insert(cur.clone()) {
                continue;
            }
            if let Some((p, _)) = split_symbol_id(cur) {
                let id = format!("sym:{p}#{}.{m}", leaf(cur));
                if id != skip && t.symbol(&id).is_some() {
                    return Some(id);
                }
            }
            next.extend(t.supers_of(cur).filter_map(|e| e.target.clone()));
        }
        if next.is_empty() {
            break;
        }
        level = next;
    }
    None
}

/// What a call to method `id` (an interface or base class method) may run through Spring:
/// for each bean of its type, the method that bean has, with the bean.
pub fn dispatch<'t>(t: &'t SymbolTable, id: &str) -> Vec<(String, &'t SpringBean)> {
    let Some(ty) = type_of_method(t, id) else {
        return Vec::new();
    };
    let Some((_, q)) = split_symbol_id(id) else {
        return Vec::new();
    };
    let m = q.rsplit('.').next().unwrap_or(q);
    let mut out: Vec<(String, &SpringBean)> = beans_of(t, &ty)
        .into_iter()
        .filter(|b| b.ty != ty)
        .filter_map(|b| method_on(t, &b.ty, m, id).map(|x| (x, b)))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// The beans a call at `file:line` (from `caller`, a method id or `None` outside any
/// method) to a method of type `ty` may reach, when the calling class has injection points
/// of `ty`: the one whose field or parameter the call is made on, else all of them
/// together. `None` without one (the receiver is not injected there, or not known).
pub fn reaching<'t>(
    t: &'t SymbolTable,
    caller: Option<&str>,
    file: &str,
    line: u32,
    ty: &str,
) -> Option<Vec<&'t SpringBean>> {
    let points: Vec<&SpringInjection> = caller_classes(t, caller, file)
        .iter()
        .flat_map(|c| t.spring.injections_into(c))
        .filter(|i| i.ty == ty)
        .collect();
    if points.is_empty() {
        return None;
    }
    let on_line: Vec<&SpringInjection> = points
        .iter()
        .copied()
        .filter(|i| i.uses.binary_search(&line).is_ok())
        .collect();
    let points = if on_line.is_empty() { points } else { on_line };
    let mut out: Vec<&SpringBean> = Vec::new();
    for p in points {
        for b in pick(p, &beans_for(t, p)).0 {
            if !out.iter().any(|x| std::ptr::eq(*x, b)) {
                out.push(b);
            }
        }
    }
    Some(out)
}

/// The class around a caller id: `sym:p#C` of `sym:p#C.m`; for a call outside any method
/// (`None`, a Java constructor or initializer), the classes of the calling file.
pub fn caller_classes(t: &SymbolTable, caller: Option<&str>, file: &str) -> Vec<String> {
    if let Some(c) = caller
        && let Some(ty) = type_of_method(t, c)
    {
        return vec![ty];
    }
    t.of_file(file)
        .iter()
        .map(|&i| &t.symbols[i])
        .filter(|s| s.kind.is_type())
        .map(|s| s.id())
        .collect()
}

/// `List<PriceRule>`, `PaymentGateway`: an injection point's declared type.
pub fn declared(inj: &SpringInjection) -> String {
    let ty = leaf(&inj.ty);
    match inj.wrapper.as_str() {
        "" => ty.to_string(),
        "[]" => format!("{ty}[]"),
        "Map" | "HashMap" | "LinkedHashMap" | "TreeMap" | "SortedMap" => {
            format!("{}<String, {ty}>", inj.wrapper)
        }
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
    }
}
