//! What a Java call runs when Spring chooses the object: one rule, shared by flows, slices
//! and `callers`.
//!
//! A call names the method its receiver's type has or inherits ([`SymbolCall::target`]).
//! What runs is that method on the object the receiver holds:
//! - `super.m()`, a private and a static method run as they are;
//! - on what Spring injects (a field, a parameter, the variable of a loop over them): the
//!   beans that injection point receives;
//! - else every bean below the receiver's type: the type a variable is declared with, the
//!   calling class for a call on itself, else the type that declares the method.
//!
//! Each of those beans runs the method its class has or inherits, overloads told apart by
//! their number of parameters, a class's before an interface's default. The method named
//! may run too when some bean inherits it, or when it has a body and the receiver is not
//! known to be a bean. With no bean, or none that runs another method, the call is what it
//! names.
//!
//! Types no bean is below are discarded first, by one lookup; what each type's beans and
//! ancestors are, and what a bean runs for a method, is worked out once per
//! [`Dispatcher`].

use crate::spring::{SpringTable, Types, beans_for, is_subtype, pick};
use cartograph_core::{SpringBean, SymbolCall};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// What a call may run.
#[derive(Debug, Clone)]
pub struct Reach<'t> {
    /// Methods beans run, other than the one named, with those beans.
    pub runs: Vec<(String, Vec<&'t SpringBean>)>,
    /// The method named may run too.
    pub named: bool,
}

impl Reach<'_> {
    /// The methods to go on to: `(id, through Spring)`.
    pub fn edges<'s>(&'s self, target: &'s str) -> impl Iterator<Item = (&'s str, bool)> {
        self.runs
            .iter()
            .map(|(m, _)| (m.as_str(), true))
            .chain(self.named.then_some((target, false)))
    }
}

/// What a bean runs for a method: `None` when its class is not known (it is only known by
/// the type that declares the method), or the method comes from outside the repo.
type Ran = Option<String>;

/// What a call may run through Spring, shared.
pub type Reached<'t> = Option<Rc<Reach<'t>>>;

/// Decides what calls reach, remembering what it works out.
pub struct Dispatcher<'t, T: Types> {
    t: &'t T,
    /// `(bean type, method)` → what it runs.
    ran: RefCell<HashMap<(String, String), Ran>>,
    /// `(type, other)` → whether the first is below the second.
    below: RefCell<HashMap<(String, String), bool>>,
    /// `(method, receiver types)` → what runs, for receivers Spring does not inject.
    open: RefCell<HashMap<(String, String), Reached<'t>>>,
}

impl<'t, T: Types> Dispatcher<'t, T> {
    pub fn new(t: &'t T) -> Dispatcher<'t, T> {
        Dispatcher {
            t,
            ran: RefCell::default(),
            below: RefCell::default(),
            open: RefCell::default(),
        }
    }

    fn spring(&self) -> &'t SpringTable {
        self.t.spring()
    }

    /// Type `a` is `b` or below it.
    fn is(&self, a: &str, b: &str) -> bool {
        if a == b {
            return true;
        }
        let key = (a.to_string(), b.to_string());
        if let Some(v) = self.below.borrow().get(&key) {
            return *v;
        }
        let v = is_subtype(self.t, a, b);
        self.below.borrow_mut().insert(key, v);
        v
    }

    /// The call names a method that runs as it is: `super.m()`, a private or static one.
    pub fn as_named(&self, c: &SymbolCall) -> bool {
        let sp = self.spring();
        sp.method(&c.target)
            .is_some_and(|m| m.private || m.is_static)
            || sp.sites(c).is_some_and(|s| s.iter().all(|x| x.sup))
    }

    /// The types a call's receiver may have, below `ty` (its method's): those known, else
    /// `ty` itself.
    pub fn receivers(&self, c: &SymbolCall, ty: &str) -> Vec<String> {
        let mut out: Vec<String> = match self.spring().sites(c) {
            None => vec![ty.to_string()],
            Some(sites) => sites
                .iter()
                .filter(|s| !s.sup)
                .map(|s| match &s.recv {
                    Some(r) if self.is(r, ty) => r.clone(),
                    _ => ty.to_string(),
                })
                .collect(),
        };
        out.sort();
        out.dedup();
        out
    }

    /// The beans Spring gives the receiver of a call to a method of type `ty`, when the
    /// call is made on what it injects; `None` otherwise.
    pub fn injected(&self, c: &SymbolCall, ty: &str) -> Option<Vec<&'t SpringBean>> {
        let sp = self.spring();
        let mut out: Vec<&SpringBean> = Vec::new();
        let mut any = false;
        for (p, each) in sp.injected_at(&c.file, c.line) {
            if !self.is(&p.ty, ty) {
                continue;
            }
            any = true;
            let all = beans_for(self.t, p);
            for b in pick(p, &all, each).0 {
                if !out.iter().any(|x| std::ptr::eq(*x, b)) {
                    out.push(b);
                }
            }
        }
        any.then_some(out)
    }

    /// What bean `b` runs for method `target`.
    pub fn ran(&self, b: &SpringBean, target: &str) -> Ran {
        let key = (b.ty.clone(), target.to_string());
        if let Some(v) = self.ran.borrow().get(&key) {
            return v.clone();
        }
        let v = crate::spring::method_on(self.t, &b.ty, target);
        self.ran.borrow_mut().insert(key, v.clone());
        v
    }

    /// Some bean of the application runs method `id` for a call to `target`.
    pub fn of_a_bean(&self, target: &str, id: &str) -> bool {
        self.spring().declarer(target).is_some_and(|ty| {
            self.below_type(ty)
                .any(|b| self.ran(b, target).as_deref() == Some(id))
        })
    }

    /// The application's beans of type `ty` and below.
    fn below_type(&self, ty: &str) -> impl Iterator<Item = &'t SpringBean> {
        self.spring().beans_below(ty)
    }

    /// What a call may run through Spring; `None` for a call that runs what it names.
    pub fn reach(&self, c: &SymbolCall) -> Reached<'t> {
        // Most calls stop here: no bean is of the type that declares their method.
        let ty = self.spring().declarer(&c.target)?;
        if self.as_named(c) {
            return None;
        }
        if let Some(beans) = self.injected(c, ty) {
            return self.group(&c.target, beans.into_iter(), false).map(Rc::new);
        }
        let recv = self.receivers(c, ty);
        let key = (c.target.clone(), recv.join("\n"));
        if let Some(v) = self.open.borrow().get(&key) {
            return v.clone();
        }
        let mut beans: Vec<&SpringBean> = Vec::new();
        for r in &recv {
            for b in self.below_type(r) {
                if !beans.iter().any(|x| std::ptr::eq(*x, b)) {
                    beans.push(b);
                }
            }
        }
        let v = self.group(&c.target, beans.into_iter(), true).map(Rc::new);
        self.open.borrow_mut().insert(key, v.clone());
        v
    }

    /// What a method runs on any of the beans below its type, for one that has no body of
    /// its own (where a flow starts at it).
    pub fn declared(&self, target: &str) -> Option<Reach<'t>> {
        let ty = self.spring().declarer(target)?;
        let m = self.spring().method(target)?;
        if !m.is_abstract {
            return None;
        }
        self.group(target, self.below_type(ty), true)
    }

    /// What `beans` run for `target`; `open`: other objects may be there too.
    fn group(
        &self,
        target: &str,
        beans: impl Iterator<Item = &'t SpringBean>,
        open: bool,
    ) -> Option<Reach<'t>> {
        let has_body = self.spring().method(target).is_some_and(|m| !m.is_abstract);
        let mut named = open && has_body;
        let mut runs: Vec<(String, Vec<&SpringBean>)> = Vec::new();
        for b in beans {
            match self.ran(b, target) {
                Some(m) if m != target => match runs.iter_mut().find(|(x, _)| *x == m) {
                    Some((_, v)) => v.push(b),
                    None => runs.push((m, vec![b])),
                },
                // It inherits the method named, or its class is not known.
                _ => named = true,
            }
        }
        if runs.is_empty() {
            return None;
        }
        runs.sort_by(|a, b| a.0.cmp(&b.0));
        Some(Reach { runs, named })
    }
}
