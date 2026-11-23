//! Raw records for bridges (see [`crate::bridges`]): string expressions of call arguments
//! ([`StrExpr`]), module-level string constants ([`ConstDef`]), and Electron IPC: channel
//! sites ([`IpcSite`]), preload exposures ([`Exposure`]) and renderer uses of an exposed
//! world ([`WorldUse`]). Matched after resolution (`symbols::resolve::bridge`).

use super::{CallCap, Lang, handler_ref, line, named_args, string_value, text};
use crate::symbols::strexpr::{ConstDef, Part, StrExpr};
use crate::symbols::{Call, Exposure, IpcOp, IpcSite, WorldUse};
use std::collections::HashMap;
use tree_sitter::Node;

/// Arguments recorded per call (the first ones; wrappers take the key early).
const MAX_ARGS: usize = 4;
/// Nesting of object literals read for constants and exposures.
const MAX_DEPTH: u8 = 3;

/// The node an expression wraps without changing its value (`(x)`, `x as T`, `x!`).
fn unwrap_expr(mut n: Node<'_>) -> Node<'_> {
    loop {
        match n.kind() {
            "parenthesized_expression"
            | "as_expression"
            | "satisfies_expression"
            | "non_null_expression"
            | "type_assertion"
            | "await_expression" => match n.named_child(0) {
                Some(c) if !c.kind().contains("comment") => n = c,
                _ => return n,
            },
            _ => return n,
        }
    }
}

/// A dotted name (`IPC.USERS_GET`, `this.base`, `window.api.x`), when `n` is only names.
pub(super) fn dotted(n: Node<'_>, src: &[u8]) -> Option<Vec<String>> {
    let n = unwrap_expr(n);
    match n.kind() {
        "identifier" | "this" | "property_identifier" | "shorthand_property_identifier" => {
            Some(vec![text(n, src)])
        }
        "member_expression" => {
            let mut v = dotted(n.child_by_field_name("object")?, src)?;
            let p = n.child_by_field_name("property")?;
            if !matches!(
                p.kind(),
                "property_identifier" | "private_property_identifier"
            ) {
                return None;
            }
            v.push(text(p, src));
            Some(v)
        }
        // Java `Constants.X`, Python `mod.X`.
        "field_access" => {
            let mut v = dotted(n.child_by_field_name("object")?, src)?;
            v.push(text(n.child_by_field_name("field")?, src));
            Some(v)
        }
        "attribute" => {
            let mut v = dotted(n.child_by_field_name("object")?, src)?;
            v.push(text(n.child_by_field_name("attribute")?, src));
            Some(v)
        }
        _ => None,
    }
}

/// Parameter names of the function around `n` (innermost first), each with its function's
/// parameters in order.
fn enclosing_params(lang: Lang, n: Node<'_>, src: &[u8]) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = n.parent();
    while let Some(p) = cur {
        let params = match (lang, p.kind()) {
            (
                Lang::Js,
                "function_declaration"
                | "function_expression"
                | "function"
                | "generator_function_declaration"
                | "generator_function"
                | "method_definition"
                | "arrow_function",
            ) => p
                .child_by_field_name("parameters")
                .or_else(|| p.child_by_field_name("parameter")),
            (Lang::Java, "method_declaration" | "constructor_declaration") => {
                p.child_by_field_name("parameters")
            }
            (Lang::Python, "function_definition") => p.child_by_field_name("parameters"),
            _ => None,
        };
        if let Some(ps) = params {
            out.push(param_names(ps, src));
        }
        cur = p.parent();
    }
    out
}

fn param_names(ps: Node<'_>, src: &[u8]) -> Vec<String> {
    if ps.kind() == "identifier" {
        return vec![text(ps, src)];
    }
    let mut c = ps.walk();
    ps.named_children(&mut c)
        .filter(|p| !p.kind().contains("comment"))
        .map(|p| {
            let name = p
                .child_by_field_name("pattern")
                .or_else(|| p.child_by_field_name("name"))
                .or_else(|| p.child_by_field_name("left"))
                .unwrap_or(p);
            match name.kind() {
                "identifier" => text(name, src),
                // `...args`, `self`, a destructuring pattern: holds a place, never a name.
                _ => String::new(),
            }
        })
        .collect()
}

/// The [`Part`] for a name: a parameter of the innermost function that declares it, else
/// a reference.
fn name_part(lang: Lang, n: Node<'_>, src: &[u8], names: Vec<String>) -> Part {
    if names.len() == 1 {
        for params in enclosing_params(lang, n, src).iter().take(1) {
            if let Some(i) = params.iter().position(|p| *p == names[0])
                && i < u8::MAX as usize
            {
                return Part::Param(i as u8);
            }
        }
    }
    Part::Ref(names.join("."))
}

/// How the expression `n` builds a string.
pub(super) fn str_expr(lang: Lang, n: Node<'_>, src: &[u8]) -> StrExpr {
    let mut out = StrExpr::default();
    build(lang, n, src, &mut out, 0);
    out
}

fn build(lang: Lang, n: Node<'_>, src: &[u8], out: &mut StrExpr, depth: u8) {
    if depth > 12 {
        out.push(Part::Hole(text(n, src)));
        return;
    }
    let n = if lang == Lang::Js { unwrap_expr(n) } else { n };
    match n.kind() {
        "template_string" => {
            let mut c = n.walk();
            for ch in n.named_children(&mut c) {
                match ch.kind() {
                    "string_fragment" => out.push(Part::Lit(text(ch, src))),
                    "escape_sequence" => out.push(Part::Lit(unescape(&text(ch, src)))),
                    "template_substitution" => match ch.named_child(0) {
                        Some(e) => build(lang, e, src, out, depth + 1),
                        None => out.push(Part::Hole(text(ch, src))),
                    },
                    _ => {}
                }
            }
        }
        // Python f-strings and plain strings (the interpolations are holes or names).
        "string" if lang == Lang::Python => {
            let prefix = text(n, src)
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect::<String>()
                .to_ascii_lowercase();
            if !prefix.contains('f') {
                out.push(Part::Lit(string_value(n, src).unwrap_or_default()));
                return;
            }
            let mut c = n.walk();
            for ch in n.named_children(&mut c) {
                match ch.kind() {
                    "string_content" => out.push(Part::Lit(text(ch, src))),
                    "interpolation" => match ch.child_by_field_name("expression") {
                        Some(e) => build(lang, e, src, out, depth + 1),
                        None => out.push(Part::Hole(text(ch, src))),
                    },
                    _ => {}
                }
            }
        }
        "concatenated_string" => {
            let mut c = n.walk();
            for ch in n.named_children(&mut c) {
                build(lang, ch, src, out, depth + 1);
            }
        }
        k if k.contains("string") => match string_value(n, src) {
            Some(s) => out.push(Part::Lit(s)),
            None => out.push(Part::Hole(text(n, src))),
        },
        "number" | "decimal_integer_literal" | "integer" => out.push(Part::Lit(text(n, src))),
        "binary_expression" | "binary_operator"
            if n.child_by_field_name("operator")
                .is_some_and(|o| text(o, src) == "+") =>
        {
            match (
                n.child_by_field_name("left"),
                n.child_by_field_name("right"),
            ) {
                (Some(l), Some(r)) => {
                    build(lang, l, src, out, depth + 1);
                    build(lang, r, src, out, depth + 1);
                }
                _ => out.push(Part::Hole(text(n, src))),
            }
        }
        "parenthesized_expression" => match n.named_child(0) {
            Some(e) => build(lang, e, src, out, depth + 1),
            None => out.push(Part::Hole(text(n, src))),
        },
        "identifier" | "member_expression" | "field_access" | "attribute" => match dotted(n, src) {
            Some(names) => out.push(name_part(lang, n, src, names)),
            None => out.push(Part::Hole(text(n, src))),
        },
        _ => out.push(Part::Hole(text(n, src))),
    }
}

fn unescape(s: &str) -> String {
    match s {
        "\\n" => "\n".into(),
        "\\t" => "\t".into(),
        _ => s.trim_start_matches('\\').to_string(),
    }
}

/// The arguments of a call worth keeping for bridges (see [`Call::args`]): the string-like
/// ones among the first [`MAX_ARGS`], `None` for the others; empty when none is. With a
/// string-like first argument, the last argument as a function reference
/// ([`Call::handler`]; `this.m.bind(this)` reads as `this.m`).
pub(super) fn call_args(
    lang: Lang,
    args: Node<'_>,
    src: &[u8],
) -> (Vec<Option<StrExpr>>, Option<Call>) {
    let list = named_args(args);
    let mut out: Vec<Option<StrExpr>> = list
        .iter()
        .take(MAX_ARGS)
        .map(|a| {
            let a = if a.kind() == "argument" {
                a.named_child(0).unwrap_or(*a)
            } else {
                *a
            };
            if matches!(unwrap_expr(a).kind(), "number" | "true" | "false" | "null") {
                return None;
            }
            let e = str_expr(lang, a, src);
            e.is_stringish().then_some(e)
        })
        .collect();
    while out.last().is_some_and(Option::is_none) {
        out.pop();
    }
    let handler = match (out.first(), list.as_slice()) {
        (Some(Some(_)), [_, .., last]) => function_ref(lang, *last, src),
        _ => None,
    };
    (out, handler)
}

/// A function passed by reference: `f`, `this.m`, `obj.m`, `this.m.bind(this)`.
pub(super) fn function_ref(lang: Lang, n: Node<'_>, src: &[u8]) -> Option<Call> {
    let n = unwrap_expr(n);
    if n.kind() == "call_expression"
        && let Some(f) = n.child_by_field_name("function")
        && f.kind() == "member_expression"
        && f.child_by_field_name("property")
            .is_some_and(|p| text(p, src) == "bind")
    {
        return function_ref(lang, f.child_by_field_name("object")?, src);
    }
    match n.kind() {
        "identifier" | "member_expression" => handler_ref(lang, n, src),
        _ => None,
    }
}

/// Methods of Electron's IPC objects, by receiver.
fn ipc_op(recv_last: &str, method: &str) -> Option<IpcOp> {
    Some(match (recv_last, method) {
        // `webContents.ipc` (Electron 17+) serves the channels of one renderer.
        ("ipcMain" | "ipc", "handle" | "handleOnce") => IpcOp::Handle,
        ("ipcMain" | "ipc", "on" | "once" | "addListener") => IpcOp::On,
        ("ipcRenderer", "invoke") => IpcOp::Invoke,
        ("ipcRenderer", "send") => IpcOp::Send,
        ("ipcRenderer", "sendSync") => IpcOp::SendSync,
        ("ipcRenderer", "on" | "once" | "addListener") => IpcOp::Listen,
        ("webContents", "send") => IpcOp::Push,
        _ => return None,
    })
}

/// The IPC channel sites among a JS/TS file's calls: `ipcMain.handle|handleOnce|on|once`
/// (also on `webContents.ipc`), `ipcRenderer.invoke|send|sendSync|on|once`,
/// `<window>.webContents.send`.
pub(super) fn ipc_sites(calls: &[CallCap<'_>], src: &[u8]) -> Vec<IpcSite> {
    let mut out = Vec::new();
    for c in calls {
        if c.cap != "call.member" {
            continue;
        }
        let Some(recv) = c.recv else { continue };
        let recv_text = text(unwrap_expr(recv), src);
        let recv_last = recv_text
            .rsplit(['.', '?'])
            .find(|s| !s.is_empty())
            .unwrap_or(&recv_text)
            .trim();
        let Some(op) = ipc_op(recv_last, &text(c.name, src)) else {
            continue;
        };
        let Some(args) = c.args else { continue };
        let list = named_args(args);
        let Some(first) = list.first() else { continue };
        let channel = str_expr(Lang::Js, *first, src);
        let handler = match op {
            IpcOp::Handle | IpcOp::On | IpcOp::Listen if list.len() >= 2 => {
                list.last().and_then(|h| function_ref(Lang::Js, *h, src))
            }
            _ => None,
        };
        out.push(IpcSite {
            line: line(c.name),
            op,
            channel,
            handler,
        });
    }
    out
}

/// Names a renderer reads from `window` that are never an exposed world.
const BROWSER_GLOBALS: &[&str] = &[
    "location",
    "localStorage",
    "sessionStorage",
    "document",
    "navigator",
    "history",
    "console",
    "performance",
    "crypto",
    "screen",
    "parent",
    "top",
    "opener",
    "frames",
    "caches",
    "indexedDB",
    "visualViewport",
    "speechSynthesis",
    "customElements",
    "matchMedia",
];

fn is_global_root(s: &str) -> bool {
    matches!(s, "window" | "globalThis" | "self")
}

/// Calls on an exposed world: `window.W.a.b(…)`, `globalThis.W.a(…)`,
/// `(window as any).W.a(…)`, and through an alias (`const api = window.W; api.a(…)`).
pub(super) fn world_uses(calls: &[CallCap<'_>], root: Node<'_>, src: &[u8]) -> Vec<WorldUse> {
    let aliases = world_aliases(root, src);
    let mut out = Vec::new();
    for c in calls {
        if c.cap != "call.member" {
            continue;
        }
        let Some(recv) = c.recv else { continue };
        let Some(mut segs) = dotted(recv, src) else {
            continue;
        };
        segs.push(text(c.name, src));
        let (world, path, alias) = if is_global_root(&segs[0]) && segs.len() >= 3 {
            (segs[1].clone(), segs[2..].join("."), false)
        } else if let Some(w) = aliases.get(&segs[0])
            && segs.len() >= 2
        {
            (w.clone(), segs[1..].join("."), true)
        } else {
            continue;
        };
        if BROWSER_GLOBALS.contains(&world.as_str()) {
            continue;
        }
        let args = match c.args {
            Some(a) => call_args(Lang::Js, a, src).0,
            None => Vec::new(),
        };
        out.push(WorldUse {
            line: line(c.name),
            world,
            path,
            alias,
            args,
        });
    }
    out
}

/// `const api = window.W` (also `(window as any).W`, `globalThis.W`) anywhere in the file:
/// local name → world.
fn world_aliases(root: Node<'_>, src: &[u8]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "variable_declarator"
            && let Some(name) = n.child_by_field_name("name")
            && name.kind() == "identifier"
            && let Some(v) = n.child_by_field_name("value")
            && let Some(segs) = dotted(v, src)
            && segs.len() == 2
            && is_global_root(&segs[0])
            && !BROWSER_GLOBALS.contains(&segs[1].as_str())
        {
            out.insert(text(name, src), segs[1].clone());
        }
        let mut c = n.walk();
        for ch in n.named_children(&mut c) {
            stack.push(ch);
        }
    }
    out
}

/// `contextBridge.exposeInMainWorld("W", obj)`: every function property of the object
/// (literal, or a module-level `const obj = {…}` of the file), nested objects included,
/// as `W.a.b` with the definition of the file that runs it (`obj.a.b`, or the function a
/// shorthand property names).
pub(super) fn exposures(
    calls: &[CallCap<'_>],
    root: Node<'_>,
    src: &[u8],
    defs: &std::collections::HashSet<String>,
) -> Vec<Exposure> {
    let mut out = Vec::new();
    for c in calls {
        if text(c.name, src) != "exposeInMainWorld" {
            continue;
        }
        let Some(args) = c.args else { continue };
        let [world, obj] = named_args(args)[..] else {
            continue;
        };
        let Some(world) = string_value(world, src).filter(|w| !w.is_empty()) else {
            continue;
        };
        let obj = unwrap_expr(obj);
        let (owner, obj) = match obj.kind() {
            "object" => (world.clone(), Some(obj)),
            "identifier" => {
                let name = text(obj, src);
                (name.clone(), module_object(root, &name, src))
            }
            _ => continue,
        };
        let Some(obj) = obj else { continue };
        walk_exposed(obj, src, &world, &owner, "", defs, &mut out, 0);
    }
    out
}

/// The object literal of the module-level `const name = {…}`.
fn module_object<'t>(root: Node<'t>, name: &str, src: &[u8]) -> Option<Node<'t>> {
    let mut c = root.walk();
    for st in root.named_children(&mut c) {
        let decl = match st.kind() {
            "lexical_declaration" | "variable_declaration" => st,
            "export_statement" => match st.child_by_field_name("declaration") {
                Some(d) => d,
                None => continue,
            },
            _ => continue,
        };
        let mut dc = decl.walk();
        for d in decl.named_children(&mut dc) {
            if d.kind() == "variable_declarator"
                && d.child_by_field_name("name")
                    .is_some_and(|n| text(n, src) == name)
                && let Some(v) = d.child_by_field_name("value")
            {
                let v = unwrap_expr(v);
                return (v.kind() == "object").then_some(v);
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn walk_exposed(
    obj: Node<'_>,
    src: &[u8],
    world: &str,
    owner: &str,
    prefix: &str,
    defs: &std::collections::HashSet<String>,
    out: &mut Vec<Exposure>,
    depth: u8,
) {
    if depth >= MAX_DEPTH {
        return;
    }
    let mut c = obj.walk();
    for p in obj.named_children(&mut c) {
        let (key, value) = match p.kind() {
            "pair" => {
                let Some(k) = p.child_by_field_name("key") else {
                    continue;
                };
                let key = match k.kind() {
                    "property_identifier" => text(k, src),
                    "string" => match string_value(k, src) {
                        Some(s) => s,
                        None => continue,
                    },
                    _ => continue,
                };
                (key, p.child_by_field_name("value"))
            }
            "method_definition" => match p.child_by_field_name("name") {
                Some(n) => (text(n, src), None),
                None => continue,
            },
            "shorthand_property_identifier" => (text(p, src), Some(p)),
            _ => continue,
        };
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        let own = format!("{owner}.{path}");
        let value = value.map(unwrap_expr);
        let def = match value.map(|v| v.kind()) {
            Some("object") => {
                if let Some(v) = value {
                    walk_exposed(v, src, world, owner, &path, defs, out, depth + 1);
                }
                continue;
            }
            None | Some("arrow_function" | "function_expression" | "function") => {
                defs.contains(&own).then_some(own)
            }
            Some("identifier" | "shorthand_property_identifier") => {
                let name = value.map(|v| text(v, src)).unwrap_or_default();
                defs.contains(&name).then_some(name)
            }
            // A call, a value: nothing the renderer can run through this key.
            _ => continue,
        };
        out.push(Exposure {
            line: line(p),
            world: world.to_string(),
            path,
            def,
        });
    }
}

/// Module-level string constants of a file (see [`ConstDef`]).
pub(super) fn consts(lang: Lang, root: Node<'_>, src: &[u8]) -> Vec<ConstDef> {
    let mut out = Vec::new();
    match lang {
        Lang::Js => {
            let mut c = root.walk();
            for st in root.named_children(&mut c) {
                let st = if st.kind() == "export_statement" {
                    match st.child_by_field_name("declaration") {
                        Some(d) => d,
                        None => continue,
                    }
                } else {
                    st
                };
                match st.kind() {
                    "lexical_declaration" | "variable_declaration" => {
                        let mut dc = st.walk();
                        for d in st.named_children(&mut dc) {
                            if d.kind() != "variable_declarator" {
                                continue;
                            }
                            let (Some(n), Some(v)) = (
                                d.child_by_field_name("name"),
                                d.child_by_field_name("value"),
                            ) else {
                                continue;
                            };
                            if n.kind() == "identifier" {
                                js_const(&text(n, src), v, src, &mut out, 0);
                            }
                        }
                    }
                    "enum_declaration" => {
                        let (Some(n), Some(body)) = (
                            st.child_by_field_name("name"),
                            st.child_by_field_name("body"),
                        ) else {
                            continue;
                        };
                        let en = text(n, src);
                        let mut bc = body.walk();
                        for m in body.named_children(&mut bc) {
                            if m.kind() == "enum_assignment"
                                && let (Some(k), Some(v)) = (
                                    m.child_by_field_name("name"),
                                    m.child_by_field_name("value"),
                                )
                            {
                                let e = str_expr(lang, v, src);
                                let stringy =
                                    v.kind().contains("string") || v.kind() == "binary_expression";
                                if stringy && is_constant(&e) {
                                    out.push(ConstDef {
                                        name: format!("{en}.{}", text(k, src)),
                                        value: e,
                                        line: line(m),
                                    });
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Lang::Python => {
            let mut c = root.walk();
            for st in root.named_children(&mut c) {
                if st.kind() != "expression_statement" {
                    continue;
                }
                let Some(a) = st.named_child(0).filter(|a| a.kind() == "assignment") else {
                    continue;
                };
                let (Some(l), Some(r)) = (
                    a.child_by_field_name("left"),
                    a.child_by_field_name("right"),
                ) else {
                    continue;
                };
                if l.kind() != "identifier" {
                    continue;
                }
                let e = str_expr(lang, r, src);
                if is_constant(&e) {
                    out.push(ConstDef {
                        name: text(l, src),
                        value: e,
                        line: line(a),
                    });
                }
            }
        }
        Lang::Java => java_consts(root, src, &mut out, &[]),
        _ => {}
    }
    out
}

/// A value worth keeping as a constant: a literal or one built from names (no hole, no
/// parameter).
fn is_constant(e: &StrExpr) -> bool {
    !e.0.is_empty()
        && e.0.iter().all(|p| matches!(p, Part::Lit(_) | Part::Ref(_)))
        && e.0.iter().any(|p| matches!(p, Part::Lit(_)))
        || matches!(e.0.as_slice(), [Part::Ref(_)])
}

fn js_const(name: &str, v: Node<'_>, src: &[u8], out: &mut Vec<ConstDef>, depth: u8) {
    let v = unwrap_expr(v);
    if v.kind() == "object" {
        if depth >= MAX_DEPTH {
            return;
        }
        let mut c = v.walk();
        for p in v.named_children(&mut c) {
            if p.kind() != "pair" {
                continue;
            }
            let (Some(k), Some(val)) =
                (p.child_by_field_name("key"), p.child_by_field_name("value"))
            else {
                continue;
            };
            let key = match k.kind() {
                "property_identifier" => text(k, src),
                "string" => match string_value(k, src) {
                    Some(s) => s,
                    None => continue,
                },
                _ => continue,
            };
            js_const(&format!("{name}.{key}"), val, src, out, depth + 1);
        }
        return;
    }
    if !(v.kind().contains("string") || v.kind() == "binary_expression") {
        return;
    }
    let e = str_expr(Lang::Js, v, src);
    if is_constant(&e) {
        out.push(ConstDef {
            name: name.to_string(),
            value: e,
            line: line(v),
        });
    }
}

/// `static final String X = …` of every class and interface (fields of an interface are
/// constants), as `Class.X` (`Outer.Inner.X` for a nested class).
fn java_consts(n: Node<'_>, src: &[u8], out: &mut Vec<ConstDef>, classes: &[String]) {
    let mut c = n.walk();
    for ch in n.named_children(&mut c) {
        match ch.kind() {
            "class_declaration" | "interface_declaration" | "enum_declaration" => {
                let Some(name) = ch.child_by_field_name("name") else {
                    continue;
                };
                let mut inner = classes.to_vec();
                inner.push(text(name, src));
                if let Some(body) = ch.child_by_field_name("body") {
                    let iface = ch.kind() == "interface_declaration";
                    java_fields(body, src, out, &inner, iface);
                    java_consts(body, src, out, &inner);
                }
            }
            "program" | "class_body" | "interface_body" | "enum_body_declarations" => {
                java_consts(ch, src, out, classes)
            }
            _ => {}
        }
    }
}

fn java_fields(
    body: Node<'_>,
    src: &[u8],
    out: &mut Vec<ConstDef>,
    classes: &[String],
    iface: bool,
) {
    let mut c = body.walk();
    for f in body.named_children(&mut c) {
        if !matches!(f.kind(), "field_declaration" | "constant_declaration") {
            continue;
        }
        let ty = f.child_by_field_name("type").map(|t| text(t, src));
        if ty.as_deref() != Some("String") {
            continue;
        }
        if !iface
            && !(super::has_modifier(f, src, &["static"])
                && super::has_modifier(f, src, &["final"]))
        {
            continue;
        }
        let mut dc = f.walk();
        for d in f.named_children(&mut dc) {
            if d.kind() != "variable_declarator" {
                continue;
            }
            let (Some(n), Some(v)) = (
                d.child_by_field_name("name"),
                d.child_by_field_name("value"),
            ) else {
                continue;
            };
            let e = str_expr(Lang::Java, v, src);
            if is_constant(&e) {
                out.push(ConstDef {
                    name: format!("{}.{}", classes.join("."), text(n, src)),
                    value: e,
                    line: line(d),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::grammars::{Grammar, parse};
    use crate::symbols::strexpr::{Part, StrExpr};
    use crate::symbols::{Code, IpcOp, extract};

    fn code(lang: &str, src: &str) -> Code {
        let g = Grammar::for_lang(lang).unwrap();
        let tree = parse(g, src.as_bytes()).unwrap();
        extract(g, &tree, src.as_bytes()).unwrap()
    }

    fn show(e: &StrExpr) -> String {
        e.0.iter()
            .map(|p| match p {
                Part::Lit(s) => format!("'{s}'"),
                Part::Hole(h) => format!("<{h}>"),
                Part::Ref(r) => format!("@{r}"),
                Part::Param(i) => format!("${i}"),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn consts(c: &Code) -> Vec<String> {
        c.consts
            .iter()
            .map(|k| format!("{} = {} L{}", k.name, show(&k.value), k.line))
            .collect()
    }

    #[test]
    fn ts_constants_objects_enums_and_concatenations() {
        let c = code(
            "typescript",
            r#"
export const IPC = {
  USERS_GET: 'users:get',
  NESTED: { SAVE: "doc:save" },
  COUNT: 3,
} as const;
const BASE = '/api';
export const USERS = BASE + '/users';
const T = `v1/${BASE}`;
let notConst = f();
export enum Ch { A = 'a:x', B = 2 }
function local() { const HIDDEN = 'x'; }
"#,
        );
        assert_eq!(
            consts(&c),
            vec![
                "IPC.USERS_GET = 'users:get' L3",
                "IPC.NESTED.SAVE = 'doc:save' L4",
                "BASE = '/api' L7",
                "USERS = @BASE '/users' L8",
                "T = 'v1/' @BASE L9",
                "Ch.A = 'a:x' L11",
            ]
        );
    }

    #[test]
    fn java_and_python_constants() {
        let c = code(
            "java",
            r#"
package a;
public class Routes {
  public static final String USERS = "/users";
  static final String ME = USERS + "/me";
  private final String notStatic = "x";
  interface Keys { String SAVE = "save"; }
}
"#,
        );
        assert_eq!(
            consts(&c),
            vec![
                "Routes.USERS = '/users' L4",
                "Routes.ME = @USERS '/me' L5",
                "Routes.Keys.SAVE = 'save' L7",
            ]
        );
        let c = code(
            "python",
            "BASE = 'http://x'\nUSERS = BASE + '/users'\nF = f'{BASE}/me'\nn = compute()\n",
        );
        assert_eq!(
            consts(&c),
            vec![
                "BASE = 'http://x' L1",
                "USERS = @BASE '/users' L2",
                "F = @BASE '/me' L3",
            ]
        );
    }

    #[test]
    fn ipc_sites_wrappers_exposures_and_world_uses() {
        let main = code(
            "typescript",
            r#"
import { ipcMain, BrowserWindow } from 'electron';
import { IPC } from './channels';
abstract class Base {
  protected handle(channel: string, handler: (e: unknown) => unknown) {
    ipcMain.handle(channel, async (e) => handler(e));
  }
}
class Users extends Base {
  register(win: BrowserWindow) {
    this.handle('users:list', this.onList.bind(this));
    ipcMain.handle(IPC.USERS_GET, (_e, id) => id);
    ipcMain.on(`log:${win.id}`, onLog);
    win.webContents.send('progress', 1);
  }
  onList() {}
}
function onLog() {}
"#,
        );
        let sites: Vec<String> = main
            .ipc
            .iter()
            .map(|s| {
                format!(
                    "{:?} {} L{}{}",
                    s.op,
                    show(&s.channel),
                    s.line,
                    s.handler
                        .as_ref()
                        .map(|h| format!(" -> {}", h.name))
                        .unwrap_or_default()
                )
            })
            .collect();
        assert_eq!(
            sites,
            vec![
                "Handle $0 L6",
                "Handle @IPC.USERS_GET L12",
                "On 'log:' @win.id L13 -> onLog",
                "Push 'progress' L14",
            ]
        );
        let wrapped = main
            .calls
            .iter()
            .find(|c| c.name == "handle" && c.line == 11)
            .unwrap();
        assert_eq!(wrapped.args.len(), 1);
        assert_eq!(show(wrapped.args[0].as_ref().unwrap()), "'users:list'");
        assert_eq!(wrapped.handler.as_ref().unwrap().name, "onList");
        assert!(main.ipc.iter().all(|s| s.op != IpcOp::Invoke));

        let preload = code(
            "typescript",
            r#"
import { contextBridge, ipcRenderer } from 'electron';
function getUser(id: number) { return ipcRenderer.invoke('users:get', id); }
const deskApi = {
  save: (d: unknown) => ipcRenderer.send('doc:save', d),
  updater: { check: async () => ipcRenderer.invoke('update:check') },
  getUser,
  version: '1.0',
};
contextBridge.exposeInMainWorld('desk', deskApi);
contextBridge.exposeInMainWorld('api', { getUser, broken: () => ipcRenderer.invoke('nope') });
"#,
        );
        let ex: Vec<String> = preload
            .exposes
            .iter()
            .map(|e| format!("{}.{} L{} {:?}", e.world, e.path, e.line, e.def))
            .collect();
        assert_eq!(
            ex,
            vec![
                "desk.save L5 Some(\"deskApi.save\")",
                "desk.updater.check L6 Some(\"deskApi.updater.check\")",
                "desk.getUser L7 Some(\"getUser\")",
                "api.getUser L11 Some(\"getUser\")",
                "api.broken L11 Some(\"api.broken\")",
            ]
        );

        let renderer = code(
            "tsx",
            r#"
export function App() {
  const api = (window as any).api;
  api.getUser(1);
  window.desk.updater.check();
  (globalThis as any).desk.save('x');
  window.location.reload();
  api.on('progress', () => {});
  return null;
}
"#,
        );
        let uses: Vec<String> = renderer
            .worlds
            .iter()
            .map(|w| {
                format!(
                    "{}.{} L{}{}{}",
                    w.world,
                    w.path,
                    w.line,
                    if w.alias { " alias" } else { "" },
                    w.args
                        .first()
                        .and_then(|a| a.as_ref())
                        .map(|a| format!(" {}", show(a)))
                        .unwrap_or_default()
                )
            })
            .collect();
        assert_eq!(
            uses,
            vec![
                "api.getUser L4 alias",
                "desk.updater.check L5",
                "desk.save L6 'x'",
                "api.on L8 alias 'progress'",
            ]
        );
    }
}
