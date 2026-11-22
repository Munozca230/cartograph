//! N2: symbol definitions, call sites, import bindings and flow entry points.
//!
//! Extraction runs on the tree the import extractor already parsed (one parse per file),
//! with one tree-sitter query per language (`queries/*.scm`, capture names documented at
//! the top of `javascript.scm`). The Rust side reads a few captures and applies the
//! language's rules for containers, visibility, receivers and import bindings
//! ([`extract`]). What comes out ([`Code`]) is cached with the raw imports.
//!
//! Resolution ([`resolve`]) never runs from the cache: it needs the whole repo (import
//! sites, every definition) and runs after import resolution on every index.

mod extract;
pub mod resolve;

pub use extract::extract;

use crate::grammars::Grammar;
use cartograph_core::{EntryKind, SymbolKind};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tree_sitter::Query;

/// A Rust call's path (`krate::m` of `krate::m::f()`) as `(file, inline-module scope,
/// path)`, to resolve like a `use` of it.
pub type RustCallPath = (String, Vec<String>, Vec<String>);

/// The file each Rust call path leads to, as import resolution found it (see
/// [`crate::resolve::rust::Resolver::path_file`]); paths that lead to no repo file are left
/// out. An input of [`resolve::resolve`].
pub type RustCallPaths = std::collections::HashMap<RustCallPath, String>;

/// Everything N2 needs from one file, before resolution. Field names are short: this is
/// stored per file in the extraction cache.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Code {
    #[serde(rename = "d", default, skip_serializing_if = "Vec::is_empty")]
    pub defs: Vec<Def>,
    #[serde(rename = "c", default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<Call>,
    #[serde(rename = "b", default, skip_serializing_if = "Vec::is_empty")]
    pub bindings: Vec<Binding>,
    #[serde(rename = "e", default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<RawEntry>,
    /// Name of the definition a JS/TS `export default` names.
    #[serde(rename = "x", default, skip_serializing_if = "Option::is_none")]
    pub default_export: Option<String>,
    /// The file is an ES module (has `import` or `export`): an unqualified name comes from
    /// the file itself or an import, never from another file's globals.
    #[serde(rename = "m", default, skip_serializing_if = "std::ops::Not::not")]
    pub module: bool,
    /// Go: the file's package is `main`.
    #[serde(rename = "p", default, skip_serializing_if = "std::ops::Not::not")]
    pub package_main: bool,
    /// Python: the names of an explicit literal `__all__` (sorted), the only names a star
    /// import of the module brings, imported ones included. `None` without one, or when it
    /// is built in a way the extractor cannot read.
    #[serde(rename = "a", default, skip_serializing_if = "Option::is_none")]
    pub all: Option<Vec<String>>,
    /// Declared types of fields (Rust struct fields, TS/Java/C# class fields), for
    /// `self.field.m()` calls.
    #[serde(rename = "t", default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldType>,
    /// Rust `impl Trait for Type` blocks: `Type` has `Trait` as a supertype. Kept per file
    /// because the type is often defined in another one.
    #[serde(rename = "i", default, skip_serializing_if = "Vec::is_empty")]
    pub impls: Vec<TraitImpl>,
    /// GDScript: what the script itself extends (`extends Node`, `extends "res://a.gd"`, the
    /// quotes kept). The file is the class: its functions have no container.
    #[serde(rename = "g", default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<ScriptExtends>,
}

/// A supertype of a class, struct, interface or trait, as written in its definition (generic
/// arguments removed): a base class, an implemented or extended interface, a supertrait, a
/// Go embedded type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Super {
    /// `Base`, `ns.Base`, `a::b::Trait`; anything that is not a name (`mixin(Base)`) is kept
    /// as written and never resolves.
    #[serde(rename = "n")]
    pub name: String,
    /// It cannot hide a method of a later supertype: an implemented interface, a trait, an
    /// embedded Go interface, or a base known to add no methods (Python `object`, `ABC`,
    /// `Generic`, `Protocol`). When it is not a type of the repo, the walk over supertypes
    /// goes past it; past any other supertype of no repo type it stops (the method may come
    /// from there).
    #[serde(rename = "i", default, skip_serializing_if = "std::ops::Not::not")]
    pub iface: bool,
}

/// Rust `impl Trait for Type` (both as written, generic arguments removed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraitImpl {
    #[serde(rename = "t")]
    pub ty: String,
    #[serde(rename = "r")]
    pub tr: String,
    #[serde(rename = "l")]
    pub line: u32,
}

/// GDScript `extends` of the script itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptExtends {
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "l")]
    pub line: u32,
}

/// A field whose declared type is one named type: `guard: Guard`, `guard: Arc<Guard>`,
/// `&'a Guard`, `private guard: Guard`. Wrappers a method call sees through (`&`, `Box`,
/// `Arc`, `Rc`) are removed; anything else (`Option<T>`, `Vec<T>`, a path, a union) is
/// left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldType {
    /// The struct or class that declares the field.
    #[serde(rename = "c")]
    pub container: String,
    #[serde(rename = "n")]
    pub name: String,
    /// The type's name as written (`Guard`).
    #[serde(rename = "t")]
    pub ty: String,
}

/// A definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Def {
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "k")]
    pub kind: SymbolKind,
    #[serde(rename = "c", default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// Line of the name.
    #[serde(rename = "l")]
    pub line: u32,
    /// First line of the definition (decorators and annotations included).
    #[serde(rename = "s")]
    pub start: u32,
    #[serde(rename = "e")]
    pub end: u32,
    #[serde(rename = "x", default, skip_serializing_if = "std::ops::Not::not")]
    pub exported: bool,
    /// Go: the method's receiver variable (`s` in `func (s *Server) Run()`), a `self`.
    #[serde(rename = "r", default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
    /// The method implements or overrides an interface or trait method (Rust
    /// `impl Trait for`, Java `@Override`, C# `override`): library types share its name,
    /// so `obj.m()` is never matched to it by name alone.
    #[serde(rename = "o", default, skip_serializing_if = "std::ops::Not::not")]
    pub overrides: bool,
    /// A class that may declare methods it does not define (a TS `abstract` class, or an
    /// ambient one: `declare class`): a subclass defines them, as for an interface.
    #[serde(rename = "a", default, skip_serializing_if = "std::ops::Not::not")]
    pub declares: bool,
    /// Supertypes of a class, struct, interface or trait, in declared order (`extends`
    /// before `implements`). Rust trait impls and a GDScript script's own `extends` are on
    /// [`Code`].
    #[serde(rename = "u", default, skip_serializing_if = "Vec::is_empty")]
    pub supers: Vec<Super>,
    /// A GDScript `class_name` statement: the class is the whole script.
    #[serde(rename = "g", default, skip_serializing_if = "std::ops::Not::not")]
    pub class_name: bool,
    /// A member of a JS object literal (`api.start` of `const api = {…}`). Reached through
    /// the object (`api.start()`, `this.start()`, an import of `api`), never by its name
    /// alone: a preload bridge repeats the names of the services it wraps, and would make
    /// their `svc.m()` calls ambiguous.
    #[serde(rename = "j", default, skip_serializing_if = "std::ops::Not::not")]
    pub object: bool,
}

impl Def {
    /// `Container.name` or `name`.
    pub fn qualified(&self) -> String {
        match &self.container {
            Some(c) => format!("{c}.{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// How a call names what it calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Form {
    /// `f()`, `<Comp/>`.
    #[serde(rename = "p")]
    Plain,
    /// `this.f()`, `self.f()`, `Self::f()`.
    #[serde(rename = "s")]
    SelfCall,
    /// `x.f()`; `qualifier` is `x` when it is a plain identifier.
    #[serde(rename = "m")]
    Member,
    /// `Type::f()` / `module::f()` (Rust, C++).
    #[serde(rename = "q")]
    Path,
    /// `new T()`.
    #[serde(rename = "n")]
    New,
    /// `super.f()`, `super().f()`, `base.F()`: a method of the caller's supertypes.
    /// `qualifier` is the class Python's `super(Class, self)` names.
    #[serde(rename = "u")]
    Super,
}

/// A call site (or a reference resolved like one, such as a route handler).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    #[serde(rename = "l")]
    pub line: u32,
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "f")]
    pub form: Form,
    #[serde(rename = "q", default, skip_serializing_if = "Option::is_none")]
    pub qualifier: Option<String>,
    /// Rust `a::b::f()`: the whole path before the name (`["a", "b"]`; `crate`, `super`,
    /// `self` and an empty first segment for a leading `::` kept), when every segment is a
    /// plain identifier. Resolved like a `use` of it ([`RustCallPaths`]).
    #[serde(rename = "p", default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
    /// Rust: the inline modules around the call (`mod tests { … }`), outermost first: what
    /// `super` and `self` in `path` are relative to.
    #[serde(rename = "m", default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    /// `self.field.f()` / `this.field.f()` ([`Form::Member`]): the field, resolved through
    /// its declared type ([`Code::fields`]).
    #[serde(rename = "v", default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

/// What an import binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BindKind {
    /// `import { a as b }`, `from m import a as b`, `use m::a as b`, `import a.b.C;`.
    #[serde(rename = "n")]
    Named,
    /// `import X from` (the target's default export).
    #[serde(rename = "d")]
    Default,
    /// `import * as ns`, `import m as ns`, a Go package, `mod m;`, `const X = preload(…)`.
    #[serde(rename = "s")]
    Namespace,
    /// Every name of the target: `export * from`, `from m import *`, `use m::*`,
    /// `import a.b.*;`, `using N;`, a Go dot import.
    #[serde(rename = "g")]
    Glob,
}

/// One name an import brings into the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    /// Line of the import statement (its import sites share it).
    #[serde(rename = "l")]
    pub line: u32,
    /// Local name (empty for a glob).
    #[serde(rename = "n")]
    pub local: String,
    /// Name in the target, when it differs or matters (`a` in `import { a as b }`).
    #[serde(rename = "i", default, skip_serializing_if = "Option::is_none")]
    pub imported: Option<String>,
    #[serde(rename = "k")]
    pub kind: BindKind,
    /// The specifier as the import resolver records it, to pick the right site when a line
    /// has several.
    #[serde(rename = "s", default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    /// Java static import: the class that holds `imported`.
    #[serde(rename = "c", default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

/// A flow entry point found in a file, before resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawEntry {
    #[serde(rename = "l")]
    pub line: u32,
    #[serde(rename = "k")]
    pub kind: EntryKind,
    #[serde(rename = "n")]
    pub name: String,
    /// The handler, named like a call (`getUser`, `ctrl.getUser`).
    #[serde(rename = "h", default, skip_serializing_if = "Option::is_none")]
    pub handler: Option<Call>,
    /// The handler is a definition of this file (qualified name).
    #[serde(rename = "d", default, skip_serializing_if = "Option::is_none")]
    pub def: Option<String>,
    /// Kept only when the file imports a library of this family (see
    /// [`resolve::gate_matches`]); `None` for patterns that identify themselves.
    #[serde(rename = "g", default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
}

const JS_QUERY: &str = concat!(
    include_str!("queries/javascript.scm"),
    "\n",
    include_str!("queries/javascript-only.scm"),
    "\n",
    include_str!("queries/jsx.scm")
);
const TS_QUERY: &str = concat!(
    include_str!("queries/javascript.scm"),
    "\n",
    include_str!("queries/typescript.scm")
);
const TSX_QUERY: &str = concat!(
    include_str!("queries/javascript.scm"),
    "\n",
    include_str!("queries/typescript.scm"),
    "\n",
    include_str!("queries/jsx.scm")
);
const PY_QUERY: &str = include_str!("queries/python.scm");
const RUST_QUERY: &str = include_str!("queries/rust.scm");
const GO_QUERY: &str = include_str!("queries/go.scm");
const JAVA_QUERY: &str = include_str!("queries/java.scm");
const CS_QUERY: &str = include_str!("queries/csharp.scm");
const C_QUERY: &str = include_str!("queries/c.scm");
const CPP_QUERY: &str = concat!(
    include_str!("queries/c.scm"),
    "\n",
    include_str!("queries/cpp.scm")
);
const GD_QUERY: &str = include_str!("queries/gdscript.scm");

/// Every symbol query, for the extraction-cache key.
pub const QUERIES: [&str; 11] = [
    JS_QUERY, TS_QUERY, TSX_QUERY, PY_QUERY, RUST_QUERY, GO_QUERY, JAVA_QUERY, CS_QUERY, C_QUERY,
    CPP_QUERY, GD_QUERY,
];

/// Bump when extraction returns something else for the same input (part of the
/// extraction-cache key with the queries themselves). 8: field types and `self.field.f()`;
/// 9: a function's or class's own `__all__` leaves the module's known; 10: `__all__[i] = …`
/// and `del __all__[i]` in a function mutate the module's list; 11: `self.field.m()` on a
/// concrete field type without `m` is unresolved again (a resolution rule, bumped so every
/// stored index is rebuilt with it); 12: a Rust `self.field.m()` finds an `impl` anywhere
/// in the type's crate, else the other rules decide; 13: [`Def::declares`] (TS abstract and
/// ambient classes); 14: supertypes ([`Def::supers`], [`Code::impls`], [`Code::extends`]),
/// `super(Class, self)`; 15: [`Def::class_name`] (an inner GDScript class on line 1 is not
/// the script); 16: methods and function properties of named JS object literals
/// (module-level `const x = {…}`, `export default {…}`, `module.exports = {…}`,
/// `exposeInMainWorld("api", {…})`) as `x.prop`; 17: Electron `ipcMain.handle` / `on`
/// entries ([`cartograph_core::EntryKind::Ipc`]).
pub const REVISION: u32 = 17;

/// Query source for a grammar.
pub fn query_source(grammar: Grammar) -> &'static str {
    match grammar {
        Grammar::TypeScript => TS_QUERY,
        Grammar::Tsx => TSX_QUERY,
        Grammar::JavaScript => JS_QUERY,
        Grammar::Python => PY_QUERY,
        Grammar::Rust => RUST_QUERY,
        Grammar::Go => GO_QUERY,
        Grammar::Java => JAVA_QUERY,
        Grammar::CSharp => CS_QUERY,
        Grammar::C => C_QUERY,
        Grammar::Cpp => CPP_QUERY,
        Grammar::GdScript => GD_QUERY,
    }
}

/// Compiled symbol query for a grammar (once, shared by every thread).
pub fn query(grammar: Grammar) -> Result<&'static Query, String> {
    static CELLS: [OnceLock<Result<Query, String>>; 11] = [const { OnceLock::new() }; 11];
    let i = Grammar::ALL.iter().position(|g| *g == grammar).unwrap_or(0);
    CELLS[i]
        .get_or_init(|| {
            Query::new(&grammar.language(), query_source(grammar))
                .map_err(|e| format!("symbol query for {}: {e}", grammar.name()))
        })
        .as_ref()
        .map_err(Clone::clone)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_symbol_query_compiles() {
        for g in Grammar::ALL {
            query(g).unwrap();
        }
    }
}
