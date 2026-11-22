//! What Rust import extraction ([`super::rust`]) finds: the [`RustSpec`]s that
//! [`crate::spec::Spec::Rust`] carries. Plain data with no dependency, so the import
//! vocabulary ([`crate::spec`]) can name it without depending on the extractor.

use serde::{Deserialize, Serialize};

/// One Rust item that the resolver looks at. `scope` is the chain of inline modules that
/// encloses the item, outermost first (empty at the top level of the file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RustSpec {
    /// `mod name;`: the module lives in another file. `path` is the `#[path = "…"]`
    /// attribute, if any.
    Mod {
        scope: Vec<String>,
        name: String,
        path: Option<String>,
    },
    /// `mod name { … }`: a module declared inline. Not an import.
    InlineMod { scope: Vec<String>, name: String },
    /// A `use` declaration, flattened into one leaf per imported path.
    Use {
        scope: Vec<String>,
        leaves: Vec<UseLeaf>,
    },
    /// `extern crate name;` / `extern crate name as alias;`.
    ExternCrate { name: String, alias: Option<String> },
    /// A qualified path written in code outside `use` (an expression, a type, a pattern,
    /// a macro's arguments) whose first segment can name a crate or a module: `crate`,
    /// `self`, `super`, a leading `::`, or a lowercase name (`krate::f`, `m::g`). Cut after
    /// its first uppercase segment (`krate::Type::new` → `krate::Type`): what follows a
    /// type never changes the file. One per distinct `(scope, path)` of a file, at its
    /// first line. The resolver keeps only those that lead somewhere (`u32::MAX` and
    /// other non-crate names are dropped silently), once per target.
    Path {
        scope: Vec<String>,
        path: Vec<String>,
    },
}

/// One imported path of a `use` tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UseLeaf {
    /// Path segments. A leading empty segment stands for a leading `::` (`::std::fmt`).
    /// `self` at the end of a group (`a::{self}`) is dropped, so the leaf is `a`.
    pub path: Vec<String>,
    /// `as` name (`as _` included).
    pub alias: Option<String>,
    /// `path::*`.
    pub glob: bool,
}

impl UseLeaf {
    /// The leaf as written: `crate::a::b`, `a::*`, `a::b as c`.
    pub fn text(&self) -> String {
        let mut s = self.path.join("::");
        if self.glob {
            if !s.is_empty() {
                s.push_str("::");
            }
            s.push('*');
        }
        if let Some(a) = &self.alias {
            s.push_str(" as ");
            s.push_str(a);
        }
        s
    }

    /// The name this leaf brings into scope (`b` for `a::b`, `c` for `a::b as c`); `None`
    /// for globs and `as _`.
    pub fn bound_name(&self) -> Option<&str> {
        if self.glob {
            return None;
        }
        match self.alias.as_deref() {
            Some("_") => None,
            Some(a) => Some(a),
            None => self
                .path
                .last()
                .map(String::as_str)
                .filter(|s| !s.is_empty()),
        }
    }
}
