//! Go, Java and C# imports: each `@import` node of their queries is one statement or one
//! declaration, read here into [`Spec`]s.
//!
//! - Go `import_spec` → [`Spec::Go`] with the path without quotes or backticks.
//! - Java `import_declaration` → [`Spec::Java`]; `package_declaration` →
//!   [`Spec::Declares`]; a top-level type → [`Spec::DeclaresType`] (`package.Name`).
//! - C# `using_directive` → [`Spec::CSharp`]; a namespace declaration →
//!   [`Spec::Declares`] with its full name (enclosing blocks and a file-scoped namespace
//!   included); a type declared directly in a namespace or at top level →
//!   [`Spec::DeclaresType`].
//!
//! A statement whose name is missing (a syntax error inside it) becomes a
//! [`Spec::ParseError`], never nothing. The one exception is a C# 12 alias of a type that
//! is not a name (`using Point = (int, int);`): it imports no namespace and yields nothing.

use super::{Spec, compact, snippet};
use tree_sitter::Node;

/// Specs of one `@import` node (empty for nodes this module does not know).
pub(super) fn specs(node: Node<'_>, src: &[u8]) -> Vec<Spec> {
    match node.kind() {
        "import_spec" => vec![go_import(node, src)],
        "import_declaration" => vec![java_import(node, src)],
        "package_declaration" => java_package(node, src)
            .map(Spec::Declares)
            .into_iter()
            .collect(),
        "using_directive" => csharp_using(node, src).into_iter().collect(),
        "namespace_declaration" => node
            .child_by_field_name("name")
            .map(|n| Spec::Declares(qualify(&csharp_scope(node, src), &clean_cs_name(n, src))))
            .into_iter()
            .collect(),
        "file_scoped_namespace_declaration" => node
            .child_by_field_name("name")
            .map(|n| Spec::Declares(clean_cs_name(n, src)))
            .into_iter()
            .collect(),
        _ => type_declaration(node, src).into_iter().collect(),
    }
}

/// `scope.name`, or `name` when the scope is empty.
fn qualify(scope: &str, name: &str) -> String {
    if scope.is_empty() {
        name.to_string()
    } else {
        format!("{scope}.{name}")
    }
}

fn go_import(node: Node<'_>, src: &[u8]) -> Spec {
    let Some(path) = node.child_by_field_name("path") else {
        return Spec::ParseError(snippet(node, src));
    };
    if path.has_error() || path.is_missing() {
        return Spec::ParseError(snippet(node, src));
    }
    let raw = super::text(path, src);
    let quoted = (raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"'))
        || (raw.len() >= 2 && raw.starts_with('`') && raw.ends_with('`'));
    if !quoted {
        return Spec::ParseError(snippet(node, src));
    }
    Spec::Go(raw[1..raw.len() - 1].to_string())
}

/// First named child of `node` whose kind is one of `kinds`.
fn child_of_kind<'t>(node: Node<'t>, kinds: &[&str]) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| kinds.contains(&c.kind()))
}

/// Whether `node` has a direct (anonymous or named) child of kind `kind`.
fn has_child(node: Node<'_>, kind: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|c| c.kind() == kind)
}

fn java_import(node: Node<'_>, src: &[u8]) -> Spec {
    match child_of_kind(node, &["scoped_identifier", "identifier"]) {
        Some(name) if !name.has_error() => Spec::Java {
            name: compact(name, src),
            is_static: has_child(node, "static"),
            wildcard: has_child(node, "asterisk"),
        },
        _ => Spec::ParseError(snippet(node, src)),
    }
}

fn java_package(node: Node<'_>, src: &[u8]) -> Option<String> {
    child_of_kind(node, &["scoped_identifier", "identifier"]).map(|n| compact(n, src))
}

/// The package declared by the file holding `node` (`""` for the default package).
fn java_file_package(node: Node<'_>, src: &[u8]) -> String {
    let mut root = node;
    while let Some(p) = root.parent() {
        root = p;
    }
    let mut cursor = root.walk();
    let pkg = root
        .named_children(&mut cursor)
        .find(|c| c.kind() == "package_declaration");
    pkg.and_then(|p| java_package(p, src)).unwrap_or_default()
}

/// Java or C# type declaration → its fully qualified name.
fn type_declaration(node: Node<'_>, src: &[u8]) -> Option<Spec> {
    let name = node.child_by_field_name("name")?;
    let name = compact(name, src);
    let in_java = node.parent().is_some_and(|p| p.kind() == "program");
    let scope = if in_java {
        java_file_package(node, src)
    } else {
        csharp_scope(node, src)
    };
    Some(Spec::DeclaresType(qualify(&scope, &name)))
}

/// Full name of the C# namespace that encloses `node` (not counting `node` itself): every
/// enclosing block namespace, under the file-scoped namespace if one precedes `node`.
fn csharp_scope(node: Node<'_>, src: &[u8]) -> String {
    let mut parts = Vec::new();
    let mut root = node;
    let mut cur = node.parent();
    while let Some(p) = cur {
        if p.kind() == "namespace_declaration"
            && let Some(n) = p.child_by_field_name("name")
        {
            parts.push(clean_cs_name(n, src));
        }
        root = p;
        cur = p.parent();
    }
    let mut cursor = root.walk();
    let file_scoped = root
        .named_children(&mut cursor)
        .find(|c| c.kind() == "file_scoped_namespace_declaration")
        .filter(|c| c.start_byte() <= node.start_byte() && c.id() != node.id())
        .and_then(|c| c.child_by_field_name("name"));
    if let Some(n) = file_scoped {
        parts.push(clean_cs_name(n, src));
    }
    parts.reverse();
    parts.join(".")
}

/// A C# name without whitespace, `global::` / `alias::` prefixes or generic arguments:
/// `global::System.Collections.Generic.List<int>` → `System.Collections.Generic.List`.
fn clean_cs_name(n: Node<'_>, src: &[u8]) -> String {
    let text = compact(n, src);
    // Generic arguments first: they may hold `global::` names of their own.
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    match out.split_once("::") {
        Some((_, rest)) => rest.to_string(),
        None => out,
    }
}

fn csharp_using(node: Node<'_>, src: &[u8]) -> Option<Spec> {
    let alias = node.child_by_field_name("name");
    let mut cursor = node.walk();
    let target = node
        .named_children(&mut cursor)
        .filter(|c| Some(c.id()) != alias.map(|a| a.id()) && c.kind() != "comment")
        .find(|c| !matches!(c.kind(), "global" | "static"));
    let Some(target) = target else {
        return Some(Spec::ParseError(snippet(node, src)));
    };
    // An ERROR child anywhere in the directive (`using X = ;`) or inside the target; a
    // MISSING `;` alone is recovered and keeps the using.
    let mut c2 = node.walk();
    let broken_child = node.children(&mut c2).any(|c| c.is_error());
    if broken_child || target.is_error() || target.has_error() {
        return Some(Spec::ParseError(snippet(node, src)));
    }
    if !matches!(
        target.kind(),
        "identifier" | "qualified_name" | "generic_name" | "alias_qualified_name"
    ) {
        // `using Point = (int, int);` (C# 12): an alias of a type that is not a name.
        return None;
    }
    Some(Spec::CSharp {
        name: clean_cs_name(target, src),
        is_static: has_child(node, "static"),
        alias: alias.is_some(),
        scope: csharp_scope(node, src),
    })
}

#[cfg(test)]
mod tests {
    use super::super::extract;
    use super::*;

    fn specs_of(lang: &str, src: &str) -> Vec<(u32, Spec)> {
        extract(lang, src.as_bytes())
            .into_iter()
            .map(|r| (r.line, r.spec))
            .collect()
    }

    fn java(name: &str, is_static: bool, wildcard: bool) -> Spec {
        Spec::Java {
            name: name.into(),
            is_static,
            wildcard,
        }
    }

    fn cs(name: &str, is_static: bool, alias: bool, scope: &str) -> Spec {
        Spec::CSharp {
            name: name.into(),
            is_static,
            alias,
            scope: scope.into(),
        }
    }

    #[test]
    fn go_forms() {
        let src = "package foo_test\n\nimport \"fmt\"\nimport (\n\t_ \"embed\"\n\tm \"x.org/y\"\n\
            \t. `a/b`\n\t\"C\"\n)\nfunc f() {}\n";
        assert_eq!(
            specs_of("go", src),
            vec![
                (3, Spec::Go("fmt".into())),
                (5, Spec::Go("embed".into())),
                (6, Spec::Go("x.org/y".into())),
                (7, Spec::Go("a/b".into())),
                (8, Spec::Go("C".into())),
            ]
        );
    }

    #[test]
    fn java_forms() {
        let src = "package a.b;\nimport x.y.Z;\nimport x.y.*;\nimport static x.y.Z.m;\n\
            import static x.y.Z.*;\npublic class A { class In {} }\ninterface I {}\nenum E {}\n\
            record R() {}\n@interface Ann {}\n";
        assert_eq!(
            specs_of("java", src),
            vec![
                (1, Spec::Declares("a.b".into())),
                (2, java("x.y.Z", false, false)),
                (3, java("x.y", false, true)),
                (4, java("x.y.Z.m", true, false)),
                (5, java("x.y.Z", true, true)),
                (6, Spec::DeclaresType("a.b.A".into())),
                (7, Spec::DeclaresType("a.b.I".into())),
                (8, Spec::DeclaresType("a.b.E".into())),
                (9, Spec::DeclaresType("a.b.R".into())),
                (10, Spec::DeclaresType("a.b.Ann".into())),
            ]
        );
        // Default package.
        assert_eq!(
            specs_of("java", "import java.util.List;\nclass Main {}\n"),
            vec![
                (1, java("java.util.List", false, false)),
                (2, Spec::DeclaresType("Main".into())),
            ]
        );
    }

    #[test]
    fn csharp_forms() {
        let src = "global using System;\nusing static System.Math;\n\
            using L = System.Collections.Generic.List<int>;\nusing Foo.Bar;\n\
            using global::System.Text;\nusing P = (int, int);\n\
            namespace A.B { using Inner; class C {} namespace D { struct S {} } }\n\
            namespace F;\nusing After;\npublic partial class G {}\nrecord struct RS();\n\
            interface II {}\nenum EE {}\ndelegate void Del();\n\
            class K { void M() { using (var x = Open()) {} } }\n";
        assert_eq!(
            specs_of("csharp", src),
            vec![
                (1, cs("System", false, false, "")),
                (2, cs("System.Math", true, false, "")),
                (3, cs("System.Collections.Generic.List", false, true, "")),
                (4, cs("Foo.Bar", false, false, "")),
                (5, cs("System.Text", false, false, "")),
                (7, Spec::Declares("A.B".into())),
                (7, cs("Inner", false, false, "A.B")),
                (7, Spec::DeclaresType("A.B.C".into())),
                (7, Spec::Declares("A.B.D".into())),
                (7, Spec::DeclaresType("A.B.D.S".into())),
                (8, Spec::Declares("F".into())),
                (9, cs("After", false, false, "F")),
                (10, Spec::DeclaresType("F.G".into())),
                (11, Spec::DeclaresType("F.RS".into())),
                (12, Spec::DeclaresType("F.II".into())),
                (13, Spec::DeclaresType("F.EE".into())),
                (14, Spec::DeclaresType("F.Del".into())),
                (15, Spec::DeclaresType("F.K".into())),
            ]
        );
    }

    #[test]
    fn broken_statements_are_parse_errors() {
        let got = specs_of("java", "package a;\nimport ;\nclass A {}\n");
        assert!(
            got.iter().any(|(_, s)| matches!(s, Spec::ParseError(_))),
            "{got:?}"
        );
        let got = specs_of("go", "package a\nimport (\n\t\"fmt\n)\n");
        assert!(
            got.iter().any(|(_, s)| matches!(s, Spec::ParseError(_))),
            "{got:?}"
        );
    }

    #[test]
    fn csharp_broken_using_inside_a_namespace_is_a_parse_error() {
        for src in [
            "namespace A\n{\n    using B.;\n    class C {}\n}\n",
            "namespace A\n{\n    using ;\n    class C {}\n}\n",
            "namespace A\n{\n    using X = ;\n    class D {}\n}\n",
        ] {
            let got = specs_of("csharp", src);
            assert!(
                got.iter().any(|(_, s)| matches!(s, Spec::ParseError(_))),
                "{src:?}: {got:?}"
            );
        }
        // A missing `;` is recovered by the grammar: the using is still extracted.
        let got = specs_of(
            "csharp",
            "namespace A\n{\n    using B.C\n    class D {}\n}\n",
        );
        assert!(got.contains(&(3, cs("B.C", false, false, "A"))), "{got:?}");
    }

    #[test]
    fn csharp_generic_arguments_do_not_leak_into_the_name() {
        let got = specs_of(
            "csharp",
            "using Map = System.Collections.Generic.Dictionary<string, global::App.Models.User>;\n\
             using Ext = Lib::Pkg.Thing<global::A.B>;\nusing global::System.IO;\n",
        );
        assert_eq!(
            got,
            vec![
                (
                    1,
                    cs("System.Collections.Generic.Dictionary", false, true, "")
                ),
                (2, cs("Pkg.Thing", false, true, "")),
                (3, cs("System.IO", false, false, "")),
            ]
        );
    }

    #[test]
    fn csharp_using_statements_in_broken_methods_are_not_imports() {
        let src = "namespace A;\nclass C\n{\n    void M()\n    {\n        using var x = Open();\n\
            using (var y = Open()) { }\n        int z = ;\n    }\n}\n";
        let got = specs_of("csharp", src);
        assert!(
            !got.iter().any(|(_, s)| matches!(s, Spec::ParseError(_))),
            "{got:?}"
        );
    }
}
