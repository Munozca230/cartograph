//! What a Java call may run when Spring makes the objects: one rule, shared by flows,
//! slices and `callers`. What Spring chooses is annotated, never subtracted.
//!
//! A call names the method its receiver's type has or inherits ([`SymbolCall::target`]).
//! `super.m()`, a private and a static method run as they are. Any other may run:
//! - the method named, kept whenever it has a body (and where Spring's choice is not known);
//! - what the beans of the receiver's type and below have or inherit under that name: first
//!   those Spring injects where the call is made, then the others.
//!
//! Types no bean is below are discarded by one lookup; what a bean runs for a method and
//! what is below a type are worked out once per [`Dispatcher`].

use crate::spring::{SpringTable, Types, Why, beans_for, is_subtype, method_on, pick};
use cartograph_core::{SpringBean, SymbolCall};
use std::cell::RefCell;
use std::collections::HashMap;

/// How a call reaches a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// It is the one named.
    Named,
    /// A bean runs it: one Spring injects there, or any when that is not known.
    Bean,
    /// A bean runs it, and Spring injects others there.
    OtherBean,
}

/// Decides what calls reach, remembering what it works out.
pub struct Dispatcher<'t, T: Types> {
    t: &'t T,
    /// `(bean type, method)` → what the bean runs for it, when known.
    ran: RefCell<HashMap<(String, String), Option<String>>>,
    /// `(type, other)` → whether the first is the other or below it.
    below: RefCell<HashMap<(String, String), bool>>,
}

impl<'t, T: Types> Dispatcher<'t, T> {
    pub fn new(t: &'t T) -> Dispatcher<'t, T> {
        Dispatcher {
            t,
            ran: RefCell::default(),
            below: RefCell::default(),
        }
    }

    fn spring(&self) -> &'t SpringTable {
        self.t.spring()
    }

    fn is(&self, a: &str, b: &str) -> bool {
        let key = (a.to_string(), b.to_string());
        if let Some(v) = self.below.borrow().get(&key) {
            return *v;
        }
        let v = a == b || is_subtype(self.t, a, b);
        self.below.borrow_mut().insert(key, v);
        v
    }

    /// The call runs the method it names: `super.m()`, a private or a static one.
    pub fn as_named(&self, c: &SymbolCall) -> bool {
        let sp = self.spring();
        sp.method(&c.target)
            .is_some_and(|m| m.private || m.is_static)
            || sp.sites(c).is_some_and(|s| s.iter().all(|x| x.sup))
    }

    /// The beans Spring gives what a call to a method of type `ty` is made on, and how it
    /// chose them; `None` when the call is not made on what it injects.
    pub fn injected(&self, c: &SymbolCall, ty: &str) -> Option<(Vec<&'t SpringBean>, Why)> {
        let mut out: Option<(Vec<&SpringBean>, Why)> = None;
        for (p, each) in self.spring().injected_at(&c.file, c.line) {
            if self.is(&p.ty, ty) {
                let (beans, why) = pick(p, &beans_for(self.t, p), each);
                out.get_or_insert((Vec::new(), why)).0.extend(beans);
            }
        }
        out
    }

    /// What bean `b` runs for method `target`; `None` when its class is not known, or the
    /// method comes from outside the repo.
    pub fn ran(&self, b: &SpringBean, target: &str) -> Option<String> {
        let key = (b.ty.clone(), target.to_string());
        if let Some(v) = self.ran.borrow().get(&key) {
            return v.clone();
        }
        let v = method_on(self.t, &b.ty, target);
        self.ran.borrow_mut().insert(key, v.clone());
        v
    }

    /// The methods a call goes on to, in order; empty for a call that only runs the one it
    /// names.
    pub fn reach(&self, c: &SymbolCall) -> Vec<(String, Via)> {
        // Most calls stop here: no bean is of the type that declares their method.
        let Some(ty) = self.spring().declarer(&c.target) else {
            return Vec::new();
        };
        if self.as_named(c) {
            return Vec::new();
        }
        let mut beans: Vec<&SpringBean> = Vec::new();
        match self.spring().sites(c) {
            None => beans.extend(self.spring().beans_below(ty)),
            Some(sites) => {
                for s in sites.iter().filter(|s| !s.sup) {
                    let of = s.recv.as_deref().filter(|r| self.is(r, ty)).unwrap_or(ty);
                    beans.extend(self.spring().beans_below(of));
                }
            }
        }
        let chosen = self.injected(c, ty).map(|(beans, _)| beans);
        self.order(&c.target, beans, chosen)
    }

    /// What the beans below a method without a body run, for a flow or slice that gets to
    /// it by other means than a call.
    pub fn declared(&self, target: &str) -> Vec<(String, Via)> {
        let sp = self.spring();
        match (sp.declarer(target), sp.method(target)) {
            (Some(ty), Some(m)) if m.is_abstract => {
                self.order(target, sp.beans_below(ty).collect(), None)
            }
            _ => Vec::new(),
        }
    }

    fn order(
        &self,
        target: &str,
        beans: Vec<&'t SpringBean>,
        chosen: Option<Vec<&'t SpringBean>>,
    ) -> Vec<(String, Via)> {
        // The method named stays with a body, and where what runs is not known.
        let mut named = self.spring().method(target).is_none_or(|m| !m.is_abstract)
            || chosen.as_ref().is_some_and(Vec::is_empty);
        let rest = chosen.as_ref().map_or(Via::Bean, |_| Via::OtherBean);
        let all = chosen
            .unwrap_or_default()
            .into_iter()
            .map(|b| (b, Via::Bean))
            .chain(beans.into_iter().map(|b| (b, rest)));
        let mut out: Vec<(String, Via)> = Vec::new();
        for (b, via) in all {
            match self.ran(b, target) {
                Some(m) if m != target => {
                    if !out.iter().any(|(x, _)| *x == m) {
                        out.push((m, via));
                    }
                }
                // It inherits the method named, or its class is not known.
                _ => named |= via == Via::Bean,
            }
        }
        if named && !out.is_empty() {
            let at = out.iter().take_while(|(_, v)| *v == Via::Bean).count();
            out.insert(at, (target.to_string(), Via::Named));
        }
        out
    }
}
