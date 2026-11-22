//! Reading the symbol query's captures, language by language: containers (which class,
//! impl or trait a function sits in), visibility, receivers (`this`, `self`, a Go
//! receiver), import bindings and flow entry points.
//!
//! Definitions nested in a function body (local functions, closures, classes declared in a
//! function) are left out: they are not reachable by name from elsewhere.

use super::{
    BindKind, Binding, Call, Code, Def, FieldType, Form, RawEntry, ScriptExtends, Super, TraitImpl,
};
use crate::grammars::Grammar;
use cartograph_core::{EntryKind, SymbolKind};
use std::collections::{HashMap, HashSet};
use tree_sitter::{Node, QueryCursor, StreamingIterator, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Js,
    Python,
    Rust,
    Go,
    Java,
    CSharp,
    C,
    Cpp,
    Gd,
}

fn lang_of(g: Grammar) -> Lang {
    match g {
        Grammar::TypeScript | Grammar::Tsx | Grammar::JavaScript => Lang::Js,
        Grammar::Python => Lang::Python,
        Grammar::Rust => Lang::Rust,
        Grammar::Go => Lang::Go,
        Grammar::Java => Lang::Java,
        Grammar::CSharp => Lang::CSharp,
        Grammar::C => Lang::C,
        Grammar::Cpp => Lang::Cpp,
        Grammar::GdScript => Lang::Gd,
    }
}

/// Node kinds whose body makes a definition local.
fn function_kinds(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Js => &[
            "function_declaration",
            "generator_function_declaration",
            "function_expression",
            "function",
            "generator_function",
            "arrow_function",
            "method_definition",
            "object",
        ],
        Lang::Python | Lang::Gd => &["function_definition", "lambda"],
        Lang::Rust => &["function_item", "closure_expression"],
        Lang::Go => &["function_declaration", "method_declaration", "func_literal"],
        Lang::Java => &[
            "method_declaration",
            "constructor_declaration",
            "lambda_expression",
            "object_creation_expression",
            "static_initializer",
        ],
        Lang::CSharp => &[
            "method_declaration",
            "constructor_declaration",
            "local_function_statement",
            "lambda_expression",
            "anonymous_method_expression",
            "accessor_declaration",
            "destructor_declaration",
            "operator_declaration",
        ],
        Lang::C | Lang::Cpp => &["function_definition", "lambda_expression"],
    }
}

/// Node kinds that hold methods.
fn class_kinds(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Js => &[
            "class_declaration",
            "class",
            "abstract_class_declaration",
            "interface_declaration",
        ],
        Lang::Python => &["class_definition"],
        Lang::Gd => &["class_definition"],
        Lang::Rust => &["impl_item", "trait_item"],
        Lang::Go => &[],
        Lang::Java => &[
            "class_declaration",
            "interface_declaration",
            "enum_declaration",
            "record_declaration",
            "annotation_type_declaration",
        ],
        Lang::CSharp => &[
            "class_declaration",
            "interface_declaration",
            "struct_declaration",
            "record_declaration",
            "record_struct_declaration",
            "enum_declaration",
        ],
        Lang::C => &[],
        Lang::Cpp => &["class_specifier", "struct_specifier", "union_specifier"],
    }
}

fn text(n: Node<'_>, src: &[u8]) -> String {
    String::from_utf8_lossy(src.get(n.byte_range()).unwrap_or(&[])).into_owned()
}

fn line(n: Node<'_>) -> u32 {
    u32::try_from(n.start_position().row + 1).unwrap_or(u32::MAX)
}

fn end_line(n: Node<'_>) -> u32 {
    u32::try_from(n.end_position().row + 1).unwrap_or(u32::MAX)
}

/// `T<U>` → `T`, `a::b::T` → `T`, `*T` → `T`.
fn base_name(t: &str) -> String {
    let t = t.split('<').next().unwrap_or(t);
    let t = t.split('[').next().unwrap_or(t);
    let t = t.rsplit("::").next().unwrap_or(t);
    let t = t.rsplit('.').next().unwrap_or(t);
    t.trim_start_matches(['*', '&'])
        .trim_start_matches("mut ")
        .trim()
        .to_string()
}

fn is_upper_snake(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn starts_upper(s: &str) -> bool {
    s.chars().next().is_some_and(char::is_uppercase)
}

/// The names a Python module lists in `__all__ = [...]` (or `(...)`, and `+=`) as
/// module-level statements of their own. `None` when there is none, or when `__all__` is
/// built in any other way (`.append()`, a computed value, a non-literal item, or any
/// assignment or mutation inside an `if`, a `try` or a loop, or in a function or class
/// that reaches the module's `__all__`; see [`scope_writes_all`]): its contents are then
/// unknown. Reading it anywhere is fine.
fn python_all(root: Node<'_>, src: &[u8]) -> Option<HashSet<String>> {
    let mut out: Option<HashSet<String>> = None;
    let mut c = root.walk();
    for stmt in root.named_children(&mut c) {
        if !text(stmt, src).contains("__all__") {
            continue;
        }
        let Some(a) = plain_all_assignment(stmt, src) else {
            if writes_all(stmt, src) {
                return None;
            }
            continue;
        };
        let right = a
            .child_by_field_name("right")
            .filter(|r| matches!(r.kind(), "list" | "tuple"))?;
        let set = out.get_or_insert_with(HashSet::new);
        let mut c2 = right.walk();
        for n in right.named_children(&mut c2) {
            if n.kind() == "comment" {
                continue;
            }
            set.insert(string_value(n, src)?);
        }
    }
    out
}

/// `__all__ = …` or `__all__ += …` as a module-level statement of its own: the assignment.
fn plain_all_assignment<'t>(stmt: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    if stmt.kind() != "expression_statement" {
        return None;
    }
    let a = stmt.named_child(0)?;
    let left = a.child_by_field_name("left");
    (matches!(a.kind(), "assignment" | "augmented_assignment")
        && left.is_some_and(|l| l.kind() == "identifier" && text(l, src) == "__all__"))
    .then_some(a)
}

/// Whether module-level statement `n` assigns, rebinds or may mutate the module's
/// `__all__` anywhere inside it (functions and classes in it: see [`scope_writes_all`]).
fn writes_all(n: Node<'_>, src: &[u8]) -> bool {
    let (ids, scopes) = own_all_ids(n, src);
    ids.into_iter().any(all_written) || scopes.into_iter().any(|s| scope_writes_all(s, src))
}

/// Whether a `def` or `class` writes the module's `__all__`: in its own scope, any write
/// when it declares `global __all__`; otherwise only a mutation (`__all__.append(…)`,
/// `__all__[0] = …`, `del __all__[0]`) of a name it does not bind itself (an assignment
/// there makes a local, or a class attribute). Nested functions and classes are judged the
/// same way.
fn scope_writes_all(def: Node<'_>, src: &[u8]) -> bool {
    let Some(body) = def.child_by_field_name("body") else {
        return false;
    };
    let (ids, scopes) = own_all_ids(body, src);
    let declared = |kind: &str| {
        ids.iter()
            .any(|id| id.parent().is_some_and(|p| p.kind() == kind))
    };
    // `__all__.append(…)`, `__all__[0] = …`, `del __all__[i]`: the module's list changes
    // (no local is bound). Reading `__all__[0]` is not one.
    let mutation = |id: &Node<'_>| {
        id.parent().is_some_and(|p| match p.kind() {
            "attribute" => p.child_by_field_name("object") == Some(*id),
            "subscript" => p.child_by_field_name("value") == Some(*id) && is_target(p),
            _ => false,
        })
    };
    let writes = if declared("global_statement") {
        ids.iter().any(|id| {
            id.parent().is_some_and(|p| p.kind() != "global_statement") && all_written(*id)
        })
    } else {
        let local = declared("nonlocal_statement")
            || ids.iter().any(|id| all_written(*id) && !mutation(id));
        !local && ids.iter().any(mutation)
    };
    writes || scopes.into_iter().any(|s| scope_writes_all(s, src))
}

/// The `__all__` identifiers in `n` outside nested functions and classes, and those nested
/// functions and classes.
fn own_all_ids<'t>(n: Node<'t>, src: &[u8]) -> (Vec<Node<'t>>, Vec<Node<'t>>) {
    let (mut ids, mut scopes) = (Vec::new(), Vec::new());
    let mut stack = vec![n];
    while let Some(x) = stack.pop() {
        match x.kind() {
            "function_definition" | "class_definition" => {
                scopes.push(x);
                continue;
            }
            "identifier" => {
                if text(x, src) == "__all__" {
                    ids.push(x);
                }
                continue;
            }
            _ => {}
        }
        let mut c = x.walk();
        stack.extend(x.named_children(&mut c));
    }
    (ids, scopes)
}

/// Whether this `__all__` identifier is written: an assignment, `for` or `with` target,
/// `del`, `global`/`nonlocal` (a function rebinding it), `:=`, an item or slice
/// assignment, or a method call on it (`.append()`, `.extend()`: it may change it).
fn all_written(id: Node<'_>) -> bool {
    let Some(p) = id.parent() else {
        return false;
    };
    // `from m import __all__` binds it to another module's list.
    let mut up = Some(p);
    while let Some(a) = up {
        if matches!(a.kind(), "import_from_statement" | "import_statement") {
            return true;
        }
        up = a.parent();
    }
    match p.kind() {
        "attribute" => p.child_by_field_name("object") == Some(id),
        "subscript" => p.child_by_field_name("value") == Some(id) && is_target(p),
        "global_statement" | "nonlocal_statement" => true,
        "named_expression" => p.child_by_field_name("name") == Some(id),
        _ => is_target(id),
    }
}

/// Whether `n` is (part of) what an assignment, a `for`, a `with … as` or a `del` writes.
fn is_target(n: Node<'_>) -> bool {
    let mut cur = n;
    while let Some(p) = cur.parent() {
        match p.kind() {
            "pattern_list"
            | "tuple_pattern"
            | "list_pattern"
            | "list_splat_pattern"
            | "parenthesized_expression"
            | "expression_list"
            | "tuple"
            | "list" => cur = p,
            "assignment" | "augmented_assignment" | "for_statement" | "for_in_clause" => {
                return p.child_by_field_name("left") == Some(cur);
            }
            "delete_statement" | "as_pattern_target" => return true,
            _ => return false,
        }
    }
    false
}

/// Value of a string literal node (any language), without quotes; `None` if it is not a
/// plain literal.
fn string_value(n: Node<'_>, src: &[u8]) -> Option<String> {
    let k = n.kind();
    if !(k.contains("string") || k == "template_string") {
        return None;
    }
    let t = text(n, src);
    if t.contains("${") {
        return None;
    }
    let t = t.trim();
    let t = t
        .trim_start_matches(['r', 'b', 'f', 'u', 'R', 'B', 'F', 'U', '@', '$'])
        .trim_start_matches('#');
    let t = t.trim_matches(|c| matches!(c, '"' | '\'' | '`' | '#'));
    Some(t.to_string())
}

/// First named child of `args` that is not a comment.
fn first_arg(args: Node<'_>) -> Option<Node<'_>> {
    let mut c = args.walk();
    args.named_children(&mut c)
        .find(|n| !n.kind().contains("comment"))
}

fn named_args(args: Node<'_>) -> Vec<Node<'_>> {
    let mut c = args.walk();
    args.named_children(&mut c)
        .filter(|n| !n.kind().contains("comment"))
        .collect()
}

/// Where a definition sits.
enum Scope {
    Top,
    Member(String),
    Local,
}

fn class_name(lang: Lang, n: Node<'_>, src: &[u8]) -> Option<String> {
    if lang == Lang::Rust && n.kind() == "impl_item" {
        return n
            .child_by_field_name("type")
            .map(|t| base_name(&text(t, src)));
    }
    n.child_by_field_name("name")
        .map(|t| base_name(&text(t, src)))
}

fn scope_of(lang: Lang, node: Node<'_>, src: &[u8]) -> Scope {
    let (funcs, classes) = (function_kinds(lang), class_kinds(lang));
    let mut cur = node.parent();
    while let Some(p) = cur {
        let k = p.kind();
        if funcs.contains(&k) {
            return Scope::Local;
        }
        if classes.contains(&k) {
            return match class_name(lang, p, src) {
                Some(n) if !n.is_empty() => Scope::Member(n),
                _ => Scope::Local,
            };
        }
        cur = p.parent();
    }
    Scope::Top
}

/// Name (and class, for `Foo::bar`) of a C/C++ function declarator.
fn c_decl_name(n: Node<'_>, src: &[u8]) -> Option<(Option<String>, String)> {
    match n.kind() {
        "identifier" | "field_identifier" => Some((None, text(n, src))),
        "qualified_identifier" => {
            let name = n.child_by_field_name("name")?;
            let (inner, leaf) = c_decl_name(name, src)?;
            let scope = n
                .child_by_field_name("scope")
                .map(|s| base_name(&text(s, src)));
            Some((inner.or(scope), leaf))
        }
        "destructor_name" | "operator_name" | "operator_cast" => None,
        _ => {
            let inner = n
                .child_by_field_name("declarator")
                .or_else(|| n.named_child(0))?;
            c_decl_name(inner, src)
        }
    }
}

fn has_child_kind(n: Node<'_>, kinds: &[&str]) -> bool {
    let mut c = n.walk();
    n.children(&mut c).any(|ch| kinds.contains(&ch.kind()))
}

fn has_modifier(n: Node<'_>, src: &[u8], words: &[&str]) -> bool {
    let mut c = n.walk();
    for ch in n.children(&mut c) {
        let k = ch.kind();
        if k == "modifiers" {
            let mut c2 = ch.walk();
            if ch
                .children(&mut c2)
                .any(|m| words.contains(&text(m, src).as_str()))
            {
                return true;
            }
        } else if k == "modifier" && words.contains(&text(ch, src).as_str()) {
            return true;
        }
    }
    false
}

/// Whether a JS/TS node is directly exported (`export function`, `export const x`).
fn js_exported(n: Node<'_>) -> bool {
    let mut cur = n.parent();
    for _ in 0..3 {
        match cur {
            Some(p) if p.kind() == "export_statement" => return true,
            Some(p) if matches!(p.kind(), "lexical_declaration" | "variable_declaration") => {
                cur = p.parent();
            }
            _ => return false,
        }
    }
    false
}

/// One definition capture.
struct DefCap<'t> {
    cap: String,
    node: Node<'t>,
    name: Option<Node<'t>>,
    decl: Option<Node<'t>>,
    recv_name: Option<Node<'t>>,
    recv_type: Option<Node<'t>>,
}

struct CallCap<'t> {
    cap: String,
    node: Node<'t>,
    name: Node<'t>,
    recv: Option<Node<'t>>,
    args: Option<Node<'t>>,
}

/// Extract the definitions, calls, bindings and entry points of a parsed file.
pub fn extract(grammar: Grammar, tree: &Tree, src: &[u8]) -> Result<Code, String> {
    let lang = lang_of(grammar);
    let query = super::query(grammar)?;
    let names = query.capture_names();

    let mut defs: Vec<DefCap<'_>> = Vec::new();
    let mut calls: HashMap<usize, CallCap<'_>> = HashMap::new();
    let mut imports: Vec<Node<'_>> = Vec::new();
    let mut exports: Vec<Node<'_>> = Vec::new();
    let mut decorators: Vec<Node<'_>> = Vec::new();
    let mut attributes: Vec<Node<'_>> = Vec::new();
    let mut mains: Vec<(Node<'_>, Node<'_>)> = Vec::new();
    let mut cobras: Vec<Node<'_>> = Vec::new();
    let mut seen: HashSet<(usize, String)> = HashSet::new();

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src);
    while let Some(m) = matches.next() {
        let mut main: Option<(&str, Node<'_>)> = None;
        let (mut name, mut recv, mut args, mut decl, mut rname, mut rtype, mut cond) =
            (None, None, None, None, None, None, None);
        for c in m.captures() {
            let cap = names.get(c.index as usize).copied().unwrap_or("");
            match cap {
                "name" => name = Some(c.node),
                "recv" => recv = Some(c.node),
                "args" => args = Some(c.node),
                "decl" => decl = Some(c.node),
                "recv.name" => rname = Some(c.node),
                "recv.type" => rtype = Some(c.node),
                "cond" => cond = Some(c.node),
                c2 if c2.starts_with('_') || c2.is_empty() => {}
                other => main = Some((other, c.node)),
            }
        }
        let Some((cap, node)) = main else { continue };
        if cap.starts_with("call") {
            let Some(name) = name else { continue };
            let entry = calls.entry(name.id()).or_insert(CallCap {
                cap: cap.to_string(),
                node,
                name,
                recv,
                args,
            });
            // Several patterns can hit one call: keep the most specific.
            if entry.recv.is_none() && recv.is_some() {
                entry.recv = recv;
                entry.cap = cap.to_string();
            }
            if entry.args.is_none() {
                entry.args = args;
            }
            continue;
        }
        if !seen.insert((node.id(), cap.to_string())) {
            continue;
        }
        match cap {
            "import" => imports.push(node),
            "export" => exports.push(node),
            "decorator" => decorators.push(node),
            "attribute" => attributes.push(node),
            "cobra" => cobras.push(node),
            "entry.main" => {
                if let Some(c) = cond {
                    mains.push((node, c));
                }
            }
            c if c.starts_with("def.") => defs.push(DefCap {
                cap: c.to_string(),
                node,
                name,
                decl,
                recv_name: rname,
                recv_type: rtype,
            }),
            _ => {}
        }
    }

    let mut code = Code {
        module: lang == Lang::Js && (!imports.is_empty() || !exports.is_empty()),
        ..Code::default()
    };
    // `require` bindings do not make a file an ES module.
    if lang == Lang::Js
        && exports.is_empty()
        && imports.iter().all(|n| n.kind() == "variable_declarator")
    {
        code.module = false;
    }
    if lang == Lang::Go {
        let root = tree.root_node();
        let mut c = root.walk();
        code.package_main = root.children(&mut c).any(|ch| {
            ch.kind() == "package_clause" && text(ch, src).split_whitespace().nth(1) == Some("main")
        });
    }

    // Export lists (`export { a }`, `export default a`) before definitions read them.
    let mut listed_exports: HashSet<String> = HashSet::new();
    for e in &exports {
        js_export(*e, src, &mut listed_exports, &mut code.default_export);
    }

    defs.sort_by_key(|d| (d.node.start_byte(), d.node.id()));
    let mut def_nodes: Vec<(usize, Node<'_>)> = Vec::new();
    for d in &defs {
        if let Some(def) = definition(lang, d, src, tree, &code, &listed_exports) {
            def_nodes.push((code.defs.len(), d.node));
            code.defs.push(def);
        }
    }
    // Python `__all__`: the module exports what it lists, `_names` included.
    if lang == Lang::Python
        && let Some(all) = python_all(tree.root_node(), src)
    {
        for d in code.defs.iter_mut().filter(|d| d.container.is_none()) {
            d.exported = all.contains(&d.name);
        }
        let mut names: Vec<String> = all.into_iter().collect();
        names.sort_unstable();
        code.all = Some(names);
    }
    // Methods of a private class, or private methods, are not exported.
    if matches!(lang, Lang::Js | Lang::Python | Lang::Gd) {
        let types: HashMap<String, bool> = code
            .defs
            .iter()
            .filter(|d| d.kind.is_type() && d.container.is_none())
            .map(|d| (d.name.clone(), d.exported))
            .collect();
        for d in &mut code.defs {
            if let Some(c) = &d.container
                && let Some(&exp) = types.get(c)
            {
                d.exported &= exp;
            }
        }
    }
    // One definition per (container, name, line): `typedef struct S {…} S;` and patterns
    // that match twice.
    let mut uniq = HashSet::new();
    code.defs
        .retain(|d| uniq.insert((d.container.clone(), d.name.clone(), d.line)));

    let mut call_list: Vec<CallCap<'_>> = calls.into_values().collect();
    call_list.sort_by_key(|c| (c.name.start_byte(), c.name.id()));
    for c in &call_list {
        if let Some(call) = call_of(lang, c, src) {
            code.calls.push(call);
        }
    }

    for n in &imports {
        bindings(lang, *n, src, &mut code.bindings);
    }
    code.fields = field_types(lang, tree.root_node(), src);
    match lang {
        Lang::Rust => code.impls = trait_impls(tree.root_node(), src),
        Lang::Gd => code.extends = script_extends(tree.root_node(), src),
        _ => {}
    }

    entries(
        lang,
        src,
        &code_defs_index(&code),
        &call_list,
        &decorators,
        &attributes,
        &mains,
        &cobras,
        &defs,
        &mut code,
    );
    code.entries
        .sort_by(|a, b| (a.line, &a.name).cmp(&(b.line, &b.name)));
    code.entries.dedup();
    Ok(code)
}

/// `(container, name)` → qualified name, for entry handlers defined in the file.
fn code_defs_index(code: &Code) -> HashSet<String> {
    code.defs.iter().map(Def::qualified).collect()
}

fn js_export(e: Node<'_>, src: &[u8], listed: &mut HashSet<String>, default: &mut Option<String>) {
    if e.child_by_field_name("source").is_some() {
        return;
    }
    let mut c = e.walk();
    let is_default = e.children(&mut c).any(|ch| ch.kind() == "default");
    if let Some(v) = e.child_by_field_name("value")
        && v.kind() == "identifier"
    {
        let name = text(v, src);
        listed.insert(name.clone());
        if is_default {
            *default = Some(name);
        }
    }
    if let Some(d) = e.child_by_field_name("declaration")
        && is_default
        && let Some(n) = d.child_by_field_name("name")
    {
        *default = Some(text(n, src));
    }
    let mut c = e.walk();
    for ch in e.named_children(&mut c) {
        if ch.kind() == "export_clause" {
            let mut c2 = ch.walk();
            for spec in ch.named_children(&mut c2) {
                if let Some(n) = spec.child_by_field_name("name") {
                    listed.insert(text(n, src));
                    let alias = spec.child_by_field_name("alias").map(|a| text(a, src));
                    if alias.as_deref() == Some("default") {
                        *default = Some(text(n, src));
                    }
                }
            }
        }
    }
}

fn definition(
    lang: Lang,
    d: &DefCap<'_>,
    src: &[u8],
    tree: &Tree,
    code: &Code,
    listed: &HashSet<String>,
) -> Option<Def> {
    // A JS variable is a symbol only at module level: skip the (many) local ones before the
    // parent walk.
    if d.cap == "def.var" {
        let decl = d.node.parent()?;
        if !matches!(decl.kind(), "lexical_declaration" | "variable_declaration")
            || !decl
                .parent()
                .is_some_and(|p| matches!(p.kind(), "program" | "export_statement"))
        {
            return None;
        }
    }
    let scope = scope_of(lang, d.node, src);
    let mut container = match scope {
        Scope::Local => return None,
        Scope::Top => None,
        Scope::Member(c) => Some(c),
    };
    let (name, name_node) = match (d.decl, d.name) {
        (Some(decl), _) => {
            let (owner, name) = c_decl_name(decl, src)?;
            if owner.is_some() {
                container = owner;
            }
            (name, decl)
        }
        (None, Some(n)) => (text(n, src), n),
        (None, None) => return None,
    };
    if name.is_empty() {
        return None;
    }
    let node = d.node;
    let mut receiver = None;
    let kind = match d.cap.as_str() {
        "def.function" => {
            if container.is_some() {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            }
        }
        "def.method" => {
            if lang == Lang::Go {
                container = d.recv_type.map(|t| base_name(&text(t, src)));
                receiver = d.recv_name.map(|n| text(n, src));
            }
            if container.is_some() {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            }
        }
        "def.class" | "def.class_name" => SymbolKind::Class,
        "def.interface" => SymbolKind::Interface,
        "def.struct" => SymbolKind::Struct,
        "def.enum" => SymbolKind::Enum,
        "def.trait" => SymbolKind::Trait,
        "def.type" => SymbolKind::Type,
        "def.typespec" => match node.child_by_field_name("type").map(|t| t.kind()) {
            Some("struct_type") => SymbolKind::Struct,
            Some("interface_type") => SymbolKind::Interface,
            _ => SymbolKind::Type,
        },
        "def.const" => {
            if lang == Lang::Python && !is_upper_snake(&name) {
                return None;
            }
            SymbolKind::Const
        }
        "def.var" => {
            let value = node.child_by_field_name("value")?;
            match value.kind() {
                "arrow_function" | "function_expression" | "function" | "generator_function" => {
                    SymbolKind::Function
                }
                "class" => SymbolKind::Class,
                // `const Panel = forwardRef((props, ref) => …)`, `memo(function …)`,
                // `const save = debounce(() => …)`: the wrapped function is the symbol.
                "call_expression" if wraps_function(value, &name, src) => SymbolKind::Function,
                _ => {
                    let decl = node.parent()?;
                    let is_const = decl.kind() == "lexical_declaration"
                        && decl.child(0).is_some_and(|k| k.kind() == "const");
                    if container.is_some()
                        || !is_const
                        || !(is_upper_snake(&name) || js_exported(node))
                    {
                        return None;
                    }
                    SymbolKind::Const
                }
            }
        }
        _ => return None,
    };
    if d.cap == "def.var"
        && !node
            .parent()
            .is_some_and(|p| matches!(p.kind(), "lexical_declaration" | "variable_declaration"))
    {
        return None;
    }
    let (start, end) = if d.cap == "def.class_name" {
        (1, end_line(tree.root_node()))
    } else {
        (line(node), end_line(node))
    };
    let exported = exported(lang, node, &name, &container, kind, src, code, listed);
    let overrides = kind == SymbolKind::Method
        && match lang {
            Lang::Rust => node.parent().and_then(|p| p.parent()).is_some_and(|g| {
                g.kind() == "impl_item" && g.child_by_field_name("trait").is_some()
            }),
            Lang::Java => java_annotations(node, src)
                .iter()
                .any(|(n, _, _)| n == "Override"),
            Lang::CSharp => has_modifier(node, src, &["override"]),
            Lang::Js => {
                let mut c = node.walk();
                node.children(&mut c)
                    .any(|ch| ch.kind() == "override_modifier")
            }
            _ => false,
        };
    let supers = if kind.is_type() && d.cap != "def.class_name" {
        supers_of(lang, node, src)
    } else {
        Vec::new()
    };
    let declares = kind == SymbolKind::Class
        && lang == Lang::Js
        && (node.kind() == "abstract_class_declaration"
            || std::iter::successors(node.parent(), |p| p.parent())
                .any(|p| p.kind() == "ambient_declaration"));
    Some(Def {
        line: line(name_node),
        name,
        kind,
        container,
        start,
        end,
        exported,
        receiver,
        overrides,
        declares,
        supers,
        class_name: d.cap == "def.class_name",
    })
}

/// A supertype as written: whitespace and generic arguments removed (`Base<T>` -> `Base`,
/// `Generic[T]` -> `Generic`, `*Base` -> `Base`). Anything that is not a dotted or `::` path of
/// names (`mixin(Base)`) is kept, clipped, and never resolves.
fn super_name(n: Node<'_>, src: &[u8]) -> String {
    let t: String = text(n, src)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let t = t.trim_start_matches(['*', '&']);
    let t = t.split(['<', '[']).next().unwrap_or(t);
    let t = t.strip_prefix("global::").unwrap_or(t);
    t.chars().take(120).collect()
}

/// Python bases that add no method a subclass would call through `self`.
const PY_PLAIN_BASES: &[&str] = &[
    "object",
    "ABC",
    "abc.ABC",
    "Generic",
    "typing.Generic",
    "Protocol",
    "typing.Protocol",
];

fn named_children(n: Node<'_>) -> Vec<Node<'_>> {
    let mut c = n.walk();
    n.named_children(&mut c)
        .filter(|x| !x.kind().contains("comment"))
        .collect()
}

/// The supertypes a class, struct, interface or trait definition declares, in order (see
/// [`Super`]). Rust trait impls and a GDScript script's own `extends` are read elsewhere.
fn supers_of(lang: Lang, node: Node<'_>, src: &[u8]) -> Vec<Super> {
    let mut out = Vec::new();
    let mut push = |n: Node<'_>, iface: bool| {
        let name = super_name(n, src);
        if !name.is_empty() {
            out.push(Super { name, iface });
        }
    };
    match lang {
        Lang::Js => {
            for ch in named_children(node) {
                match ch.kind() {
                    // TS: `extends_clause` / `implements_clause`; JS: the expression itself.
                    "class_heritage" => {
                        for h in named_children(ch) {
                            match h.kind() {
                                "extends_clause" => {
                                    let mut c = h.walk();
                                    for v in h.children_by_field_name("value", &mut c) {
                                        push(v, false);
                                    }
                                }
                                "implements_clause" => {
                                    for t in named_children(h) {
                                        push(t, true);
                                    }
                                }
                                _ => push(h, false),
                            }
                        }
                    }
                    "extends_type_clause" => {
                        let mut c = ch.walk();
                        for t in ch.children_by_field_name("type", &mut c) {
                            push(t, true);
                        }
                    }
                    _ => {}
                }
            }
        }
        Lang::Java => {
            for ch in named_children(node) {
                match ch.kind() {
                    "superclass" => {
                        if let Some(t) = named_children(ch).into_iter().next() {
                            push(t, false);
                        }
                    }
                    "super_interfaces" | "extends_interfaces" => {
                        for list in named_children(ch) {
                            for t in named_children(list) {
                                push(t, true);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Lang::CSharp => {
            for ch in named_children(node) {
                if ch.kind() != "base_list" {
                    continue;
                }
                for t in named_children(ch) {
                    let t = match t.kind() {
                        "argument_list" => continue,
                        "primary_constructor_base_type" => match t.child_by_field_name("type") {
                            Some(x) => x,
                            None => continue,
                        },
                        _ => t,
                    };
                    // `IName`: an interface by the .NET naming rule.
                    let name = super_name(t, src);
                    let last = name.rsplit('.').next().unwrap_or(&name);
                    let mut cs = last.chars();
                    let iface = cs.next() == Some('I') && cs.next().is_some_and(char::is_uppercase);
                    push(t, iface);
                }
            }
        }
        Lang::Python => {
            if let Some(args) = node.child_by_field_name("superclasses") {
                for a in named_children(args) {
                    if matches!(
                        a.kind(),
                        "keyword_argument" | "list_splat" | "dictionary_splat"
                    ) {
                        continue;
                    }
                    let plain = PY_PLAIN_BASES.contains(&super_name(a, src).as_str());
                    push(a, plain);
                }
            }
        }
        Lang::Cpp => {
            for ch in named_children(node) {
                if ch.kind() == "base_class_clause" {
                    for t in named_children(ch) {
                        if matches!(
                            t.kind(),
                            "type_identifier" | "qualified_identifier" | "template_type"
                        ) {
                            push(t, false);
                        }
                    }
                }
            }
        }
        Lang::Rust => {
            if let Some(b) = node.child_by_field_name("bounds") {
                for t in named_children(b) {
                    if matches!(
                        t.kind(),
                        "type_identifier" | "scoped_type_identifier" | "generic_type"
                    ) {
                        push(t, true);
                    }
                }
            }
        }
        // Embedded types: an anonymous struct field, an interface's type element.
        Lang::Go => match node.child_by_field_name("type") {
            Some(t) if t.kind() == "struct_type" => {
                for list in named_children(t) {
                    for f in named_children(list) {
                        if f.kind() == "field_declaration"
                            && f.child_by_field_name("name").is_none()
                            && let Some(ty) = f.child_by_field_name("type")
                        {
                            push(ty, false);
                        }
                    }
                }
            }
            Some(t) if t.kind() == "interface_type" => {
                for e in named_children(t) {
                    if e.kind() == "type_elem" {
                        let parts = named_children(e);
                        // `A | B` is a type set (a constraint), not an embedding.
                        if parts.len() == 1 {
                            push(parts[0], true);
                        }
                    }
                }
            }
            _ => {}
        },
        Lang::Gd => {
            if let Some(e) = node.child_by_field_name("extends")
                && let Some(t) = named_children(e).into_iter().next()
            {
                push(t, false);
            }
        }
        Lang::C => {}
    }
    out
}

/// Rust `impl Trait for Type` blocks of a file, outside function bodies. Blanket impls
/// (`impl<T: X> Tr for T`) and impls for references, tuples or slices are left out.
fn trait_impls(root: Node<'_>, src: &[u8]) -> Vec<TraitImpl> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if function_kinds(Lang::Rust).contains(&n.kind()) {
            continue;
        }
        if n.kind() == "impl_item"
            && let Some(tr) = n.child_by_field_name("trait")
            && let Some(ty) = n.child_by_field_name("type")
            && matches!(
                ty.kind(),
                "type_identifier" | "scoped_type_identifier" | "generic_type"
            )
        {
            let params: Vec<String> = n
                .child_by_field_name("type_parameters")
                .map(|p| {
                    named_children(p)
                        .into_iter()
                        .filter_map(|x| {
                            x.child_by_field_name("name")
                                .or_else(|| (x.kind() == "type_identifier").then_some(x))
                        })
                        .map(|x| text(x, src))
                        .collect()
                })
                .unwrap_or_default();
            let ty_name = super_name(ty, src);
            if !params.contains(&ty_name) {
                out.push(TraitImpl {
                    ty: ty_name,
                    tr: super_name(tr, src),
                    line: line(n),
                });
            }
        }
        stack.extend(named_children(n));
    }
    out.sort_by(|a, b| (a.line, &a.ty, &a.tr).cmp(&(b.line, &b.ty, &b.tr)));
    out
}

/// GDScript: the script's own `extends` (a statement, or part of `class_name ... extends ...`).
fn script_extends(root: Node<'_>, src: &[u8]) -> Option<ScriptExtends> {
    for ch in named_children(root) {
        let e = match ch.kind() {
            "extends_statement" => Some(ch),
            "class_name_statement" => ch.child_by_field_name("extends"),
            _ => None,
        };
        if let Some(e) = e
            && let Some(t) = e.named_child(0)
        {
            return Some(ScriptExtends {
                name: super_name(t, src),
                line: line(e),
            });
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn exported(
    lang: Lang,
    node: Node<'_>,
    name: &str,
    container: &Option<String>,
    kind: SymbolKind,
    src: &[u8],
    code: &Code,
    listed: &HashSet<String>,
) -> bool {
    match lang {
        Lang::Js => {
            if container.is_some() {
                let private = name.starts_with('#')
                    || node
                        .children(&mut node.walk())
                        .any(|c| c.kind() == "accessibility_modifier" && text(c, src) == "private");
                return !private;
            }
            !code.module || js_exported(node) || listed.contains(name)
        }
        Lang::Python | Lang::Gd => !name.starts_with('_'),
        Lang::Rust => {
            has_child_kind(node, &["visibility_modifier"])
                || node.parent().and_then(|p| p.parent()).is_some_and(|g| {
                    g.kind() == "trait_item"
                        || (g.kind() == "impl_item" && g.child_by_field_name("trait").is_some())
                })
        }
        Lang::Go => starts_upper(name),
        Lang::Java => {
            in_interface(node)
                || has_modifier(node, src, &["public"])
                || (kind.is_type() && has_modifier(node, src, &["protected"]))
        }
        Lang::CSharp => {
            if in_interface(node) {
                return true;
            }
            if has_modifier(node, src, &["public", "internal", "protected"]) {
                return true;
            }
            // Types default to internal (visible in the assembly); members to private.
            container.is_none() && !has_modifier(node, src, &["private"])
        }
        Lang::C | Lang::Cpp => {
            if container.is_some() {
                return true;
            }
            let mut c = node.walk();
            !node
                .children(&mut c)
                .any(|ch| ch.kind() == "storage_class_specifier" && text(ch, src) == "static")
        }
    }
}

/// Wrappers whose argument is the function that a `const` names.
const JS_WRAPPERS: &[&str] = &[
    "forwardRef",
    "memo",
    "observer",
    "debounce",
    "throttle",
    "defineComponent",
    "lazy",
];

/// Whether a call (`forwardRef(…)`, `React.memo(…)`, nested wrappers) has a function as an
/// argument, and is a known wrapper or names a component (`PascalCase`).
fn wraps_function(call: Node<'_>, name: &str, src: &[u8]) -> bool {
    let mut cur = call;
    for _ in 0..3 {
        let callee = cur
            .child_by_field_name("function")
            .map(|f| text(f, src))
            .unwrap_or_default();
        let callee = callee.rsplit('.').next().unwrap_or("");
        let callee = callee.split('<').next().unwrap_or(callee);
        if !(JS_WRAPPERS.contains(&callee) || starts_upper(name)) {
            return false;
        }
        let Some(args) = cur.child_by_field_name("arguments") else {
            return false;
        };
        let list = named_args(args);
        if list.iter().any(|a| {
            matches!(
                a.kind(),
                "arrow_function" | "function_expression" | "function"
            )
        }) {
            return true;
        }
        match list.first() {
            Some(inner) if inner.kind() == "call_expression" => cur = *inner,
            _ => return false,
        }
    }
    false
}

fn in_interface(node: Node<'_>) -> bool {
    node.parent()
        .and_then(|p| p.parent())
        .is_some_and(|g| g.kind() == "interface_declaration")
}

fn call_of(lang: Lang, c: &CallCap<'_>, src: &[u8]) -> Option<Call> {
    let name = text(c.name, src);
    if name.is_empty() {
        return None;
    }
    let line = line(c.name);
    let jsx = c.node.kind().starts_with("jsx_");
    if jsx && !starts_upper(&name) && c.recv.is_none() {
        return None; // an HTML tag
    }
    let (form, qualifier) = match c.cap.as_str() {
        "call" => (Form::Plain, None),
        "call.new" => (Form::New, c.recv.map(|r| text(r, src))),
        "call.path" => path_receiver(lang, c.recv, src),
        "call.macro" => macro_call(c, src)?,
        _ => member_receiver(lang, c.recv, src),
    };
    let (path, scope) = if c.cap == "call.path" {
        rust_call_path(lang, form, c.node, c.recv, src)
    } else {
        (Vec::new(), Vec::new())
    };
    let field = match (form, c.recv) {
        (Form::Member, Some(r)) if qualifier.is_none() => self_field(lang, r, src),
        _ => None,
    };
    Some(Call {
        line,
        name,
        form,
        qualifier,
        path,
        scope,
        field,
    })
}

/// `field` of a receiver `self.field` (Rust), `this.field` / `this.#field` (TS/JS, Java,
/// C#).
fn self_field(lang: Lang, r: Node<'_>, src: &[u8]) -> Option<String> {
    let (obj, field) = match (lang, r.kind()) {
        (Lang::Rust, "field_expression") => (
            r.child_by_field_name("value")?,
            r.child_by_field_name("field")?,
        ),
        (Lang::Js, "member_expression") => (
            r.child_by_field_name("object")?,
            r.child_by_field_name("property")?,
        ),
        (Lang::Java, "field_access") => (
            r.child_by_field_name("object")?,
            r.child_by_field_name("field")?,
        ),
        (Lang::CSharp, "member_access_expression") => (
            r.child_by_field_name("expression")?,
            r.child_by_field_name("name")?,
        ),
        _ => return None,
    };
    let this = match lang {
        Lang::Rust => obj.kind() == "self",
        _ => matches!(obj.kind(), "this" | "this_expression"),
    };
    let plain = matches!(
        field.kind(),
        "field_identifier" | "property_identifier" | "private_property_identifier" | "identifier"
    );
    (this && plain).then(|| text(field, src))
}

/// Declared field types of the structs and classes of a file (see [`FieldType`]): Rust
/// struct fields, TS class fields and constructor parameter properties, Java and C# fields
/// and C# properties. Types nested in functions are skipped like their definitions.
fn field_types(lang: Lang, root: Node<'_>, src: &[u8]) -> Vec<FieldType> {
    let mut out = Vec::new();
    if !matches!(lang, Lang::Rust | Lang::Js | Lang::Java | Lang::CSharp) {
        return out;
    }
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if function_kinds(lang).contains(&n.kind()) {
            continue;
        }
        let owner = match (lang, n.kind()) {
            (Lang::Rust, "struct_item")
            | (Lang::Js, "class_declaration" | "abstract_class_declaration")
            | (Lang::Java, "class_declaration")
            | (Lang::CSharp, "class_declaration" | "record_declaration" | "struct_declaration") => {
                n.child_by_field_name("name").map(|x| text(x, src))
            }
            _ => None,
        };
        if let Some(owner) = owner
            && let Some(body) = n.child_by_field_name("body")
        {
            let mut c = body.walk();
            for member in body.named_children(&mut c) {
                for (name, ty) in member_fields(lang, member, src) {
                    out.push(FieldType {
                        container: owner.clone(),
                        name,
                        ty,
                    });
                }
            }
        }
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c));
    }
    out.sort_by(|a, b| (&a.container, &a.name).cmp(&(&b.container, &b.name)));
    out.dedup_by(|a, b| a.container == b.container && a.name == b.name);
    out
}

/// `(field, type)` of one member of a struct or class body.
fn member_fields(lang: Lang, m: Node<'_>, src: &[u8]) -> Vec<(String, String)> {
    let named = |name: Option<Node<'_>>, ty: Option<Node<'_>>| -> Vec<(String, String)> {
        match (name, ty.and_then(|t| plain_type(lang, t, src))) {
            (Some(n), Some(t)) => vec![(text(n, src), t)],
            _ => Vec::new(),
        }
    };
    match (lang, m.kind()) {
        (Lang::Rust, "field_declaration") => {
            named(m.child_by_field_name("name"), m.child_by_field_name("type"))
        }
        (Lang::Js, "public_field_definition" | "field_definition") => named(
            m.child_by_field_name("name")
                .or_else(|| m.child_by_field_name("property")),
            m.child_by_field_name("type"),
        ),
        // `constructor(private readonly guard: Guard)`.
        (Lang::Js, "method_definition")
            if m.child_by_field_name("name")
                .map(|n| text(n, src))
                .as_deref()
                == Some("constructor") =>
        {
            let Some(params) = m.child_by_field_name("parameters") else {
                return Vec::new();
            };
            let mut out = Vec::new();
            let mut c = params.walk();
            for p in params.named_children(&mut c) {
                let mut c2 = p.walk();
                let property = p
                    .children(&mut c2)
                    .any(|x| matches!(x.kind(), "accessibility_modifier" | "readonly"));
                if property
                    && p.child_by_field_name("pattern").map(|x| x.kind()) == Some("identifier")
                {
                    out.extend(named(
                        p.child_by_field_name("pattern"),
                        p.child_by_field_name("type"),
                    ));
                }
            }
            out
        }
        (Lang::Java, "field_declaration") => {
            let ty = m.child_by_field_name("type");
            let mut c = m.walk();
            m.children_by_field_name("declarator", &mut c)
                .flat_map(|d| named(d.child_by_field_name("name"), ty))
                .collect()
        }
        (Lang::CSharp, "field_declaration") => {
            let mut c = m.walk();
            let Some(decl) = m
                .named_children(&mut c)
                .find(|x| x.kind() == "variable_declaration")
            else {
                return Vec::new();
            };
            let ty = decl.child_by_field_name("type");
            let mut c2 = decl.walk();
            decl.named_children(&mut c2)
                .filter(|x| x.kind() == "variable_declarator")
                .flat_map(|d| {
                    named(
                        d.child_by_field_name("name").or_else(|| d.named_child(0)),
                        ty,
                    )
                })
                .collect()
        }
        (Lang::CSharp, "property_declaration") => {
            named(m.child_by_field_name("name"), m.child_by_field_name("type"))
        }
        _ => Vec::new(),
    }
}

/// The one type name a field type stands for (see [`FieldType`]), if it is that simple.
fn plain_type(lang: Lang, t: Node<'_>, src: &[u8]) -> Option<String> {
    match (lang, t.kind()) {
        (Lang::Js, "type_annotation") => plain_type(lang, t.named_child(0)?, src),
        (Lang::Rust | Lang::Js | Lang::Java, "type_identifier") | (Lang::CSharp, "identifier") => {
            Some(text(t, src))
        }
        (Lang::Rust, "reference_type") => plain_type(lang, t.child_by_field_name("type")?, src),
        (Lang::Rust, "generic_type") => {
            let outer = t.child_by_field_name("type")?;
            let args = t.child_by_field_name("type_arguments")?;
            if !matches!(text(outer, src).as_str(), "Box" | "Arc" | "Rc")
                || args.named_child_count() != 1
            {
                return None;
            }
            plain_type(lang, args.named_child(0)?, src)
        }
        _ => None,
    }
}

/// Rust `a::b::f()` (`form` [`Form::Path`]): the segments of `a::b` when each is a plain
/// identifier (`crate`, `super`, `self` included, and an empty first one for a leading
/// `::`), and the inline modules around `node`. Empty for anything else (`Vec::<u8>::new`,
/// `<T as Tr>::f`, raw identifiers).
fn rust_call_path(
    lang: Lang,
    form: Form,
    node: Node<'_>,
    recv: Option<Node<'_>>,
    src: &[u8],
) -> (Vec<String>, Vec<String>) {
    let none = (Vec::new(), Vec::new());
    let Some(r) = recv.filter(|_| lang == Lang::Rust && form == Form::Path) else {
        return none;
    };
    let t: String = text(r, src)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let segs: Vec<String> = t.split("::").map(str::to_string).collect();
    let ident = |s: &str| {
        s.starts_with(|c: char| c.is_alphabetic() || c == '_')
            && s.chars().all(|c| c.is_alphanumeric() || c == '_')
    };
    let ok = segs
        .iter()
        .enumerate()
        .all(|(i, s)| ident(s) || (i == 0 && s.is_empty() && segs.len() > 1));
    if !ok {
        return none;
    }
    (segs, crate::syntax::inline_scope(node, src))
}

/// A call written inside a Rust macro's arguments, where the grammar keeps raw tokens:
/// `name(…)`, `x.name(…)`, `self.name(…)`, `Type::name(…)`. `None` when the identifier is
/// not directly followed by a `(…)` group (a macro name, `vec![…]`, a struct literal).
fn macro_call(c: &CallCap<'_>, src: &[u8]) -> Option<(Form, Option<String>)> {
    let args = c.args?;
    if c.name.next_sibling().map(|n| n.id()) != Some(args.id()) || !text(args, src).starts_with('(')
    {
        return None;
    }
    let prev = c.name.prev_sibling();
    let before = prev.and_then(|p| p.prev_sibling());
    Some(match prev.map(|p| p.kind()) {
        Some(".") => match before.map(|b| (b.kind(), text(b, src))) {
            Some(("self", _)) => (Form::SelfCall, None),
            Some(("identifier", t)) => (Form::Member, Some(t)),
            _ => (Form::Member, None),
        },
        Some("::") => match before.map(|b| (b.kind(), text(b, src))) {
            Some((_, t)) if t == "Self" => (Form::SelfCall, None),
            Some((_, t)) if t == "self" => (Form::Plain, None),
            Some(("identifier", t)) => (Form::Path, Some(t)),
            _ => (Form::Path, None),
        },
        // `fn name(` inside `macro_rules!` and the like: a definition, not a call.
        Some("fn") => return None,
        Some("identifier") if prev.is_some_and(|p| text(p, src) == "fn") => return None,
        _ => (Form::Plain, None),
    })
}

fn member_receiver(lang: Lang, recv: Option<Node<'_>>, src: &[u8]) -> (Form, Option<String>) {
    let Some(r) = recv else {
        return (Form::Member, None);
    };
    match r.kind() {
        "this" | "this_expression" | "self" => (Form::SelfCall, None),
        "super" | "base" | "base_expression" => (Form::Super, None),
        "identifier" | "type_identifier" | "package_identifier" => {
            let t = text(r, src);
            match (lang, t.as_str()) {
                (Lang::Python, "self" | "cls") | (Lang::Gd, "self") => (Form::SelfCall, None),
                (Lang::Python | Lang::Gd, "super") => (Form::Super, None),
                (Lang::CSharp, "this") => (Form::SelfCall, None),
                (Lang::CSharp, "base") => (Form::Super, None),
                _ => (Form::Member, Some(t)),
            }
        }
        // Python `super().f()`, `super(Class, self).f()` (the class is the qualifier).
        "call"
            if lang == Lang::Python
                && r.child_by_field_name("function")
                    .is_some_and(|f| text(f, src) == "super") =>
        {
            let first = r
                .child_by_field_name("arguments")
                .and_then(|a| named_args(a).into_iter().next())
                .map(|a| text(a, src));
            (Form::Super, first)
        }
        _ => {
            let t = text(r, src);
            if lang == Lang::CSharp && t == "this" {
                (Form::SelfCall, None)
            } else if lang == Lang::CSharp && t == "base" {
                (Form::Super, None)
            } else {
                (Form::Member, None)
            }
        }
    }
}

fn path_receiver(lang: Lang, recv: Option<Node<'_>>, src: &[u8]) -> (Form, Option<String>) {
    let Some(r) = recv else {
        return (Form::Path, None);
    };
    let t = text(r, src);
    match (lang, t.as_str()) {
        (Lang::Rust, "Self") => (Form::SelfCall, None),
        (Lang::Rust, "self") => (Form::Plain, None),
        _ => {
            let q = base_name(&t);
            if q.is_empty() {
                (Form::Path, None)
            } else {
                (Form::Path, Some(q))
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Import bindings

fn bindings(lang: Lang, n: Node<'_>, src: &[u8], out: &mut Vec<Binding>) {
    let l = line(n);
    let mut push = |local: String, imported: Option<String>, kind, spec: Option<String>| {
        out.push(Binding {
            line: l,
            local,
            imported,
            kind,
            spec,
            container: None,
        });
    };
    match lang {
        Lang::Js => match n.kind() {
            "import_statement" | "export_statement" => {
                let spec = n
                    .child_by_field_name("source")
                    .and_then(|s| string_value(s, src));
                let mut c = n.walk();
                for ch in n.named_children(&mut c) {
                    match ch.kind() {
                        "import_clause" => {
                            let mut c2 = ch.walk();
                            for part in ch.named_children(&mut c2) {
                                match part.kind() {
                                    "identifier" => {
                                        push(text(part, src), None, BindKind::Default, spec.clone())
                                    }
                                    "namespace_import" => {
                                        if let Some(id) = part.named_child(0) {
                                            push(
                                                text(id, src),
                                                None,
                                                BindKind::Namespace,
                                                spec.clone(),
                                            );
                                        }
                                    }
                                    "named_imports" => {
                                        let mut c3 = part.walk();
                                        for s in part.named_children(&mut c3) {
                                            named_spec(s, src, &spec, &mut push);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        "export_clause" => {
                            let mut c2 = ch.walk();
                            for s in ch.named_children(&mut c2) {
                                named_spec(s, src, &spec, &mut push);
                            }
                        }
                        "namespace_export" => {
                            if let Some(id) = ch.named_child(0) {
                                push(text(id, src), None, BindKind::Namespace, spec.clone());
                            }
                        }
                        _ => {}
                    }
                }
                // `export * from './x'`.
                if n.kind() == "export_statement" {
                    let mut c = n.walk();
                    let star = n.children(&mut c).any(|ch| ch.kind() == "*");
                    let mut c = n.walk();
                    let named = n
                        .named_children(&mut c)
                        .any(|ch| matches!(ch.kind(), "export_clause" | "namespace_export"));
                    if star && !named {
                        push(String::new(), None, BindKind::Glob, spec);
                    }
                }
            }
            "variable_declarator" => {
                let spec = n
                    .child_by_field_name("value")
                    .and_then(|v| v.child_by_field_name("arguments"))
                    .and_then(first_arg)
                    .and_then(|a| string_value(a, src));
                let Some(name) = n.child_by_field_name("name") else {
                    return;
                };
                match name.kind() {
                    "identifier" => push(text(name, src), None, BindKind::Namespace, spec),
                    "object_pattern" => {
                        let mut c = name.walk();
                        for p in name.named_children(&mut c) {
                            match p.kind() {
                                "shorthand_property_identifier_pattern" => {
                                    let t = text(p, src);
                                    push(t.clone(), Some(t), BindKind::Named, spec.clone());
                                }
                                "pair_pattern" => {
                                    if let (Some(k), Some(v)) = (
                                        p.child_by_field_name("key"),
                                        p.child_by_field_name("value"),
                                    ) && v.kind() == "identifier"
                                    {
                                        push(
                                            text(v, src),
                                            Some(text(k, src)),
                                            BindKind::Named,
                                            spec.clone(),
                                        );
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        },
        Lang::Python => {
            let mut c = n.walk();
            if n.kind() == "import_statement" {
                for ch in n.named_children(&mut c) {
                    match ch.kind() {
                        "aliased_import" => {
                            if let (Some(name), Some(alias)) = (
                                ch.child_by_field_name("name"),
                                ch.child_by_field_name("alias"),
                            ) {
                                push(
                                    text(alias, src),
                                    None,
                                    BindKind::Namespace,
                                    Some(text(name, src)),
                                );
                            }
                        }
                        "dotted_name" => {
                            let t = text(ch, src);
                            if !t.contains('.') {
                                push(t.clone(), None, BindKind::Namespace, Some(t));
                            }
                        }
                        _ => {}
                    }
                }
            } else {
                let module = n.child_by_field_name("module_name").map(|m| text(m, src));
                for ch in n.named_children(&mut c) {
                    if Some(ch) == n.child_by_field_name("module_name") {
                        continue;
                    }
                    match ch.kind() {
                        "dotted_name" => {
                            let t = text(ch, src);
                            let local = t.rsplit('.').next().unwrap_or(&t).to_string();
                            push(local, Some(t), BindKind::Named, module.clone());
                        }
                        "aliased_import" => {
                            if let (Some(name), Some(alias)) = (
                                ch.child_by_field_name("name"),
                                ch.child_by_field_name("alias"),
                            ) {
                                push(
                                    text(alias, src),
                                    Some(text(name, src)),
                                    BindKind::Named,
                                    module.clone(),
                                );
                            }
                        }
                        "wildcard_import" => {
                            push(String::new(), None, BindKind::Glob, module.clone())
                        }
                        _ => {}
                    }
                }
            }
        }
        Lang::Rust => {
            if n.kind() == "mod_item" {
                if let Some(name) = n.child_by_field_name("name") {
                    let t = text(name, src);
                    push(
                        t.clone(),
                        None,
                        BindKind::Namespace,
                        Some(format!("mod {t}")),
                    );
                }
                return;
            }
            if let Some(arg) = n.child_by_field_name("argument") {
                rust_use(arg, src, "", &mut push);
            }
        }
        Lang::Go => {
            let Some(path) = n
                .child_by_field_name("path")
                .and_then(|p| string_value(p, src))
            else {
                return;
            };
            match n
                .child_by_field_name("name")
                .map(|a| (a.kind(), text(a, src)))
            {
                Some((_, a)) if a == "_" => {}
                Some((_, a)) if a == "." => push(String::new(), None, BindKind::Glob, Some(path)),
                Some((_, a)) => push(a, None, BindKind::Namespace, Some(path)),
                None => {
                    let last = path.rsplit('/').next().unwrap_or(&path).to_string();
                    push(last, None, BindKind::Namespace, Some(path));
                }
            }
        }
        Lang::Java => {
            let t = text(n, src);
            let body = t
                .trim()
                .trim_start_matches("import")
                .trim()
                .trim_end_matches(';')
                .trim();
            let (is_static, body) = match body.strip_prefix("static") {
                Some(rest) => (true, rest.trim()),
                None => (false, body),
            };
            let dotted: String = body.chars().filter(|c| !c.is_whitespace()).collect();
            if let Some(pkg) = dotted.strip_suffix(".*") {
                push(
                    String::new(),
                    None,
                    BindKind::Glob,
                    Some(format!("{pkg}.*")),
                );
                return;
            }
            let mut segs: Vec<&str> = dotted.split('.').collect();
            let Some(last) = segs.pop() else { return };
            if is_static {
                out.push(Binding {
                    line: l,
                    local: last.to_string(),
                    imported: Some(last.to_string()),
                    kind: BindKind::Named,
                    spec: Some(dotted.clone()),
                    container: segs.last().map(|s| s.to_string()),
                });
            } else {
                push(
                    last.to_string(),
                    Some(last.to_string()),
                    BindKind::Named,
                    Some(dotted.clone()),
                );
            }
        }
        Lang::CSharp => {
            let t = text(n, src);
            let body = t
                .trim()
                .trim_start_matches("global")
                .trim()
                .trim_start_matches("using")
                .trim()
                .trim_end_matches(';')
                .trim();
            let body = body.strip_prefix("static").map_or(body, str::trim);
            if let Some((alias, target)) = body.split_once('=') {
                push(
                    alias.trim().to_string(),
                    None,
                    BindKind::Namespace,
                    Some(target.trim().to_string()),
                );
            } else {
                push(String::new(), None, BindKind::Glob, Some(body.to_string()));
            }
        }
        Lang::Gd => {
            if let Some(name) = n.child_by_field_name("name") {
                push(text(name, src), None, BindKind::Namespace, None);
            }
        }
        Lang::C | Lang::Cpp => {}
    }
}

fn named_spec(
    s: Node<'_>,
    src: &[u8],
    spec: &Option<String>,
    push: &mut impl FnMut(String, Option<String>, BindKind, Option<String>),
) {
    let Some(name) = s.child_by_field_name("name") else {
        return;
    };
    let imported = text(name, src);
    let local = s
        .child_by_field_name("alias")
        .map_or_else(|| imported.clone(), |a| text(a, src));
    if imported == "default" {
        push(local, None, BindKind::Default, spec.clone());
    } else {
        push(local, Some(imported), BindKind::Named, spec.clone());
    }
}

/// Flatten a Rust `use` tree into bindings. `prefix` is the path so far (`crate::a`).
fn rust_use(
    n: Node<'_>,
    src: &[u8],
    prefix: &str,
    push: &mut impl FnMut(String, Option<String>, BindKind, Option<String>),
) {
    let join = |a: &str, b: &str| {
        if a.is_empty() {
            b.to_string()
        } else {
            format!("{a}::{b}")
        }
    };
    let compact = |s: String| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
    match n.kind() {
        "identifier" | "scoped_identifier" | "crate" | "super" | "self" => {
            let path = join(prefix, &compact(text(n, src)));
            let leaf = path.rsplit("::").next().unwrap_or(&path).to_string();
            if leaf == "self" {
                let module = path.trim_end_matches("::self");
                let local = module.rsplit("::").next().unwrap_or(module).to_string();
                push(local, None, BindKind::Namespace, Some(module.to_string()));
            } else if !matches!(leaf.as_str(), "crate" | "super") {
                push(leaf.clone(), Some(leaf), BindKind::Named, Some(path));
            }
        }
        "use_as_clause" => {
            if let (Some(p), Some(a)) = (
                n.child_by_field_name("path"),
                n.child_by_field_name("alias"),
            ) {
                let path = join(prefix, &compact(text(p, src)));
                let leaf = path.rsplit("::").next().unwrap_or(&path).to_string();
                push(text(a, src), Some(leaf), BindKind::Named, Some(path));
            }
        }
        "use_wildcard" => {
            let inner = n
                .named_child(0)
                .map(|p| compact(text(p, src)))
                .unwrap_or_default();
            let path = join(prefix, &inner);
            push(
                String::new(),
                None,
                BindKind::Glob,
                Some(format!("{path}::*")),
            );
        }
        "scoped_use_list" => {
            let base = n
                .child_by_field_name("path")
                .map(|p| join(prefix, &compact(text(p, src))))
                .unwrap_or_else(|| prefix.to_string());
            if let Some(list) = n.child_by_field_name("list") {
                rust_use(list, src, &base, push);
            }
        }
        "use_list" => {
            let mut c = n.walk();
            for ch in n.named_children(&mut c) {
                rust_use(ch, src, prefix, push);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------
// Entry points

const JS_HTTP: &[&str] = &[
    "get", "post", "put", "delete", "patch", "all", "options", "head",
];
const GO_HTTP: &[&str] = &[
    "GET",
    "POST",
    "PUT",
    "DELETE",
    "PATCH",
    "HEAD",
    "OPTIONS",
    "Any",
    "Handle",
    "HandleFunc",
    "Get",
    "Post",
    "Put",
    "Delete",
    "Patch",
];
const NEST_HTTP: &[&str] = &[
    "Get", "Post", "Put", "Delete", "Patch", "All", "Options", "Head",
];

/// A handler argument, named like a call.
fn handler_ref(lang: Lang, n: Node<'_>, src: &[u8]) -> Option<Call> {
    let l = line(n);
    match n.kind() {
        "identifier" => Some(Call {
            line: l,
            name: text(n, src),
            form: Form::Plain,
            qualifier: None,
            path: Vec::new(),
            scope: Vec::new(),
            field: None,
        }),
        "member_expression"
        | "selector_expression"
        | "attribute"
        | "field_expression"
        | "member_access_expression" => {
            let (obj, prop) = match lang {
                Lang::Go => (
                    n.child_by_field_name("operand"),
                    n.child_by_field_name("field"),
                ),
                Lang::Python => (
                    n.child_by_field_name("object"),
                    n.child_by_field_name("attribute"),
                ),
                Lang::Rust => (
                    n.child_by_field_name("value"),
                    n.child_by_field_name("field"),
                ),
                Lang::CSharp => (
                    n.child_by_field_name("expression"),
                    n.child_by_field_name("name"),
                ),
                _ => (
                    n.child_by_field_name("object"),
                    n.child_by_field_name("property"),
                ),
            };
            let prop = prop?;
            let (form, qualifier) = member_receiver(lang, obj, src);
            Some(Call {
                line: l,
                name: text(prop, src),
                form,
                qualifier,
                path: Vec::new(),
                scope: Vec::new(),
                field: obj.and_then(|o| self_field(lang, o, src)),
            })
        }
        "argument" => n.named_child(0).and_then(|c| handler_ref(lang, c, src)),
        "scoped_identifier" => {
            let name = n.child_by_field_name("name")?;
            let recv = n.child_by_field_name("path");
            let (form, qualifier) = path_receiver(lang, recv, src);
            let (path, scope) = rust_call_path(lang, form, n, recv, src);
            Some(Call {
                line: l,
                name: text(name, src),
                form,
                qualifier,
                path,
                scope,
                field: None,
            })
        }
        _ => None,
    }
}

fn upper_method(m: &str) -> String {
    match m {
        "all" | "Any" | "Handle" | "HandleFunc" => "ANY".to_string(),
        other => other.to_ascii_uppercase(),
    }
}

/// `a/b` + `c` → `/a/b/c`.
fn join_path(prefix: &str, path: &str) -> String {
    let p = prefix.trim_matches('/');
    let s = path.trim_start_matches('/');
    let joined = match (p.is_empty(), s.is_empty()) {
        (true, _) => s.to_string(),
        (false, true) => p.to_string(),
        (false, false) => format!("{p}/{s}"),
    };
    format!("/{joined}")
}

#[allow(clippy::too_many_arguments)]
fn entries(
    lang: Lang,
    src: &[u8],
    defs: &HashSet<String>,
    calls: &[CallCap<'_>],
    decorators: &[Node<'_>],
    attributes: &[Node<'_>],
    mains: &[(Node<'_>, Node<'_>)],
    cobras: &[Node<'_>],
    def_caps: &[DefCap<'_>],
    code: &mut Code,
) {
    let mut out: Vec<RawEntry> = Vec::new();
    let entry = |l: u32, kind, name: String| RawEntry {
        line: l,
        kind,
        name,
        handler: None,
        def: None,
        gate: None,
    };

    // `main` functions.
    for d in &code.defs {
        let is_main = match lang {
            Lang::Rust | Lang::C | Lang::Cpp => d.name == "main" && d.container.is_none(),
            Lang::Go => d.name == "main" && d.container.is_none() && code.package_main,
            Lang::Java => d.name == "main" && d.kind == SymbolKind::Method,
            Lang::CSharp => d.name == "Main" && d.kind == SymbolKind::Method,
            _ => false,
        };
        if is_main && d.kind.is_callable() {
            out.push(RawEntry {
                def: Some(d.qualified()),
                ..entry(d.line, EntryKind::Main, "main".to_string())
            });
        }
    }
    // Java `static void main`: require `static`; C# `Main`: require `static`.
    if matches!(lang, Lang::Java | Lang::CSharp) {
        let statics: HashSet<u32> = def_caps
            .iter()
            .filter(|d| d.cap == "def.method" && has_modifier(d.node, src, &["static"]))
            .filter_map(|d| d.name.map(line))
            .collect();
        out.retain(|e| e.kind != EntryKind::Main || statics.contains(&e.line));
    }
    for (node, cond) in mains {
        let t: String = text(*cond, src)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if t.contains("__name__") && t.contains("__main__") {
            out.push(entry(line(*node), EntryKind::Main, "__main__".to_string()));
        }
    }

    // Route and command registrations written as calls.
    for c in calls {
        if c.cap != "call.member" {
            continue;
        }
        let Some(args) = c.args else { continue };
        let name = text(c.name, src);
        let interesting = match lang {
            Lang::Js => JS_HTTP.contains(&name.as_str()) || name == "command",
            Lang::Go => GO_HTTP.contains(&name.as_str()),
            Lang::Rust => name == "route",
            Lang::CSharp => name.starts_with("Map"),
            Lang::Python => name == "add_parser",
            _ => false,
        };
        if !interesting {
            continue;
        }
        let list = named_args(args);
        let first = list.first().and_then(|a| {
            let a = if a.kind() == "argument" {
                a.named_child(0).unwrap_or(*a)
            } else {
                *a
            };
            string_value(a, src)
        });
        let l = line(c.name);
        match lang {
            Lang::Js => {
                if JS_HTTP.contains(&name.as_str())
                    && list.len() >= 2
                    && let Some(path) = first.as_ref().filter(|p| p.starts_with('/'))
                {
                    out.push(RawEntry {
                        handler: list.last().and_then(|h| handler_ref(lang, *h, src)),
                        gate: Some("js-http".into()),
                        ..entry(
                            l,
                            EntryKind::Http,
                            format!("{} {path}", upper_method(&name)),
                        )
                    });
                } else if name == "command"
                    && let Some(cmd) = first.as_ref()
                {
                    let cmd = cmd.split_whitespace().next().unwrap_or("").to_string();
                    if cmd.is_empty() {
                        continue;
                    }
                    out.push(RawEntry {
                        handler: commander_action(c.node, src),
                        gate: Some("js-cli".into()),
                        ..entry(l, EntryKind::Cli, cmd)
                    });
                }
            }
            Lang::Go => {
                if GO_HTTP.contains(&name.as_str())
                    && list.len() >= 2
                    && let Some(path) = first.as_ref().filter(|p| p.starts_with('/'))
                {
                    out.push(RawEntry {
                        handler: list.last().and_then(|h| handler_ref(lang, *h, src)),
                        gate: Some("go-http".into()),
                        ..entry(
                            l,
                            EntryKind::Http,
                            format!("{} {path}", upper_method(&name)),
                        )
                    });
                }
            }
            Lang::Rust => {
                if name == "route"
                    && let Some(path) = first.as_ref().filter(|p| p.starts_with('/'))
                    && let Some(router) = list.get(1)
                {
                    for (method, h) in axum_methods(*router, src) {
                        out.push(RawEntry {
                            handler: h.and_then(|h| handler_ref(lang, h, src)),
                            gate: Some("rust-http".into()),
                            ..entry(l, EntryKind::Http, format!("{method} {path}"))
                        });
                    }
                }
            }
            Lang::CSharp => {
                if let Some(m) = name.strip_prefix("Map")
                    && matches!(m, "Get" | "Post" | "Put" | "Delete" | "Patch" | "Methods")
                    && let Some(path) = first.as_ref()
                {
                    let method = if m == "Methods" {
                        "ANY".into()
                    } else {
                        upper_method(m)
                    };
                    out.push(RawEntry {
                        handler: list.last().and_then(|h| handler_ref(lang, *h, src)),
                        ..entry(
                            l,
                            EntryKind::Http,
                            format!("{method} {}", join_path("", path)),
                        )
                    });
                }
            }
            Lang::Python => {
                if name == "add_parser"
                    && let Some(cmd) = first.as_ref()
                {
                    out.push(RawEntry {
                        gate: Some("py-argparse".into()),
                        ..entry(l, EntryKind::Cli, cmd.clone())
                    });
                }
            }
            _ => {}
        }
    }

    // Decorators: Flask/FastAPI routes and click/typer commands (Python), NestJS routes.
    for dec in decorators {
        let Some(target) = decorated(lang, *dec) else {
            continue;
        };
        let Some(tname) = target.child_by_field_name("name").map(|n| text(n, src)) else {
            continue;
        };
        let container = match scope_of(lang, target, src) {
            Scope::Member(c) => Some(c),
            _ => None,
        };
        let qualified = match &container {
            Some(c) => format!("{c}.{tname}"),
            None => tname.clone(),
        };
        if !defs.contains(&qualified) {
            continue;
        }
        let Some(call) = decorator_call(*dec) else {
            // `@click.command` without arguments.
            let t = text(*dec, src);
            if lang == Lang::Python && (t.ends_with(".command") || t.ends_with(".group")) {
                out.push(RawEntry {
                    def: Some(qualified.clone()),
                    gate: Some("py-cli".into()),
                    ..entry(line(target), EntryKind::Cli, tname.replace('_', "-"))
                });
            }
            continue;
        };
        let func = call.child_by_field_name("function");
        let fname = func.map(|f| text(f, src)).unwrap_or_default();
        let method = fname.rsplit('.').next().unwrap_or(&fname).to_string();
        let arg = call
            .child_by_field_name("arguments")
            .and_then(first_arg)
            .and_then(|a| string_value(a, src));
        let l = line(target);
        match lang {
            Lang::Python => {
                let http = matches!(
                    method.as_str(),
                    "get"
                        | "post"
                        | "put"
                        | "delete"
                        | "patch"
                        | "route"
                        | "api_route"
                        | "websocket"
                ) && fname.contains('.');
                if http && let Some(path) = arg.as_ref().filter(|p| p.starts_with('/')) {
                    let verb = match method.as_str() {
                        "route" | "api_route" => "ANY".to_string(),
                        m => m.to_ascii_uppercase(),
                    };
                    out.push(RawEntry {
                        def: Some(qualified.clone()),
                        gate: Some("py-http".into()),
                        ..entry(l, EntryKind::Http, format!("{verb} {path}"))
                    });
                } else if method == "command" || method == "group" {
                    let name = arg.unwrap_or_else(|| tname.replace('_', "-"));
                    out.push(RawEntry {
                        def: Some(qualified.clone()),
                        gate: Some("py-cli".into()),
                        ..entry(l, EntryKind::Cli, name)
                    });
                }
            }
            Lang::Js if NEST_HTTP.contains(&fname.as_str()) => {
                let prefix = target
                    .parent()
                    .and_then(|b| b.parent())
                    .and_then(|class| nest_prefix(class, src))
                    .unwrap_or_default();
                let path = join_path(&prefix, arg.as_deref().unwrap_or(""));
                out.push(RawEntry {
                    def: Some(qualified.clone()),
                    gate: Some("js-nest".into()),
                    ..entry(
                        l,
                        EntryKind::Http,
                        format!("{} {path}", upper_method(&fname)),
                    )
                });
            }
            _ => {}
        }
    }

    // Annotations and attributes on methods: Spring, ASP.NET.
    if matches!(lang, Lang::Java | Lang::CSharp) {
        for d in def_caps.iter().filter(|d| d.cap == "def.method") {
            let Some(name) = d.name.map(|n| text(n, src)) else {
                continue;
            };
            let class = d.node.parent().and_then(|b| b.parent());
            let Some(cname) = class.and_then(|c| class_name(lang, c, src)) else {
                continue;
            };
            let routes = match lang {
                Lang::Java => spring_routes(d.node, class, src),
                _ => aspnet_routes(d.node, class, &cname, src),
            };
            for (verb, path) in routes {
                out.push(RawEntry {
                    def: Some(format!("{cname}.{name}")),
                    ..entry(line(d.node), EntryKind::Http, format!("{verb} {path}"))
                });
            }
        }
    }

    // Rust: `#[derive(Subcommand)] enum` variants are CLI commands.
    for a in attributes {
        let t = text(*a, src);
        if !(t.contains("derive") && t.contains("Subcommand")) {
            continue;
        }
        let mut next = a.next_named_sibling();
        while let Some(n) = next {
            if n.kind() == "attribute_item" || n.kind().contains("comment") {
                next = n.next_named_sibling();
                continue;
            }
            break;
        }
        let Some(item) = next.filter(|n| n.kind() == "enum_item") else {
            continue;
        };
        let Some(ename) = item.child_by_field_name("name").map(|n| text(n, src)) else {
            continue;
        };
        let Some(body) = item.child_by_field_name("body") else {
            continue;
        };
        let mut c = body.walk();
        for v in body.named_children(&mut c) {
            if v.kind() != "enum_variant" {
                continue;
            }
            if let Some(vn) = v.child_by_field_name("name") {
                out.push(RawEntry {
                    def: Some(ename.clone()),
                    ..entry(line(vn), EntryKind::Cli, kebab(&text(vn, src)))
                });
            }
        }
    }

    // Go: `cobra.Command{Use: "serve", RunE: runServe}`.
    for lit in cobras {
        let ty = lit
            .child_by_field_name("type")
            .map(|t| text(t, src))
            .unwrap_or_default();
        if !ty.ends_with("cobra.Command") {
            continue;
        }
        let Some(body) = lit.child_by_field_name("body") else {
            continue;
        };
        let (mut name, mut handler) = (None, None);
        let mut c = body.walk();
        for kv in body.named_children(&mut c) {
            if kv.kind() != "keyed_element" {
                continue;
            }
            let (Some(k), Some(v)) = (kv.named_child(0), kv.named_child(1)) else {
                continue;
            };
            let key = text(k, src);
            let v = if v.kind() == "literal_element" {
                v.named_child(0).unwrap_or(v)
            } else {
                v
            };
            match key.as_str() {
                "Use" => {
                    name = string_value(v, src)
                        .and_then(|s| s.split_whitespace().next().map(str::to_string))
                }
                "Run" | "RunE" => handler = handler_ref(lang, v, src),
                _ => {}
            }
        }
        if let Some(name) = name {
            out.push(RawEntry {
                handler,
                ..entry(line(*lit), EntryKind::Cli, name)
            });
        }
    }

    code.entries = out;
}

/// kebab-case as clap derives a subcommand name from a variant: heck 0.5's
/// `to_kebab_case` (`DryRun` → `dry-run`, `HTTPServer` → `http-server`, `ListURLs` →
/// `list-ur-ls`). Words split at non-alphanumerics, after a lowercase letter followed by
/// an uppercase one, and before the last uppercase letter of a run followed by a lowercase
/// one; digits keep the case mode of what precedes them.
fn kebab(s: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Boundary,
        Lower,
        Upper,
    }
    let mut words: Vec<&str> = Vec::new();
    for word in s.split(|c: char| !c.is_alphanumeric()) {
        let mut chars = word.char_indices().peekable();
        let (mut init, mut mode) = (0, Mode::Boundary);
        while let Some((i, c)) = chars.next() {
            let Some(&(next_i, next)) = chars.peek() else {
                words.push(&word[init..]);
                break;
            };
            let next_mode = if c.is_lowercase() {
                Mode::Lower
            } else if c.is_uppercase() {
                Mode::Upper
            } else {
                mode
            };
            if next_mode == Mode::Lower && next.is_uppercase() {
                words.push(&word[init..next_i]);
                (init, mode) = (next_i, Mode::Boundary);
            } else if mode == Mode::Upper && c.is_uppercase() && next.is_lowercase() {
                words.push(&word[init..i]);
                (init, mode) = (i, Mode::Boundary);
            } else {
                mode = next_mode;
            }
        }
    }
    words
        .iter()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join("-")
}

/// The definition a decorator applies to.
fn decorated(lang: Lang, dec: Node<'_>) -> Option<Node<'_>> {
    match lang {
        Lang::Python => {
            let p = dec.parent()?;
            if p.kind() != "decorated_definition" {
                return None;
            }
            p.child_by_field_name("definition")
                .filter(|d| d.kind() == "function_definition")
        }
        Lang::Js => {
            let mut next = dec.next_named_sibling();
            while let Some(n) = next {
                match n.kind() {
                    "decorator" => next = n.next_named_sibling(),
                    "method_definition" => return Some(n),
                    _ => return None,
                }
            }
            None
        }
        _ => None,
    }
}

/// The call inside a decorator (`@app.get("/x")`, `@Get(':id')`).
fn decorator_call(dec: Node<'_>) -> Option<Node<'_>> {
    let mut c = dec.walk();
    dec.named_children(&mut c)
        .find(|n| matches!(n.kind(), "call" | "call_expression"))
}

/// NestJS `@Controller('users')` prefix of a class.
fn nest_prefix(class: Node<'_>, src: &[u8]) -> Option<String> {
    let mut candidates = Vec::new();
    let mut c = class.walk();
    candidates.extend(class.children(&mut c).filter(|n| n.kind() == "decorator"));
    if let Some(p) = class.parent().filter(|p| p.kind() == "export_statement") {
        let mut c = p.walk();
        candidates.extend(p.children(&mut c).filter(|n| n.kind() == "decorator"));
    }
    for d in candidates {
        let Some(call) = decorator_call(d) else {
            continue;
        };
        let f = call.child_by_field_name("function").map(|f| text(f, src));
        if f.as_deref() == Some("Controller") {
            return Some(
                call.child_by_field_name("arguments")
                    .and_then(first_arg)
                    .and_then(|a| string_value(a, src))
                    .unwrap_or_default(),
            );
        }
    }
    None
}

/// `.action(handler)` chained after a commander `.command('x')`.
fn commander_action(call: Node<'_>, src: &[u8]) -> Option<Call> {
    let mut cur = call;
    for _ in 0..6 {
        let member = cur.parent().filter(|p| p.kind() == "member_expression")?;
        let outer = member.parent().filter(|p| p.kind() == "call_expression")?;
        let prop = member.child_by_field_name("property").map(|p| text(p, src));
        if prop.as_deref() == Some("action") {
            let args = outer.child_by_field_name("arguments")?;
            return first_arg(args).and_then(|a| handler_ref(Lang::Js, a, src));
        }
        cur = outer;
    }
    None
}

/// `get(h)`, `get(h).post(h2)`, `post(h)` → `[(GET, h), (POST, h2)]`.
fn axum_methods<'t>(n: Node<'t>, src: &[u8]) -> Vec<(String, Option<Node<'t>>)> {
    let mut out = Vec::new();
    let mut cur = Some(n);
    while let Some(c) = cur {
        if c.kind() != "call_expression" {
            break;
        }
        let Some(f) = c.child_by_field_name("function") else {
            break;
        };
        let arg = c.child_by_field_name("arguments").and_then(first_arg);
        match f.kind() {
            "identifier" => {
                let m = text(f, src);
                if JS_HTTP.contains(&m.as_str()) || m == "any" {
                    out.push((m.to_ascii_uppercase(), arg));
                }
                cur = None;
            }
            "field_expression" => {
                if let Some(field) = f.child_by_field_name("field") {
                    let m = text(field, src);
                    if JS_HTTP.contains(&m.as_str()) || m == "any" {
                        out.push((m.to_ascii_uppercase(), arg));
                    }
                }
                cur = f.child_by_field_name("value");
            }
            "scoped_identifier" => {
                if let Some(name) = f.child_by_field_name("name") {
                    let m = text(name, src);
                    if JS_HTTP.contains(&m.as_str()) || m == "any" {
                        out.push((m.to_ascii_uppercase(), arg));
                    }
                }
                cur = None;
            }
            _ => cur = None,
        }
    }
    out.reverse();
    out
}

/// Annotations of a Java declaration: `(name, first string argument, text)`.
fn java_annotations(n: Node<'_>, src: &[u8]) -> Vec<(String, Option<String>, String)> {
    let mut out = Vec::new();
    let mut c = n.walk();
    for ch in n.children(&mut c) {
        if ch.kind() != "modifiers" {
            continue;
        }
        let mut c2 = ch.walk();
        for a in ch.named_children(&mut c2) {
            if !matches!(a.kind(), "annotation" | "marker_annotation") {
                continue;
            }
            let name = a
                .child_by_field_name("name")
                .map(|x| text(x, src))
                .unwrap_or_default();
            let arg = a
                .child_by_field_name("arguments")
                .and_then(|args| find_string(args, src));
            out.push((name, arg, text(a, src)));
        }
    }
    out
}

/// First string literal under `n` (depth-first).
fn find_string(n: Node<'_>, src: &[u8]) -> Option<String> {
    if n.kind().contains("string_literal") || n.kind() == "string" {
        return string_value(n, src);
    }
    let mut c = n.walk();
    let children: Vec<Node<'_>> = n.named_children(&mut c).collect();
    children.into_iter().find_map(|ch| find_string(ch, src))
}

fn spring_routes(method: Node<'_>, class: Option<Node<'_>>, src: &[u8]) -> Vec<(String, String)> {
    let prefix = class
        .map(|c| java_annotations(c, src))
        .unwrap_or_default()
        .into_iter()
        .find(|(n, _, _)| n == "RequestMapping")
        .and_then(|(_, a, _)| a)
        .unwrap_or_default();
    let mut out = Vec::new();
    for (name, arg, full) in java_annotations(method, src) {
        let verb = match name.as_str() {
            "GetMapping" => "GET".to_string(),
            "PostMapping" => "POST".to_string(),
            "PutMapping" => "PUT".to_string(),
            "DeleteMapping" => "DELETE".to_string(),
            "PatchMapping" => "PATCH".to_string(),
            "RequestMapping" => ["GET", "POST", "PUT", "DELETE", "PATCH"]
                .iter()
                .find(|v| full.contains(&format!("RequestMethod.{v}")))
                .map_or("ANY".to_string(), |v| v.to_string()),
            _ => continue,
        };
        out.push((verb, join_path(&prefix, arg.as_deref().unwrap_or(""))));
    }
    out
}

/// Attributes of a C# declaration: `(name, first string argument)`.
fn cs_attributes(n: Node<'_>, src: &[u8]) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let mut c = n.walk();
    for list in n.children(&mut c) {
        if list.kind() != "attribute_list" {
            continue;
        }
        let mut c2 = list.walk();
        for a in list.named_children(&mut c2) {
            if a.kind() != "attribute" {
                continue;
            }
            let name = a
                .child_by_field_name("name")
                .map(|x| text(x, src))
                .unwrap_or_default();
            let mut c3 = a.walk();
            let arg = a
                .named_children(&mut c3)
                .find(|x| x.kind() == "attribute_argument_list")
                .and_then(|args| find_string(args, src));
            out.push((name, arg));
        }
    }
    out
}

fn aspnet_routes(
    method: Node<'_>,
    class: Option<Node<'_>>,
    class_name: &str,
    src: &[u8],
) -> Vec<(String, String)> {
    let controller = class_name.strip_suffix("Controller").unwrap_or(class_name);
    let prefix = class
        .map(|c| cs_attributes(c, src))
        .unwrap_or_default()
        .into_iter()
        .find(|(n, _)| n == "Route")
        .and_then(|(_, a)| a)
        .unwrap_or_default()
        .replace("[controller]", controller);
    let attrs = cs_attributes(method, src);
    let route = attrs
        .iter()
        .find(|(n, _)| n == "Route")
        .and_then(|(_, a)| a.clone());
    let mut out = Vec::new();
    for (name, arg) in &attrs {
        let Some(verb) = name.strip_prefix("Http") else {
            continue;
        };
        if !matches!(
            verb,
            "Get" | "Post" | "Put" | "Delete" | "Patch" | "Head" | "Options"
        ) {
            continue;
        }
        let tail = arg.clone().or_else(|| route.clone()).unwrap_or_default();
        let path = if tail.starts_with('/') || tail.starts_with('~') {
            join_path("", tail.trim_start_matches('~'))
        } else {
            join_path(&prefix, &tail)
        };
        out.push((verb.to_ascii_uppercase(), path));
    }
    if out.is_empty()
        && let Some(r) = route
    {
        out.push(("ANY".to_string(), join_path(&prefix, &r)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(lang: &str, src: &str) -> Code {
        let g = Grammar::for_lang(lang).unwrap();
        let tree = crate::grammars::parse(g, src.as_bytes()).unwrap();
        extract(g, &tree, src.as_bytes()).unwrap()
    }

    fn defs(c: &Code) -> Vec<String> {
        c.defs
            .iter()
            .map(|d| {
                format!(
                    "{} {}{} L{}",
                    d.kind.as_str(),
                    d.qualified(),
                    if d.exported { " pub" } else { "" },
                    d.line
                )
            })
            .collect()
    }

    fn calls(c: &Code) -> Vec<String> {
        c.calls
            .iter()
            .map(|c| {
                format!(
                    "{:?} {}{} L{}",
                    c.form,
                    c.qualifier
                        .as_ref()
                        .map_or(String::new(), |q| format!("{q}.")),
                    c.name,
                    c.line
                )
            })
            .collect()
    }

    #[test]
    fn python_all_decides_what_a_module_exports() {
        let c = code(
            "python",
            "__all__ = [\"_kept\", \"Api\"]\n__all__ += (\"extra\",)\n\n\
             def _kept():\n    pass\n\ndef dropped():\n    pass\n\ndef extra():\n    pass\n\n\
             class Api:\n    def call(self):\n        pass\n\n\
             class Hidden:\n    def call(self):\n        pass\n",
        );
        assert_eq!(
            defs(&c),
            vec![
                "function _kept pub L4",
                "function dropped L7",
                "function extra pub L10",
                "class Api pub L13",
                "method Api.call pub L14",
                "class Hidden L17",
                "method Hidden.call L18",
            ]
        );
        // Without `__all__`, `_names` are private; so with an `__all__` built at run time,
        // whose contents are not known.
        for src in [
            "def _p():\n    pass\n\ndef q():\n    pass\n",
            "__all__ = []\ndef _p():\n    pass\n\ndef q():\n    pass\n__all__.append(\"q\")\n",
            "__all__ = [\"q\"]\ndef _p():\n    pass\n\ndef q():\n    pass\n__all__ += m.__all__\n",
            "__all__ = [n for n in dir()]\ndef _p():\n    pass\n\ndef q():\n    pass\n",
            // Assigned or changed inside a block or a function: unknown too.
            "__all__ = [\"_p\"]\nif X:\n    __all__ += [\"q\"]\ndef _p():\n    pass\n\n\
             def q():\n    pass\n",
            "__all__ = [\"_p\"]\ntry:\n    __all__.append(\"q\")\nexcept E:\n    pass\n\
             def _p():\n    pass\n\ndef q():\n    pass\n",
            "__all__ = [\"_p\"]\ndef _p():\n    global __all__\n    __all__ = [\"q\"]\n\n\
             def q():\n    pass\n",
            "__all__ = [\"_p\"]\nfor n in N:\n    __all__[0] = n\ndef _p():\n    pass\n\n\
             def q():\n    pass\n",
            // A function or class that changes the module's list.
            "__all__ = [\"_p\"]\ndef _p():\n    __all__.append(\"q\")\n\n\
             def q():\n    pass\n",
            "__all__ = [\"_p\"]\ndef _p():\n    def inner():\n        global __all__\n        \
             __all__ += [\"q\"]\n\ndef q():\n    pass\n",
            "__all__ = [\"_p\"]\nif X:\n    x, __all__ = 1, [\"q\"]\ndef _p():\n    pass\n\n\
             def q():\n    pass\n",
            // Another module's list, extended here.
            "from .core import __all__\n__all__ += [\"_p\"]\ndef _p():\n    pass\n\n\
             def q():\n    pass\n",
        ] {
            let c = code("python", src);
            let d: Vec<String> = c
                .defs
                .iter()
                .map(|d| format!("{}{}", d.name, if d.exported { " pub" } else { "" }))
                .collect();
            assert_eq!(d, vec!["_p", "q pub"], "{src}");
        }
        // Read elsewhere, or a local `__all__` of a function or a class, it stays known.
        for src in [
            "__all__ = [\"_p\"]\ndef _p():\n    return len(__all__)\n\ndef q():\n    \
             return [n for n in __all__]\n",
            "__all__ = [\"_p\"]\ndef _p():\n    __all__ = []\n    __all__.append(1)\n\n\
             def q():\n    pass\n",
            "__all__ = [\"_p\"]\nclass C:\n    __all__ = [\"x\"]\ndef _p():\n    pass\n\n\
             def q():\n    for __all__ in []:\n        pass\n",
        ] {
            let c = code("python", src);
            let d: Vec<String> = c
                .defs
                .iter()
                .filter(|d| d.kind == SymbolKind::Function)
                .map(|d| format!("{}{}", d.name, if d.exported { " pub" } else { "" }))
                .collect();
            assert_eq!(d, vec!["_p pub", "q"], "{src}");
        }
    }

    fn supers(c: &Code) -> Vec<String> {
        c.defs
            .iter()
            .flat_map(|d| {
                d.supers.iter().map(move |s| {
                    format!(
                        "{} : {}{}",
                        d.name,
                        s.name,
                        if s.iface { " (i)" } else { "" }
                    )
                })
            })
            .collect()
    }

    #[test]
    fn supertypes_as_written() {
        let js = code(
            "javascript",
            "class A extends B {}\nclass C extends ns.B {}\nclass D extends mixin(B) {}\n\
             class E {}\n",
        );
        assert_eq!(supers(&js), ["A : B", "C : ns.B", "D : mixin(B)"]);
        let ts = code(
            "typescript",
            "class A<T> extends Base<T> implements I, J<T> {}\ninterface K extends I, ns.J {}\n",
        );
        assert_eq!(
            supers(&ts),
            [
                "A : Base",
                "A : I (i)",
                "A : J (i)",
                "K : I (i)",
                "K : ns.J (i)"
            ]
        );
        let py = code(
            "python",
            "class A(Base, mod.Mixin, Generic[T], metaclass=Meta):\n    pass\n",
        );
        assert_eq!(
            supers(&py),
            ["A : Base", "A : mod.Mixin", "A : Generic (i)"]
        );
        let rs = code(
            "rust",
            "trait A: B + fmt::Debug {}\nimpl<T> X for T {}\nimpl X for Vec<u8> {}\n\
             impl fmt::Display for Foo {}\n",
        );
        assert_eq!(supers(&rs), ["A : B (i)", "A : fmt::Debug (i)"]);
        let impls: Vec<String> = rs
            .impls
            .iter()
            .map(|i| format!("{} : {}", i.ty, i.tr))
            .collect();
        assert_eq!(impls, ["Vec : X", "Foo : fmt::Display"]);
        let gd = code("gdscript", "class_name Hero extends \"res://actor.gd\"\n");
        assert_eq!(
            gd.extends.map(|e| e.name).as_deref(),
            Some("\"res://actor.gd\"")
        );
    }

    #[test]
    fn kebab_matches_heck_as_clap_uses_it() {
        for (from, to) in [
            ("Run", "run"),
            ("DryRun", "dry-run"),
            ("HTTPServer", "http-server"),
            ("ListURLs", "list-ur-ls"),
            ("Http2Server", "http2-server"),
            ("V2", "v2"),
            ("ABC", "abc"),
            ("already_snake", "already-snake"),
        ] {
            assert_eq!(kebab(from), to, "{from}");
        }
    }

    #[test]
    fn nest_controller_prefix_after_a_bare_decorator() {
        let c = code(
            "typescript",
            "@Public\n@Controller('users')\nexport class UsersController {\n  \
             @Get(':id')\n  find() { return 1; }\n}\n",
        );
        let names: Vec<&str> = c.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["GET /users/:id"]);
    }

    #[test]
    fn wrapped_javascript_functions_are_symbols() {
        let c = code(
            "tsx",
            "export const Panel = forwardRef<A, B>((props, ref) => {\n  return go();\n});\n\
             const Row = React.memo(function Row() { return 1; });\n\
             const save = debounce(() => 1, 10);\nconst conf = makeConfig({ a: 1 });\n\
             export const store = createStore((set) => ({}));\n",
        );
        assert_eq!(
            defs(&c),
            vec![
                "function Panel pub L1",
                "function Row L4",
                "function save L5",
                "const store pub L7"
            ]
        );
    }

    fn fields(c: &Code) -> Vec<String> {
        c.fields
            .iter()
            .map(|f| format!("{}.{}: {}", f.container, f.name, f.ty))
            .collect()
    }

    fn field_calls(c: &Code) -> Vec<String> {
        c.calls
            .iter()
            .filter_map(|x| {
                x.field
                    .as_ref()
                    .map(|f| format!("{f}.{} L{}", x.name, x.line))
            })
            .collect()
    }

    #[test]
    fn field_types_and_self_field_calls() {
        let rust = code(
            "rust",
            "struct S<'a> {\n    guard: Guard,\n    shared: Arc<Guard>,\n    by_ref: &'a mut Store,\n    \
             opt: Option<Guard>,\n    path: crate::g::Guard,\n    many: Vec<Guard>,\n}\n\
             impl S<'_> {\n    fn f(&self) { self.guard.run(); self.opt.run(); self.guard.inner.run(); other.guard.run(); }\n}\n\
             fn g() { struct Local { x: Guard } }\n",
        );
        assert_eq!(
            fields(&rust),
            vec!["S.by_ref: Store", "S.guard: Guard", "S.shared: Guard"]
        );
        assert_eq!(field_calls(&rust), vec!["guard.run L10", "opt.run L10"]);

        let ts = code(
            "typescript",
            "export class Tools {\n  private guard: Guard;\n  #secret: Store;\n  maybe: Guard | null;\n  \
             constructor(private readonly g2: Guard, plain: Guard) {}\n  \
             go() { this.guard.run(); this.#secret.save(); this.g2.run(); }\n}\n",
        );
        assert_eq!(
            fields(&ts),
            vec![
                "Tools.#secret: Store",
                "Tools.g2: Guard",
                "Tools.guard: Guard"
            ]
        );
        assert_eq!(
            field_calls(&ts),
            vec!["guard.run L6", "#secret.save L6", "g2.run L6"]
        );

        let java = code(
            "java",
            "class Tools {\n  private Guard guard, spare;\n  List<Guard> all;\n  \
             void go() { this.guard.run(); guard.run(); }\n}\n",
        );
        assert_eq!(
            fields(&java),
            vec!["Tools.guard: Guard", "Tools.spare: Guard"]
        );
        assert_eq!(field_calls(&java), vec!["guard.run L4"]);

        let cs = code(
            "csharp",
            "class Tools {\n  private Guard guard;\n  public Store Store { get; set; }\n  \
             void Go() { this.guard.Run(); }\n}\n",
        );
        assert_eq!(
            fields(&cs),
            vec!["Tools.Store: Store", "Tools.guard: Guard"]
        );
        assert_eq!(field_calls(&cs), vec!["guard.Run L4"]);
    }

    #[test]
    fn rust_calls_inside_macro_arguments() {
        let c = code(
            "rust",
            "fn t() {\n    assert_eq!(ids(&r, 1), vec![2]);\n    println!(\"{}\", r.findings().len());\n    \
             let v = vec![Store::new(), self.go()];\n    format!(\"{x}\", x = S { a: 1 });\n}\n",
        );
        assert_eq!(
            calls(&c),
            vec![
                "Plain ids L2",
                "Member r.findings L3",
                "Member len L3",
                "Path Store.new L4",
                "SelfCall go L4",
            ]
        );
    }

    #[test]
    fn typescript_definitions_calls_and_bindings() {
        let c = code(
            "tsx",
            "import def, { foo, bar as baz } from './x';\nimport * as ns from './y';\n\
             export * from './z';\nexport function top() { foo(); ns.f(); this.m(); new Model(); \
             obj.method(); function local() {} }\n\
             export default class Model { save() { this.load(); } #p() {} }\n\
             const MAX = 3;\nconst helper = () => <Comp/>;\nconst x = 1;\n",
        );
        assert_eq!(
            defs(&c),
            vec![
                "function top pub L4",
                "class Model pub L5",
                "method Model.save pub L5",
                "method Model.#p L5",
                "const MAX L6",
                "function helper L7",
            ]
        );
        assert_eq!(
            calls(&c),
            vec![
                "Plain foo L4",
                "Member ns.f L4",
                "SelfCall m L4",
                "New Model L4",
                "Member obj.method L4",
                "SelfCall load L5",
                "Plain Comp L7",
            ]
        );
        assert_eq!(c.default_export.as_deref(), Some("Model"));
        assert!(c.module);
        let b: Vec<(String, Option<String>, BindKind)> = c
            .bindings
            .iter()
            .map(|b| (b.local.clone(), b.imported.clone(), b.kind))
            .collect();
        assert_eq!(
            b,
            vec![
                ("def".into(), None, BindKind::Default),
                ("foo".into(), Some("foo".into()), BindKind::Named),
                ("baz".into(), Some("bar".into()), BindKind::Named),
                ("ns".into(), None, BindKind::Namespace),
                (String::new(), None, BindKind::Glob),
            ]
        );
    }
}
