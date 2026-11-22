//! Rust import extraction: `mod x;`, `use` trees and `extern crate`.
//!
//! The query (`queries/rust.scm`) only finds the items; this module walks the syntax tree
//! for what a flat capture cannot express:
//! - the **inline-module scope** of every item (`mod tests { use super::*; }`: `super` is
//!   the file's own module, not its parent), recorded as the chain of enclosing
//!   `mod name { … }` blocks;
//! - `#[path = "…"]` on `mod x;`;
//! - the flattening of `use a::{b, c::{d as e, *}, self}` into one leaf per imported path.
//!
//! Inline modules (`mod x { … }`) are recorded as [`RustSpec::InlineMod`]: a declaration,
//! not an import. The resolver needs it to tell an inline module from a missing one, and
//! yields no import site for it.
//!
//! Qualified paths written in code without a `use` (`krate::run(x)`, `crate::m::T`,
//! `super::f()` in an expression, a type, a pattern or a macro's arguments) are recorded
//! as [`RustSpec::Path`]: a file depends on a crate it calls through a full path as much
//! as on one it `use`s.

use super::Family;
pub use super::rust_spec::{RustSpec, UseLeaf};
use crate::grammars::Grammar;
use crate::spec::{RawImport, Spec};
use crate::syntax::inline_scope;
use std::collections::HashSet;
use std::ops::Range;
use tree_sitter::{Node, QueryCursor, StreamingIterator, Tree};

/// Extract the Rust items of a file (see the module docs). Syntax errors that swallow a
/// `use` or `mod` yield [`Spec::ParseError`] like the other languages.
pub(super) fn extract(
    grammar: Grammar,
    tree: &tree_sitter::Tree,
    src: &[u8],
) -> Result<Vec<RawImport>, String> {
    let query = super::query(grammar)?;
    let names = query.capture_names();

    let mut found: Vec<(usize, RawImport)> = Vec::new();
    let mut covered: Vec<Range<usize>> = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src);
    while let Some(m) = matches.next() {
        let (mut stmt, mut arg, mut module, mut krate, mut alias) = (None, None, None, None, None);
        for c in m.captures() {
            match names.get(c.index as usize).copied().unwrap_or("") {
                "import" => stmt = Some(c.node),
                "use" => arg = Some(c.node),
                "mod" => module = Some(c.node),
                "crate" => krate = Some(c.node),
                "alias" => alias = Some(c.node),
                _ => {}
            }
        }
        let Some(stmt) = stmt else { continue };
        if !seen.insert(stmt.id()) {
            continue;
        }
        let scope = inline_scope(stmt, src);
        let spec = if let Some(arg) = arg {
            let mut leaves = Vec::new();
            flatten(arg, src, &[], &mut leaves);
            if leaves.is_empty() {
                Spec::ParseError(super::snippet(stmt, src))
            } else {
                Spec::Rust(RustSpec::Use { scope, leaves })
            }
        } else if let Some(name) = module {
            let name = super::text(name, src);
            if stmt.child_by_field_name("body").is_some() {
                Spec::Rust(RustSpec::InlineMod { scope, name })
            } else {
                Spec::Rust(RustSpec::Mod {
                    scope,
                    name,
                    path: path_attribute(stmt, src),
                })
            }
        } else if let Some(name) = krate {
            Spec::Rust(RustSpec::ExternCrate {
                name: super::text(name, src),
                alias: alias.map(|a| super::text(a, src)),
            })
        } else {
            continue;
        };
        covered.push(stmt.byte_range());
        found.push((
            stmt.start_byte(),
            RawImport {
                line: super::line_of(stmt),
                spec,
            },
        ));
    }
    found.extend(code_paths(tree.root_node(), src));
    found.sort_by_key(|(at, _)| *at);
    let mut out: Vec<RawImport> = found.into_iter().map(|(_, r)| r).collect();
    if tree.root_node().has_error() {
        out.extend(error_imports(tree, src, Family::Rust, &covered));
        out.sort_by_key(|r| r.line);
    }
    Ok(out)
}

/// [`Spec::ParseError`] imports for the syntax errors that swallow an import keyword not
/// covered by an extracted item (shared by the Rust and C/C++ extractors).
pub(super) fn error_imports(
    tree: &Tree,
    src: &[u8],
    family: Family,
    covered: &[Range<usize>],
) -> Vec<RawImport> {
    let mut out = Vec::new();
    for err in super::outermost_errors(tree) {
        if let Some(at) = super::uncovered_keyword(err, src, family, covered) {
            let text = String::from_utf8_lossy(src.get(at..err.end_byte()).unwrap_or(&[]))
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .chars()
                .take(super::MAX_SNIPPET_CHARS)
                .collect();
            out.push(RawImport {
                line: super::line_at(src, at),
                spec: Spec::ParseError(text),
            });
        }
    }
    out
}

/// Value of a `#[path = "…"]` attribute among the attributes right before `item`.
fn path_attribute(item: Node<'_>, src: &[u8]) -> Option<String> {
    let mut prev = item.prev_named_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {
                let mut c = p.walk();
                for attr in p.named_children(&mut c) {
                    if attr.kind() != "attribute" {
                        continue;
                    }
                    let is_path = attr
                        .named_child(0)
                        .is_some_and(|n| n.kind() == "identifier" && super::text(n, src) == "path");
                    if is_path && let Some(value) = attr.child_by_field_name("value") {
                        return Some(string_value(value, src));
                    }
                }
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        prev = p.prev_named_sibling();
    }
    None
}

/// Content of a (raw) string literal, escapes kept as written.
fn string_value(n: Node<'_>, src: &[u8]) -> String {
    let mut out = String::new();
    let mut c = n.walk();
    for child in n.named_children(&mut c) {
        if matches!(child.kind(), "string_content" | "escape_sequence") {
            out.push_str(&super::text(child, src));
        }
    }
    out
}

/// Path segments of a path node (`a::b::c`, `::std`, `super`), whitespace ignored.
fn segments(n: Node<'_>, src: &[u8]) -> Vec<String> {
    let t: String = super::text(n, src)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    t.split("::").map(str::to_string).collect()
}

/// Syntax that holds no code path worth an import edge: `use` and `extern crate` (already
/// imports), attributes (`#[allow(clippy::x)]`, `#[serde(…)]`), `pub(in path)` and macro
/// definitions (their bodies are token patterns, not code), comments, strings, and syntax
/// errors (a broken `use` there is already a parse error).
const NO_CODE_PATHS: [&str; 11] = [
    "ERROR",
    "use_declaration",
    "extern_crate_declaration",
    "attribute_item",
    "inner_attribute_item",
    "visibility_modifier",
    "macro_definition",
    "line_comment",
    "block_comment",
    "string_literal",
    "raw_string_literal",
];

/// [`RustSpec::Path`] items of a file, with the byte offset where each is first written.
fn code_paths(root: Node<'_>, src: &[u8]) -> Vec<(usize, RawImport)> {
    let mut out = Vec::new();
    let mut seen: HashSet<(Vec<String>, Vec<String>)> = HashSet::new();
    let mut record = |at: Node<'_>, path: Vec<String>, out: &mut Vec<(usize, RawImport)>| {
        let Some(path) = keep_path(path) else { return };
        let scope = inline_scope(at, src);
        if seen.insert((scope.clone(), path.clone())) {
            out.push((
                at.start_byte(),
                RawImport {
                    line: super::line_of(at),
                    spec: Spec::Rust(RustSpec::Path { scope, path }),
                },
            ));
        }
    };
    // Pre-order, in source order, so the first occurrence of a path is the one kept.
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let kind = n.kind();
        if NO_CODE_PATHS.contains(&kind) {
            continue;
        }
        if matches!(kind, "scoped_identifier" | "scoped_type_identifier")
            && let Some(path) = code_path(n, src)
        {
            record(n, path, &mut out);
            continue;
        }
        if kind == "token_tree" {
            for (at, path) in token_paths(n, src) {
                record(at, path, &mut out);
            }
        }
        let mut c = n.walk();
        let children: Vec<Node<'_>> = n.children(&mut c).collect();
        stack.extend(children.into_iter().rev());
    }
    out
}

/// Segments of a `scoped_identifier` / `scoped_type_identifier` made of plain names only
/// (`crate`, `self`, `super`, `$crate` included; `""` first for a leading `::`). `None` for
/// generic or qualified paths (`Vec::<u8>::new`, `<T as Tr>::f`).
fn code_path(n: Node<'_>, src: &[u8]) -> Option<Vec<String>> {
    let mut segs = Vec::new();
    let mut cur = n;
    loop {
        match cur.kind() {
            "scoped_identifier" | "scoped_type_identifier" => {
                let name = cur.child_by_field_name("name")?;
                if !matches!(
                    name.kind(),
                    "identifier" | "type_identifier" | "self" | "super" | "crate"
                ) {
                    return None;
                }
                segs.push(super::text(name, src));
                match cur.child_by_field_name("path") {
                    Some(p) => cur = p,
                    None => {
                        segs.push(String::new());
                        break;
                    }
                }
            }
            "identifier" | "crate" | "self" | "super" | "metavariable" => {
                segs.push(super::text(cur, src));
                break;
            }
            _ => return None,
        }
    }
    segs.reverse();
    Some(segs)
}

/// Paths in a macro's raw tokens: runs of `name :: name (:: name)*` not preceded by `.`
/// or `::`, with the token each starts at.
fn token_paths<'t>(tt: Node<'t>, src: &[u8]) -> Vec<(Node<'t>, Vec<String>)> {
    let mut out = Vec::new();
    let mut run: Option<(Node<'t>, Vec<String>)> = None;
    // The previous token was a `::` that continues `run`.
    let mut joined = false;
    let mut prev: Option<&str> = None;
    let mut c = tt.walk();
    for ch in tt.children(&mut c) {
        let kind = ch.kind();
        let segment = matches!(
            kind,
            "identifier" | "crate" | "self" | "super" | "metavariable"
        );
        match (&mut run, segment, kind) {
            (Some((_, segs)), true, _) if joined => {
                segs.push(super::text(ch, src));
                joined = false;
            }
            (Some(_), false, "::") if !joined => joined = true,
            _ => {
                if let Some((at, segs)) = run.take()
                    && segs.len() > 1
                {
                    out.push((at, segs));
                }
                joined = false;
                if segment && !matches!(prev, Some("." | "::")) {
                    run = Some((ch, vec![super::text(ch, src)]));
                }
            }
        }
        prev = Some(kind);
    }
    if let Some((at, segs)) = run
        && segs.len() > 1
    {
        out.push((at, segs));
    }
    out
}

/// A code path worth resolving (see [`RustSpec::Path`]): at least two segments, a first
/// one that can name a crate or a module, cut after its first uppercase segment.
fn keep_path(mut path: Vec<String>) -> Option<Vec<String>> {
    let first = path.first()?;
    let module_like = matches!(first.as_str(), "" | "crate" | "self" | "super" | "$crate")
        || first
            .strip_prefix("r#")
            .unwrap_or(first)
            .starts_with(|c: char| c.is_ascii_lowercase() || c == '_');
    if path.len() < 2 || !module_like || first == "_" {
        return None;
    }
    if let Some(i) = path
        .iter()
        .skip(1)
        .position(|s| s.starts_with(char::is_uppercase))
    {
        path.truncate(i + 2);
    }
    Some(path)
}

/// Flatten a `use` tree under `prefix` into leaves.
fn flatten(n: Node<'_>, src: &[u8], prefix: &[String], out: &mut Vec<UseLeaf>) {
    let leaf = |mut path: Vec<String>, alias: Option<String>, glob: bool| {
        if !glob && path.len() > 1 && path.last().is_some_and(|s| s == "self") {
            path.pop();
        }
        UseLeaf { path, alias, glob }
    };
    let join = |rest: Vec<String>| -> Vec<String> {
        let mut p = prefix.to_vec();
        p.extend(rest);
        p
    };
    match n.kind() {
        "use_list" => {
            let mut c = n.walk();
            for child in n.named_children(&mut c) {
                flatten(child, src, prefix, out);
            }
        }
        "scoped_use_list" => {
            let p = match n.child_by_field_name("path") {
                Some(path) => join(segments(path, src)),
                // `use ::{a, b}` (2015-style extern root).
                None if super::text(n, src).trim_start().starts_with("::") => {
                    join(vec![String::new()])
                }
                None => prefix.to_vec(),
            };
            if let Some(list) = n.child_by_field_name("list") {
                flatten(list, src, &p, out);
            }
        }
        "use_as_clause" => {
            if let Some(path) = n.child_by_field_name("path") {
                let alias = n.child_by_field_name("alias").map(|a| super::text(a, src));
                out.push(leaf(join(segments(path, src)), alias, false));
            }
        }
        "use_wildcard" => {
            let mut c = n.walk();
            let path = n
                .named_children(&mut c)
                .find(|ch| !matches!(ch.kind(), "line_comment" | "block_comment"))
                .map(|p| segments(p, src))
                .unwrap_or_default();
            out.push(leaf(join(path), None, true));
        }
        "identifier" | "crate" | "self" | "super" | "scoped_identifier" | "metavariable" => {
            out.push(leaf(join(segments(n, src)), None, false));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs(src: &str) -> Vec<(u32, Spec)> {
        super::super::extract("rust", src.as_bytes())
            .into_iter()
            .map(|r| (r.line, r.spec))
            .collect()
    }

    fn sv(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    fn leaf(path: &[&str]) -> UseLeaf {
        UseLeaf {
            path: sv(path),
            alias: None,
            glob: false,
        }
    }

    fn used(scope: &[&str], leaves: Vec<UseLeaf>) -> Spec {
        Spec::Rust(RustSpec::Use {
            scope: sv(scope),
            leaves,
        })
    }

    #[test]
    fn use_trees_are_flattened() {
        let src = "use std::fmt;\n\
            pub use crate::a::{b, c::d as e, f::*, self};\n\
            use super::super::x;\n\
            use ::serde::{Deserialize, Serialize};\n\
            use self::Kind::*;\n\
            pub(crate) use {g, h::{self as hh}};\n";
        let glob = |p: &[&str]| UseLeaf {
            path: sv(p),
            alias: None,
            glob: true,
        };
        assert_eq!(
            specs(src),
            vec![
                (1, used(&[], vec![leaf(&["std", "fmt"])])),
                (
                    2,
                    used(
                        &[],
                        vec![
                            leaf(&["crate", "a", "b"]),
                            UseLeaf {
                                path: sv(&["crate", "a", "c", "d"]),
                                alias: Some("e".into()),
                                glob: false
                            },
                            glob(&["crate", "a", "f"]),
                            leaf(&["crate", "a"]),
                        ]
                    )
                ),
                (3, used(&[], vec![leaf(&["super", "super", "x"])])),
                (
                    4,
                    used(
                        &[],
                        vec![
                            leaf(&["", "serde", "Deserialize"]),
                            leaf(&["", "serde", "Serialize"])
                        ]
                    )
                ),
                (5, used(&[], vec![glob(&["self", "Kind"])])),
                (
                    6,
                    used(
                        &[],
                        vec![
                            leaf(&["g"]),
                            UseLeaf {
                                path: sv(&["h"]),
                                alias: Some("hh".into()),
                                glob: false
                            }
                        ]
                    )
                ),
            ]
        );
    }

    #[test]
    fn modules_scopes_and_path_attributes() {
        let src = "mod a;\n\
            #[cfg(test)]\n\
            #[path = \"other/b_impl.rs\"]\n\
            pub mod b;\n\
            mod inner {\n    mod c;\n    fn f() { use super::*; }\n    \
            pub mod deep { use super::super::a; }\n}\n\
            extern crate alloc;\nextern crate foo as bar;\n";
        assert_eq!(
            specs(src),
            vec![
                (
                    1,
                    Spec::Rust(RustSpec::Mod {
                        scope: vec![],
                        name: "a".into(),
                        path: None
                    })
                ),
                (
                    4,
                    Spec::Rust(RustSpec::Mod {
                        scope: vec![],
                        name: "b".into(),
                        path: Some("other/b_impl.rs".into())
                    })
                ),
                (
                    5,
                    Spec::Rust(RustSpec::InlineMod {
                        scope: vec![],
                        name: "inner".into()
                    })
                ),
                (
                    6,
                    Spec::Rust(RustSpec::Mod {
                        scope: sv(&["inner"]),
                        name: "c".into(),
                        path: None
                    })
                ),
                (
                    7,
                    used(
                        &["inner"],
                        vec![UseLeaf {
                            path: sv(&["super"]),
                            alias: None,
                            glob: true
                        }]
                    )
                ),
                (
                    8,
                    Spec::Rust(RustSpec::InlineMod {
                        scope: sv(&["inner"]),
                        name: "deep".into()
                    })
                ),
                (
                    8,
                    used(&["inner", "deep"], vec![leaf(&["super", "super", "a"])])
                ),
                (
                    10,
                    Spec::Rust(RustSpec::ExternCrate {
                        name: "alloc".into(),
                        alias: None
                    })
                ),
                (
                    11,
                    Spec::Rust(RustSpec::ExternCrate {
                        name: "foo".into(),
                        alias: Some("bar".into())
                    })
                ),
            ]
        );
    }

    #[test]
    fn leaf_text_and_bound_name() {
        let l = UseLeaf {
            path: sv(&["a", "b"]),
            alias: Some("c".into()),
            glob: false,
        };
        assert_eq!(l.text(), "a::b as c");
        assert_eq!(l.bound_name(), Some("c"));
        assert_eq!(leaf(&["a", "b"]).bound_name(), Some("b"));
        let g = UseLeaf {
            path: sv(&["a"]),
            alias: None,
            glob: true,
        };
        assert_eq!(g.text(), "a::*");
        assert_eq!(g.bound_name(), None);
    }

    fn paths(src: &str) -> Vec<(u32, String, String)> {
        specs(src)
            .into_iter()
            .filter_map(|(line, s)| match s {
                Spec::Rust(RustSpec::Path { scope, path }) => {
                    Some((line, scope.join("::"), path.join("::")))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn paths_in_code_are_recorded_once() {
        let src = "use other::Thing;\n\
            #[allow(clippy::too_many_lines)]\n\
            #[derive(serde::Serialize)]\n\
            struct S { a: krate_a::Node, b: Vec<::krate_b::m::T> }\n\
            fn f(x: krate_a::Node) -> u32 {\n\
            \x20   krate_c::run(x);\n\
            \x20   let k = crate::kind::Kind::File;\n\
            \x20   match k { super::Kind::A => {} _ => {} }\n\
            \x20   println!(\"{}\", krate_d::util::show(&x.krate_x::y));\n\
            \x20   let s = \"fake::path\"; // comment::path\n\
            \x20   let v = Vec::<u8>::new(); let n = u32::MAX; Self::g(); Type::h();\n\
            \x20   krate_c::run(x);\n\
            \x20   self::local::f()\n\
            }\n\
            macro_rules! m { () => { $crate::hidden::f() }; }\n\
            pub(in crate::outer) fn g() {}\n\
            mod tests { fn t() { super::f(krate_a::Node::new()); } }\n";
        assert_eq!(
            paths(src),
            vec![
                (4, String::new(), "krate_a::Node".into()),
                (4, String::new(), "::krate_b::m::T".into()),
                (6, String::new(), "krate_c::run".into()),
                (7, String::new(), "crate::kind::Kind".into()),
                (8, String::new(), "super::Kind".into()),
                (9, String::new(), "krate_d::util::show".into()),
                // `x.krate_x::y` is not a path (it follows a `.`).
                (11, String::new(), "u32::MAX".into()),
                (13, String::new(), "self::local::f".into()),
                (17, "tests".into(), "super::f".into()),
                (17, "tests".into(), "krate_a::Node".into()),
            ]
        );
    }

    #[test]
    fn broken_use_is_a_parse_error() {
        let got = specs("use std::fmt;\nfn f( {\nuse crate::x::{a\n");
        assert!(
            got.iter().any(|(_, s)| matches!(s, Spec::ParseError(_))),
            "{got:?}"
        );
        assert_eq!(
            got.first(),
            Some(&(1, used(&[], vec![leaf(&["std", "fmt"])])))
        );
    }
}
