//! GDScript references with tree-sitter (`imports/queries/gdscript.scm`).
//!
//! What becomes a [`Ref`]:
//! - `extends "res://base.gd"` → `Path` (`extends`); `extends Name` → `Name { extends }`;
//! - `preload(…)`, `load(…)`, `ResourceLoader.load(…)` / `load_threaded_request(…)`: a
//!   string literal argument → `Path` (role = the function), anything else → `Dynamic`;
//! - any other string literal starting with `res://` or `uid://` → `Path` (`string`,
//!   heuristic), or `Dynamic` when it is an operand of `+` / `%` or holds a `%` / `{`
//!   placeholder (it is only part of a path);
//! - `class_name Name` → `DeclareClass`;
//! - an upper-case identifier that is not a member name, not declared in the file (const,
//!   inner class, enum), not the file's own class and not a Godot built-in → `Name`, once
//!   per file (first line). The resolver keeps only names of project classes and
//!   autoloads; the rest (enum values, constants of other scopes…) are not imports.
//!
//! A syntax error that swallows text with `res://`, `uid://`, `preload`, `load(` or `extends` not
//! already extracted becomes a `ParseError` record.

use super::{PathKind, Ref, is_builtin_class, looks_like_resource_path, text};
use crate::grammars::Grammar;
use crate::spec::{MAX_SNIPPET_CHARS, RawImport, Spec};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::OnceLock;
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator, Tree};

/// The GDScript query.
pub const QUERY: &str = include_str!("../imports/queries/gdscript.scm");

/// Upper-case global constants of GDScript that are not classes.
const GLOBAL_CONSTANTS: &[&str] = &["PI", "TAU", "INF", "NAN"];

/// Compiled query (once, shared by every thread).
pub fn query() -> Result<&'static Query, String> {
    static Q: OnceLock<Result<Query, String>> = OnceLock::new();
    Q.get_or_init(|| {
        Query::new(&Grammar::GdScript.language(), QUERY)
            .map_err(|e| format!("import query for gdscript: {e}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

thread_local! {
    static PARSER: RefCell<Option<Parser>> = const { RefCell::new(None) };
}

/// Parse with this thread's GDScript parser.
pub fn parse(src: &[u8]) -> Result<Tree, String> {
    PARSER.with(|cell| {
        let mut slot = cell
            .try_borrow_mut()
            .map_err(|_| "parser already in use on this thread".to_string())?;
        if slot.is_none() {
            *slot = Some(
                Grammar::GdScript
                    .parser()
                    .map_err(|e| format!("grammar gdscript: {e}"))?,
            );
        }
        slot.as_mut()
            .and_then(|p| p.parse(src, None))
            .ok_or_else(|| "tree-sitter returned no tree (gdscript)".to_string())
    })
}

/// Extract the references of a GDScript file. Never fails: a file that cannot be parsed
/// yields one `ParseError` at line 0.
pub fn extract(src: &[u8]) -> Vec<RawImport> {
    match extract_inner(src) {
        Ok(v) => v,
        Err(e) => vec![RawImport {
            line: 0,
            spec: Spec::ParseError(e),
        }],
    }
}

struct Load<'t> {
    func: Node<'t>,
    obj: Option<Node<'t>>,
    arg: Node<'t>,
}

#[derive(Default)]
struct Captures<'t> {
    extends_paths: Vec<Node<'t>>,
    extends_names: Vec<Node<'t>>,
    class_decls: Vec<Node<'t>>,
    locals: HashSet<String>,
    loads: Vec<Load<'t>>,
    strings: Vec<Node<'t>>,
    idents: Vec<Node<'t>>,
}

/// [`extract`] on a tree already parsed with [`parse`].
pub fn extract_tree(tree: &Tree, src: &[u8]) -> Vec<RawImport> {
    match extract_parsed(tree, src) {
        Ok(v) => v,
        Err(e) => vec![RawImport {
            line: 0,
            spec: Spec::ParseError(e),
        }],
    }
}

fn extract_inner(src: &[u8]) -> Result<Vec<RawImport>, String> {
    let tree = parse(src)?;
    extract_parsed(&tree, src)
}

fn extract_parsed(tree: &Tree, src: &[u8]) -> Result<Vec<RawImport>, String> {
    let query = query()?;
    let names = query.capture_names();
    let mut c = Captures::default();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src);
    while let Some(m) = matches.next() {
        let (mut func, mut obj, mut arg) = (None, None, None);
        for cap in m.captures() {
            let n = cap.node;
            match names.get(cap.index as usize).copied().unwrap_or("") {
                "extends.path" => c.extends_paths.push(n),
                "extends.name" => c.extends_names.push(n),
                "class.decl" => c.class_decls.push(n),
                "local" => {
                    c.locals.insert(text(n, src));
                }
                "load.fn" => func = Some(n),
                "load.obj" => obj = Some(n),
                "load.arg" => arg = Some(n),
                "string" => c.strings.push(n),
                "ident" => c.idents.push(n),
                _ => {}
            }
        }
        if let (Some(func), Some(arg)) = (func, arg) {
            c.loads.push(Load { func, obj, arg });
        }
    }

    let mut out = Vec::new();
    let mut covered: Vec<Range<usize>> = Vec::new();
    let mut push = |out: &mut Vec<RawImport>, node: Node<'_>, r: Ref| {
        covered.push(node.byte_range());
        out.push(RawImport {
            line: line_of(node),
            spec: Spec::Godot(r),
        });
    };

    let own: HashSet<String> = c.class_decls.iter().map(|n| text(*n, src)).collect();
    for n in &c.class_decls {
        push(&mut out, *n, Ref::DeclareClass(text(*n, src)));
    }
    for n in &c.extends_paths {
        if let Some(path) = unquote(&text(*n, src)) {
            push(
                &mut out,
                *n,
                Ref::Path {
                    role: "extends".into(),
                    path,
                    uid: None,
                    kind: PathKind::Extends,
                },
            );
        }
    }
    let extends_ids: HashSet<usize> = c.extends_names.iter().map(Node::id).collect();
    for n in &c.extends_names {
        push(
            &mut out,
            *n,
            Ref::Name {
                name: text(*n, src),
                extends: true,
            },
        );
    }
    // Every string inside a load argument belongs to that load.
    let mut in_load: Vec<Range<usize>> = Vec::new();
    for l in &c.loads {
        let func = text(l.func, src);
        let is_load = match l.obj {
            None => func == "preload" || func == "load",
            Some(o) => {
                text(o, src) == "ResourceLoader"
                    && (func == "load" || func == "load_threaded_request")
            }
        };
        if !is_load {
            continue;
        }
        in_load.push(l.arg.byte_range());
        let r = match (l.arg.kind(), unquote(&text(l.arg, src))) {
            ("string", Some(path)) => Ref::Path {
                role: func,
                path,
                uid: None,
                kind: PathKind::Direct,
            },
            _ => Ref::Dynamic {
                role: func,
                text: snippet(l.arg, src),
            },
        };
        push(&mut out, l.arg, r);
    }
    let extends_ranges: Vec<Range<usize>> = c.extends_paths.iter().map(Node::byte_range).collect();
    for n in &c.strings {
        let r = n.byte_range();
        if in_load
            .iter()
            .chain(&extends_ranges)
            .any(|x| x.start <= r.start && r.end <= x.end)
        {
            continue;
        }
        let Some(value) = unquote(&text(*n, src)) else {
            continue;
        };
        if !looks_like_resource_path(&value) {
            continue;
        }
        // Only `+` and `%` build a path; `==`, `in`, `and`… just compare or test one.
        let parent = n
            .parent()
            .filter(|p| p.kind() == "binary_operator" && builds_a_string(*p, src));
        let r = if let Some(p) = parent {
            Ref::Dynamic {
                role: "string".into(),
                text: snippet(p, src),
            }
        } else if value.contains('%') || value.contains('{') {
            Ref::Dynamic {
                role: "string".into(),
                text: snippet(*n, src),
            }
        } else {
            Ref::Path {
                role: "string".into(),
                path: value,
                uid: None,
                kind: PathKind::String,
            }
        };
        push(&mut out, *n, r);
    }

    let mut seen: HashMap<String, Node<'_>> = HashMap::new();
    for n in &c.idents {
        if extends_ids.contains(&n.id()) || !is_reference(*n) {
            continue;
        }
        let name = text(*n, src);
        if own.contains(&name)
            || c.locals.contains(&name)
            || is_builtin_class(&name)
            || GLOBAL_CONSTANTS.contains(&name.as_str())
        {
            continue;
        }
        match seen.get(&name) {
            Some(prev) if prev.start_byte() <= n.start_byte() => {}
            _ => {
                seen.insert(name, *n);
            }
        }
    }
    let mut names: Vec<(String, Node<'_>)> = seen.into_iter().collect();
    names.sort_by_key(|(_, n)| n.start_byte());
    for (name, n) in names {
        push(
            &mut out,
            n,
            Ref::Name {
                name,
                extends: false,
            },
        );
    }

    // A whole extracted call or extends statement is covered, so its keyword inside a
    // syntax error is not reported again.
    for l in &c.loads {
        if let Some(call) = call_of(l.func) {
            covered.push(call.byte_range());
        }
    }
    for n in c.extends_paths.iter().chain(&c.extends_names) {
        let mut p = n.parent();
        while let Some(x) = p {
            if x.kind() == "extends_statement" {
                covered.push(x.byte_range());
                break;
            }
            p = x.parent();
        }
    }
    if tree.root_node().has_error() {
        for err in outermost_errors(tree) {
            if let Some(at) = uncovered_keyword(err, src, &covered) {
                let text: String =
                    String::from_utf8_lossy(src.get(at..err.end_byte()).unwrap_or(&[]))
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .chars()
                        .take(MAX_SNIPPET_CHARS)
                        .collect();
                out.push(RawImport {
                    line: line_at(src, at),
                    spec: Spec::ParseError(text),
                });
            }
        }
    }
    out.sort_by_key(|r| r.line);
    Ok(out)
}

/// Whether a binary operator node concatenates or formats (`+`, `%`).
fn builds_a_string(op: Node<'_>, src: &[u8]) -> bool {
    let mut cursor = op.walk();
    op.children_by_field_name("op", &mut cursor)
        .any(|o| matches!(src.get(o.byte_range()), Some(b"+" | b"%")))
}

/// The call node of a load function name: `load(…)` or `ResourceLoader.load(…)`.
fn call_of(func: Node<'_>) -> Option<Node<'_>> {
    let p = func.parent()?;
    match p.kind() {
        "call" => Some(p),
        "attribute_call" => p.parent().filter(|a| a.kind() == "attribute").or(Some(p)),
        _ => None,
    }
}

/// Whether an identifier is a use of a name, not a member, parameter or enum value name.
fn is_reference(n: Node<'_>) -> bool {
    let Some(p) = n.parent() else { return true };
    let first_named = |p: Node<'_>| p.named_child(0).is_some_and(|c| c.id() == n.id());
    match p.kind() {
        "attribute" => first_named(p),
        "enumerator" => !first_named(p),
        "attribute_call"
        | "attribute_subscript"
        | "typed_parameter"
        | "typed_default_parameter"
        | "default_parameter"
        | "parameters"
        | "annotation" => false,
        "for_statement" => p
            .child_by_field_name("left")
            .is_none_or(|l| l.id() != n.id()),
        _ => true,
    }
}

/// The value of a GDScript string literal: `"…"`, `'…'`, `"""…"""`, `'''…'''`, with an
/// optional `r` (raw) prefix.
pub fn unquote(t: &str) -> Option<String> {
    let (raw, t) = match t.strip_prefix('r') {
        Some(rest) => (true, rest),
        None => (false, t),
    };
    for q in ["\"\"\"", "'''", "\"", "'"] {
        if t.len() >= 2 * q.len()
            && let Some(body) = t.strip_prefix(q).and_then(|b| b.strip_suffix(q))
        {
            return Some(if raw {
                body.to_string()
            } else {
                text::unescape(body)
            });
        }
    }
    None
}

fn text(n: Node<'_>, src: &[u8]) -> String {
    String::from_utf8_lossy(src.get(n.byte_range()).unwrap_or(&[])).into_owned()
}

fn snippet(n: Node<'_>, src: &[u8]) -> String {
    text(n, src)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_SNIPPET_CHARS)
        .collect()
}

fn line_of(n: Node<'_>) -> u32 {
    u32::try_from(n.start_position().row + 1).unwrap_or(u32::MAX)
}

fn line_at(src: &[u8], byte: usize) -> u32 {
    let newlines = src
        .get(..byte)
        .map_or(0, |b| b.iter().filter(|&&c| c == b'\n').count());
    u32::try_from(newlines + 1).unwrap_or(u32::MAX)
}

/// ERROR nodes that are not inside another ERROR node.
fn outermost_errors(tree: &Tree) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        let descend = if node.is_error() {
            out.push(node);
            false
        } else {
            node.has_error()
        };
        if descend && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return out;
            }
        }
    }
}

/// First byte inside `err` of a reference keyword not covered by an extracted reference.
fn uncovered_keyword(err: Node<'_>, src: &[u8], covered: &[Range<usize>]) -> Option<usize> {
    let range = err.byte_range();
    let body = src.get(range.clone())?;
    let mut best: Option<usize> = None;
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    // (keyword, whether it must stand alone as a word: `reload(`, `extends_list` are not).
    for (kw, word) in [
        ("res://", false),
        ("uid://", false),
        ("preload", true),
        ("load(", true),
        ("extends", true),
    ] {
        let kw = kw.as_bytes();
        let mut i = 0;
        while i + kw.len() <= body.len() {
            if body.get(i..i + kw.len()) == Some(kw) {
                let at = range.start + i;
                let before = at.checked_sub(1).and_then(|j| src.get(j)).copied();
                let after = src.get(at + kw.len()).copied();
                let bounded = !word
                    || (!before.is_some_and(is_ident)
                        && (kw.ends_with(b"(") || !after.is_some_and(is_ident)));
                if bounded && !covered.iter().any(|r| r.contains(&at)) {
                    best = Some(best.map_or(at, |b| b.min(at)));
                    break;
                }
            }
            i += 1;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(src: &str) -> Vec<(u32, Ref)> {
        extract(src.as_bytes())
            .into_iter()
            .map(|r| match r.spec {
                Spec::Godot(g) => (r.line, g),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    fn path(role: &str, p: &str, kind: PathKind) -> Ref {
        Ref::Path {
            role: role.into(),
            path: p.into(),
            uid: None,
            kind,
        }
    }

    fn name(n: &str, extends: bool) -> Ref {
        Ref::Name {
            name: n.into(),
            extends,
        }
    }

    fn dynamic(role: &str, t: &str) -> Ref {
        Ref::Dynamic {
            role: role.into(),
            text: t.into(),
        }
    }

    #[test]
    fn query_compiles() {
        query().unwrap();
    }

    #[test]
    fn gdscript_forms() {
        let src = r#"class_name Hero extends "res://actor.gd"
const Weapon := preload("res://weapon.gd")
const Local = preload('sibling.gd')
enum State { IDLE, RUN }
var a: Enemy = load("uid://abc123")
var b := load(path)
var c: Array[Item] = []
var d = ResourceLoader.load("res://d.tres")
var e = "res://levels/" + name
var f = "res://levels/%s.tscn" % name
var g = "res://tests/t.gd"
var h = r'res://raw.gd'
var v: Vector3
func run(p: Boss, Q: int = 3) -> Hero:
    var x = Enemy.new()
    Events.score += 1
    var s = State.IDLE
    if x is Boss: pass
    p.Member.Inner()
    return Weapon.new()
class Inner extends Item:
    pass
"#;
        assert_eq!(
            refs(src),
            vec![
                (1, Ref::DeclareClass("Hero".into())),
                (1, path("extends", "res://actor.gd", PathKind::Extends)),
                (2, path("preload", "res://weapon.gd", PathKind::Direct)),
                (3, path("preload", "sibling.gd", PathKind::Direct)),
                (5, path("load", "uid://abc123", PathKind::Direct)),
                (5, name("Enemy", false)),
                (6, dynamic("load", "path")),
                (7, name("Item", false)),
                (8, path("load", "res://d.tres", PathKind::Direct)),
                (9, dynamic("string", r#""res://levels/" + name"#)),
                (10, dynamic("string", r#""res://levels/%s.tscn" % name"#)),
                (11, path("string", "res://tests/t.gd", PathKind::String)),
                (12, path("string", "res://raw.gd", PathKind::String)),
                (14, name("Boss", false)),
                (16, name("Events", false)),
                (21, name("Item", true)),
            ]
        );
    }

    #[test]
    fn extends_forms() {
        assert_eq!(refs("extends Node2D\n"), vec![(1, name("Node2D", true))]);
        assert_eq!(
            refs("extends \"res://a.gd\".Inner\n"),
            vec![(1, path("extends", "res://a.gd", PathKind::Extends))]
        );
        assert_eq!(refs("extends Base.Inner\n"), vec![(1, name("Base", true))]);
    }

    #[test]
    fn crlf_keeps_lines() {
        assert_eq!(
            refs("extends Node\r\n\r\nconst A = preload(\"res://a.gd\")\r\n"),
            vec![
                (1, name("Node", true)),
                (3, path("preload", "res://a.gd", PathKind::Direct))
            ]
        );
    }

    #[test]
    fn swallowed_references_are_reported() {
        let got = extract(b"extends Node\nfunc f(:\n    var x = preload(\"res://a.gd\"\n");
        assert!(
            got.iter().any(|r| matches!(r.spec, Spec::ParseError(_))
                || matches!(&r.spec, Spec::Godot(Ref::Path { path, .. }) if path == "res://a.gd")),
            "{got:?}"
        );
    }

    #[test]
    fn comparisons_with_a_path_are_references() {
        assert_eq!(
            refs(
                "var x = scene_file_path == \"res://levels/one.tscn\"
"
            ),
            vec![(1, path("string", "res://levels/one.tscn", PathKind::String))]
        );
        assert_eq!(
            refs(
                "var x = \"res://a.gd\" in paths and p != \"res://b.gd\"
"
            ),
            vec![
                (1, path("string", "res://a.gd", PathKind::String)),
                (1, path("string", "res://b.gd", PathKind::String)),
            ]
        );
    }

    #[test]
    fn keyword_scan_respects_identifier_boundaries() {
        // `reload(` / `upload(` / `extends_list(` inside a syntax error are not references.
        assert_eq!(
            refs(
                "extends Node
func f(:
    reload(\"a\")
    upload(x)
"
            ),
            vec![(1, name("Node", true))]
        );
        assert_eq!(
            refs(
                "class X extends Y:
  var = = 1
  var q = extends_list(
"
            ),
            vec![(1, name("Y", true))]
        );
    }

    #[test]
    fn extracted_loads_inside_errors_are_not_reported_twice() {
        assert_eq!(
            refs(
                "extends Node
var x := [
  preload(\"res://a.gd\"),
  )
"
            ),
            vec![
                (1, name("Node", true)),
                (3, path("preload", "res://a.gd", PathKind::Direct)),
            ]
        );
    }

    #[test]
    fn unquote_forms() {
        assert_eq!(unquote("\"a\\\"b\"").as_deref(), Some("a\"b"));
        assert_eq!(unquote("'''x'''").as_deref(), Some("x"));
        assert_eq!(unquote("r\"a\\b\"").as_deref(), Some("a\\b"));
        assert_eq!(unquote("\""), None);
        assert_eq!(unquote("abc"), None);
    }
}
