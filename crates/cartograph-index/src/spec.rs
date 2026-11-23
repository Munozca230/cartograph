//! What import extraction finds in a file, before resolution: [`RawImport`]s, the import
//! part of the extraction-cache payload. A leaf that both extractors build on
//! ([`crate::imports`], and [`crate::godot`] for Godot files), so neither depends on the
//! other for it. Re-exported as `crate::imports::{Spec, RawImport, MAX_SNIPPET_CHARS}`.

use crate::godot::refs::Ref;
use crate::imports::rust_spec::RustSpec;
use serde::{Deserialize, Serialize};

/// What one import says, before resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Spec {
    /// A TS/JS specifier string: `./a`, `react`, `@/x`.
    Path(String),
    /// A TypeScript specifier only types come through (erased at compile time):
    /// `import type …`, `import type x = require(…)`, `export type … from`, `import("…")` in
    /// a type position (`let a: import("./a").A`). Not `import { type A, type B }`: that one
    /// can still load the module (see `type_only_statement`). Resolved like [`Spec::Path`];
    /// the dependency is marked type-only.
    TypePath(String),
    /// A Python import. `module` is dotted and may start with dots (relative); `names` are
    /// the imported names of a `from` import (empty for `import a.b` and `from a import *`).
    Python { module: String, names: Vec<String> },
    /// A Rust item (see [`RustSpec`]).
    Rust(RustSpec),
    /// A C/C++ `#include`: `system` for the angle form `<…>`. `path` is as written.
    Include { path: String, system: bool },
    /// The specifier is computed at run time. Holds the argument expression (truncated).
    Dynamic(String),
    /// A syntax error swallowed something that looks like an import, or the file did not
    /// parse at all. Holds the start of the broken text or the error message.
    ParseError(String),
    /// A Go import path (`net/http`, `example.com/mod/pkg`), quotes removed.
    Go(String),
    /// A Java import: the dotted name without `static` / `.*`.
    Java {
        name: String,
        is_static: bool,
        wildcard: bool,
    },
    /// A C# using directive: the imported name (`global::` and generic arguments removed).
    /// `scope` is the namespace the directive sits in (`""` at top level), searched first
    /// for a relative name; `alias` is set for `using A = X.Y;`.
    CSharp {
        name: String,
        is_static: bool,
        alias: bool,
        scope: String,
    },
    /// Not an import: a package or namespace the file declares (Java `package`, C#
    /// `namespace`). Resolvers index it; it never becomes an import site.
    Declares(String),
    /// Not an import: a top-level type the file declares, fully qualified (Java, C#).
    DeclaresType(String),
    /// Not an import: a simple type name a Java file uses without importing it (a field,
    /// parameter or local type, `new X()`, `X.m()`, `extends`/`implements`, an
    /// annotation…), once per name, at its first line. A top-level type of the file's own
    /// package needs no import: the resolver links the file to the file that declares it
    /// (a `uses` edge, not an import site). Any other name yields nothing. Names are
    /// collected per file, not per scope: a method's type parameter hides a same-named type
    /// of the package in the whole file (a missing edge), and a member type inherited from
    /// a supertype and used by its simple name may be taken for a same-named type of the
    /// package (an extra edge).
    TypeRef(String),
    /// A GDScript, scene, resource or `project.godot` reference or declaration (see
    /// [`crate::godot`]).
    Godot(Ref),
}

/// One import found in a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawImport {
    /// 1-based line of the statement or call; 0 when the whole file failed to parse.
    pub line: u32,
    pub spec: Spec,
}

/// Longest expression text kept for a dynamic import or a parse error.
pub const MAX_SNIPPET_CHARS: usize = 120;
