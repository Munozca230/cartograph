//! Contracts (see [`cartograph_core::contract`]): what a TS client passes as an HTTP body,
//! read where it is written (a typed parameter, a typed local, a local made by a function of
//! the file with a declared return type, `x as T`), and the MCP tools a server declares.
//!
//! Only names are kept here: the shapes of the types are read after resolution, for the
//! types that take part in a contract (`symbols::resolve::contract`).

use super::bridge::{arg_nodes, is_frame, is_function};
use super::{Lang, line, string_value, text};
use crate::symbols::strexpr::StrExpr;
use crate::symbols::{ArgBody, BodyExpr, RawTool};
use cartograph_core::ToolParam;
use std::collections::HashMap;
use tree_sitter::Node;

/// Arguments looked at for a body (as [`super::bridge`] does for URLs).
const MAX_ARGS: usize = 4;
/// Properties of an options object that hold the body: `fetch`'s `body`, axios' `data`,
/// got's and ky's `json`.
pub(super) const BODY_KEYS: &[&str] = &["body", "data", "json"];

/// Types that are never the shape of a JSON object.
const NOT_SHAPES: &[&str] = &[
    "string",
    "number",
    "boolean",
    "any",
    "unknown",
    "object",
    "never",
    "void",
    "null",
    "undefined",
    "bigint",
    "symbol",
    "Record",
    "Map",
    "Set",
    "Promise",
    "Object",
    "Function",
    "Date",
    "FormData",
    "Blob",
    "File",
    "URLSearchParams",
    "Buffer",
    "ArrayBuffer",
    "Uint8Array",
    "ReadableStream",
    "String",
    "Number",
    "Boolean",
    "BodyInit",
    "RequestInit",
    "Response",
    "Request",
    "HeadersInit",
    "JSON",
];

/// A type as written, when it may name the shape of an object or be one (an object type
/// written in place): whitespace removed; `None` for a primitive, a union or a library type
/// that is no shape.
pub(super) fn named_type(t: &str) -> Option<String> {
    // Whitespace out, except inside quotes (a key or a literal type) and one space between
    // two words (`readonly id`, `keyof T`).
    let word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$');
    let mut quote = None;
    let mut out = String::new();
    let mut gap = false;
    for c in t.chars() {
        if quote.is_none() && c.is_whitespace() {
            gap = true;
            continue;
        }
        if gap && out.chars().last().is_some_and(word) && word(c) {
            out.push(' ');
        }
        gap = false;
        match (quote, c) {
            (None, '\'' | '"' | '`') => quote = Some(c),
            (Some(q), _) if q == c => quote = None,
            _ => {}
        }
        out.push(c);
    }
    let t = out;
    // An object type written in place (`{ name: string; tags?: string[] }`).
    if t.starts_with('{') && t != "{}" {
        return Some(t);
    }
    let mut depth = 0i32;
    for c in t.chars() {
        match c {
            '<' | '(' | '{' | '[' => depth += 1,
            '>' | ')' | '}' | ']' => depth -= 1,
            '|' if depth == 0 => return None,
            _ => {}
        }
    }
    let head: String = t
        .chars()
        .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | '.'))
        .collect();
    if head.is_empty() || NOT_SHAPES.contains(&head.as_str()) {
        return None;
    }
    Some(t)
}

/// The type inside a type annotation (`: T`).
fn annotation_type<'t>(n: Node<'t>) -> Option<Node<'t>> {
    let a = n.child_by_field_name("type")?;
    if a.kind() == "type_annotation" {
        a.named_child(0)
    } else {
        Some(a)
    }
}

/// `Promise<T>` → `T`.
fn awaited(t: &str) -> &str {
    t.strip_prefix("Promise<")
        .and_then(|r| r.strip_suffix('>'))
        .unwrap_or(t)
}

/// Declared return types of the functions and methods of a file, by name (a name declared
/// twice with different types has none): what a local made by calling one of them is.
pub(super) fn return_types(root: Node<'_>, src: &[u8]) -> HashMap<String, Option<String>> {
    let mut out: HashMap<String, Option<String>> = HashMap::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        // Methods as `this.m` (what `this.m()` calls), functions by their name.
        let found = match n.kind() {
            "function_declaration" => n
                .child_by_field_name("name")
                .map(|x| (text(x, src), n.child_by_field_name("return_type"))),
            "method_definition" => n.child_by_field_name("name").map(|x| {
                (
                    format!("this.{}", text(x, src)),
                    n.child_by_field_name("return_type"),
                )
            }),
            "variable_declarator" | "public_field_definition" => n
                .child_by_field_name("name")
                .or_else(|| n.child_by_field_name("property"))
                .zip(n.child_by_field_name("value"))
                .filter(|(_, v)| matches!(v.kind(), "arrow_function" | "function_expression"))
                .map(|(x, v)| {
                    let name = if n.kind() == "public_field_definition" {
                        format!("this.{}", text(x, src))
                    } else {
                        text(x, src)
                    };
                    (name, v.child_by_field_name("return_type"))
                }),
            _ => None,
        };
        if let Some((name, rt)) = found {
            let ty = rt
                .and_then(|r| r.named_child(0))
                .and_then(|t| named_type(awaited(&text(t, src).replace(' ', ""))));
            match out.get(&name) {
                Some(prev) if *prev != ty => {
                    out.insert(name, None);
                }
                Some(_) => {}
                None => {
                    out.insert(name, ty);
                }
            }
        }
        let mut c = n.walk();
        for ch in n.named_children(&mut c) {
            stack.push(ch);
        }
    }
    out
}

/// What the parameters of a function are called, with their declared types.
fn typed_params(f: Node<'_>, src: &[u8]) -> Vec<(String, Option<String>)> {
    let Some(ps) = f
        .child_by_field_name("parameters")
        .or_else(|| f.child_by_field_name("parameter"))
    else {
        return Vec::new();
    };
    if ps.kind() == "identifier" {
        return vec![(text(ps, src), None)];
    }
    let mut c = ps.walk();
    ps.named_children(&mut c)
        .filter(|p| !p.kind().contains("comment"))
        .map(|p| {
            let name = p
                .child_by_field_name("pattern")
                .or_else(|| p.child_by_field_name("name"))
                .unwrap_or(p);
            let name = if name.kind() == "identifier" {
                text(name, src)
            } else {
                String::new()
            };
            let ty = annotation_type(p).map(|t| text(t, src));
            (name, ty)
        })
        .collect()
}

/// The declarator of the local `name` declared in the block `scope` before `child`.
fn local_decl<'t>(scope: Node<'t>, child: Node<'t>, name: &str, src: &[u8]) -> Option<Node<'t>> {
    if !matches!(scope.kind(), "statement_block" | "program") {
        return None;
    }
    let mut found = None;
    let mut c = scope.walk();
    for st in scope.named_children(&mut c) {
        if st.end_byte() > child.start_byte() {
            break;
        }
        if !matches!(st.kind(), "lexical_declaration" | "variable_declaration") {
            continue;
        }
        let mut dc = st.walk();
        for d in st.named_children(&mut dc) {
            if d.kind() == "variable_declarator"
                && d.child_by_field_name("name")
                    .is_some_and(|n| n.kind() == "identifier" && text(n, src) == name)
            {
                found = Some(d);
            }
        }
    }
    found
}

fn is_nothing(n: Node<'_>, src: &[u8]) -> bool {
    matches!(n.kind(), "undefined" | "null")
        || (n.kind() == "identifier" && text(n, src) == "undefined")
}

/// What the value of an initializer is: `x as T`, a call of a function of the file with a
/// declared return type (`this.toPayload(…)`, `await build(…)`), `new T(…)`.
fn value_body(v: Node<'_>, src: &[u8], rt: &HashMap<String, Option<String>>) -> Option<BodyExpr> {
    match v.kind() {
        "parenthesized_expression" | "await_expression" | "non_null_expression" => {
            value_body(v.named_child(0)?, src, rt)
        }
        "as_expression" | "satisfies_expression" => {
            named_type(&text(v.named_child(1)?, src)).map(BodyExpr::Type)
        }
        "call_expression" => {
            let f = v.child_by_field_name("function")?;
            let name = match f.kind() {
                "identifier" => text(f, src),
                "member_expression"
                    if f.child_by_field_name("object")
                        .is_some_and(|o| o.kind() == "this") =>
                {
                    format!("this.{}", text(f.child_by_field_name("property")?, src))
                }
                _ => return None,
            };
            rt.get(&name).cloned().flatten().map(BodyExpr::Type)
        }
        "new_expression" => {
            named_type(&text(v.child_by_field_name("constructor")?, src)).map(BodyExpr::Type)
        }
        _ => None,
    }
}

/// What the plain name at `n` is as a body: a parameter of the function a definition
/// stands for (with its type), a typed parameter of a callback, a typed local or one whose
/// value tells its type.
fn ident_body(n: Node<'_>, src: &[u8], rt: &HashMap<String, Option<String>>) -> Option<BodyExpr> {
    let name = text(n, src);
    let mut child = n;
    let mut cur = n.parent();
    while let Some(p) = cur {
        if is_function(Lang::Js, p) {
            if let Some((i, (_, ty))) = typed_params(p, src)
                .into_iter()
                .enumerate()
                .find(|(_, (x, _))| *x == name)
            {
                let ty = ty.and_then(|t| named_type(&t));
                return if is_frame(Lang::Js, p) && i < u8::MAX as usize {
                    Some(BodyExpr::Param(i as u8, ty))
                } else {
                    ty.map(BodyExpr::Type)
                };
            }
            if is_frame(Lang::Js, p) {
                return None;
            }
        }
        if let Some(d) = local_decl(p, child, &name, src) {
            if let Some(t) = annotation_type(d) {
                return named_type(&text(t, src)).map(BodyExpr::Type);
            }
            return value_body(d.child_by_field_name("value")?, src, rt);
        }
        child = p;
        cur = p.parent();
    }
    None
}

/// What the expression `n` sends as a body: `x`, `JSON.stringify(x)`,
/// `x ? JSON.stringify(x) : undefined`, `x as T`.
pub(super) fn ts_body(
    n: Node<'_>,
    src: &[u8],
    rt: &HashMap<String, Option<String>>,
) -> Option<BodyExpr> {
    match n.kind() {
        "parenthesized_expression" | "non_null_expression" | "await_expression" => {
            ts_body(n.named_child(0)?, src, rt)
        }
        "as_expression" | "satisfies_expression" => {
            named_type(&text(n.named_child(1)?, src)).map(BodyExpr::Type)
        }
        "call_expression" => {
            let f = n.child_by_field_name("function")?;
            if text(f, src) != "JSON.stringify" {
                return None;
            }
            ts_body(
                *arg_nodes(n.child_by_field_name("arguments")).first()?,
                src,
                rt,
            )
        }
        "ternary_expression" => {
            let (a, b) = (
                n.child_by_field_name("consequence")?,
                n.child_by_field_name("alternative")?,
            );
            if is_nothing(b, src) {
                ts_body(a, src, rt)
            } else if is_nothing(a, src) {
                ts_body(b, src, rt)
            } else {
                None
            }
        }
        "identifier" | "shorthand_property_identifier" => ident_body(n, src, rt),
        _ => None,
    }
}

/// The object literal a local is declared with, when it is one.
fn local_object<'t>(n: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let name = text(n, src);
    let mut child = n;
    let mut cur = n.parent();
    while let Some(p) = cur {
        if is_function(Lang::Js, p) && is_frame(Lang::Js, p) {
            return None;
        }
        if let Some(d) = local_decl(p, child, &name, src) {
            let v = d.child_by_field_name("value")?;
            return (v.kind() == "object").then_some(v);
        }
        child = p;
        cur = p.parent();
    }
    None
}

/// The body an options argument carries under `key` (`fetch(url, { body })`,
/// `axios({ url, data })`): its property, else what it spreads or is, a parameter's
/// options (`{ ...init }`, `init`).
pub(super) fn options_body(
    n: Node<'_>,
    key: &str,
    src: &[u8],
    rt: &HashMap<String, Option<String>>,
) -> Option<BodyExpr> {
    let n = if n.kind() == "parenthesized_expression" {
        n.named_child(0)?
    } else {
        n
    };
    let as_opts = |b: Option<BodyExpr>| match b {
        Some(BodyExpr::Param(i, _)) => Some(BodyExpr::Opts(i, key.to_string())),
        _ => None,
    };
    match n.kind() {
        "object" => {
            let mut spread = None;
            let mut c = n.walk();
            for p in n.named_children(&mut c) {
                match p.kind() {
                    "pair" => {
                        let k = p.child_by_field_name("key")?;
                        let k = string_value(k, src).unwrap_or_else(|| text(k, src));
                        if k == key {
                            return ts_body(p.child_by_field_name("value")?, src, rt);
                        }
                    }
                    "shorthand_property_identifier" if text(p, src) == key => {
                        return ident_body(p, src, rt);
                    }
                    "spread_element" => {
                        if let Some(x) = p.named_child(0).filter(|x| x.kind() == "identifier") {
                            spread = as_opts(ident_body(x, src, rt));
                        }
                    }
                    _ => {}
                }
            }
            spread
        }
        "identifier" => match local_object(n, src) {
            Some(obj) => options_body(obj, key, src, rt),
            None => as_opts(ident_body(n, src, rt)),
        },
        _ => None,
    }
}

/// What the arguments of a call may pass on as a body (see [`crate::symbols::Call::bodies`]):
/// a typed value, a parameter with its type, the body property of an object literal. A
/// parameter without a type is already among the call's arguments.
pub(super) fn arg_bodies(
    args: Node<'_>,
    src: &[u8],
    rt: &HashMap<String, Option<String>>,
) -> Vec<ArgBody> {
    let mut out = Vec::new();
    for (i, a) in arg_nodes(Some(args)).into_iter().take(MAX_ARGS).enumerate() {
        let arg = i as u8;
        if a.kind() == "object" {
            for key in BODY_KEYS {
                let has = {
                    let mut c = a.walk();
                    a.named_children(&mut c).any(|p| match p.kind() {
                        "pair" => p.child_by_field_name("key").is_some_and(|k| {
                            string_value(k, src).unwrap_or_else(|| text(k, src)) == *key
                        }),
                        "shorthand_property_identifier" => text(p, src) == *key,
                        _ => false,
                    })
                };
                if has && let Some(value) = options_body(a, key, src, rt) {
                    out.push(ArgBody {
                        arg,
                        key: Some(key.to_string()),
                        value,
                    });
                }
            }
            continue;
        }
        match ts_body(a, src, rt) {
            Some(BodyExpr::Param(_, None)) | None => {}
            Some(value) => out.push(ArgBody {
                arg,
                key: None,
                value,
            }),
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// MCP tools

/// Zod methods that make a field optional.
const ZOD_OPTIONAL: &[&str] = &["optional", "nullish", "default", "catch"];

/// Whether a zod schema expression is required: `z.string()` yes, `z.string().optional()`
/// no, a helper (`labelField()`, `nameSchema`) not known.
fn zod_required(v: Node<'_>, src: &[u8]) -> Option<bool> {
    // Walk the method chain from the outside in.
    let mut cur = v;
    loop {
        match cur.kind() {
            "call_expression" => {
                let f = cur.child_by_field_name("function")?;
                match f.kind() {
                    "member_expression" => {
                        let prop = text(f.child_by_field_name("property")?, src);
                        let obj = f.child_by_field_name("object")?;
                        if ZOD_OPTIONAL.contains(&prop.as_str()) {
                            return Some(false);
                        }
                        // `z.string()`, `z.optional(x)`: the root is zod's.
                        if obj.kind() == "identifier" && text(obj, src) == "z" {
                            return Some(prop != "optional");
                        }
                        cur = obj;
                    }
                    _ => return None,
                }
            }
            "parenthesized_expression" => cur = cur.named_child(0)?,
            _ => return None,
        }
    }
}

/// A module-level `const name = <value>` of the file.
fn module_const<'t>(root: Node<'t>, name: &str, src: &[u8]) -> Option<Node<'t>> {
    let mut c = root.walk();
    for st in root.named_children(&mut c) {
        let decl = if st.kind() == "export_statement" {
            st.child_by_field_name("declaration")
        } else {
            Some(st)
        };
        let Some(decl) = decl.filter(|d| d.kind() == "lexical_declaration") else {
            continue;
        };
        let mut dc = decl.walk();
        for d in decl.named_children(&mut dc) {
            if d.kind() == "variable_declarator"
                && d.child_by_field_name("name")
                    .is_some_and(|n| text(n, src) == name)
            {
                return d.child_by_field_name("value");
            }
        }
    }
    None
}

/// The object literal of a zod object (`z.object({…})`, with `.strict()` and the like after
/// it), or the literal itself.
fn zod_object<'t>(v: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let mut cur = v;
    loop {
        match cur.kind() {
            "object" => return Some(cur),
            "call_expression" => {
                let f = cur.child_by_field_name("function")?;
                if f.kind() != "member_expression" {
                    return None;
                }
                let prop = text(f.child_by_field_name("property")?, src);
                let obj = f.child_by_field_name("object")?;
                if prop == "object" && obj.kind() == "identifier" && text(obj, src) == "z" {
                    return arg_nodes(cur.child_by_field_name("arguments"))
                        .first()
                        .copied()
                        .filter(|a| a.kind() == "object");
                }
                if matches!(
                    prop.as_str(),
                    "strict" | "passthrough" | "strip" | "describe"
                ) {
                    cur = obj;
                } else {
                    return None;
                }
            }
            _ => return None,
        }
    }
}

/// The parameters of a zod raw shape or a zod object written as a literal (in the call or
/// as a module-level constant of the file: `schema.shape`, `schema`); `None` otherwise.
fn zod_params(v: Node<'_>, root: Node<'_>, src: &[u8]) -> Option<Vec<ToolParam>> {
    let v = match v.kind() {
        // `inputSchema.shape`
        "member_expression"
            if v.child_by_field_name("property")
                .is_some_and(|p| text(p, src) == "shape") =>
        {
            let o = v.child_by_field_name("object")?;
            if o.kind() != "identifier" {
                return None;
            }
            module_const(root, &text(o, src), src)?
        }
        "identifier" => module_const(root, &text(v, src), src)?,
        _ => v,
    };
    let obj = zod_object(v, src)?;
    let mut out = Vec::new();
    let mut c = obj.walk();
    for p in obj.named_children(&mut c) {
        match p.kind() {
            "pair" => {
                let k = p.child_by_field_name("key")?;
                let name = string_value(k, src).unwrap_or_else(|| text(k, src));
                let required = p
                    .child_by_field_name("value")
                    .and_then(|x| zod_required(x, src));
                out.push(ToolParam { name, required });
            }
            "comment" => {}
            // A spread or a method: not a literal shape.
            _ => return None,
        }
    }
    out.sort();
    Some(out)
}

/// The parameters of a JSON schema literal (`{ type: "object", properties: {…},
/// required: [...] }`); `None` when a part is not a literal.
fn json_schema_params(obj: Node<'_>, src: &[u8]) -> Option<Vec<ToolParam>> {
    let mut props = None;
    let mut required: Option<Vec<String>> = Some(Vec::new());
    let mut c = obj.walk();
    for p in obj.named_children(&mut c) {
        if p.kind() != "pair" {
            continue;
        }
        let k = p.child_by_field_name("key")?;
        let k = string_value(k, src).unwrap_or_else(|| text(k, src));
        let v = p.child_by_field_name("value")?;
        match k.as_str() {
            "properties" if v.kind() == "object" => props = Some(v),
            "required" => {
                required = (v.kind() == "array")
                    .then(|| {
                        let mut cc = v.walk();
                        v.named_children(&mut cc)
                            .map(|x| string_value(x, src))
                            .collect::<Option<Vec<String>>>()
                    })
                    .flatten();
            }
            _ => {}
        }
    }
    let props = props?;
    let mut out = Vec::new();
    let mut c = props.named_children(&mut props.walk()).collect::<Vec<_>>();
    c.retain(|p| p.kind() != "comment");
    for p in c {
        if p.kind() != "pair" {
            return None;
        }
        let k = p.child_by_field_name("key")?;
        let name = string_value(k, src).unwrap_or_else(|| text(k, src));
        let req = required.as_ref().map(|r| r.contains(&name));
        out.push(ToolParam {
            name,
            required: req,
        });
    }
    out.sort();
    Some(out)
}

/// A property of an object literal.
fn prop<'t>(obj: Node<'t>, key: &str, src: &[u8]) -> Option<Node<'t>> {
    let mut c = obj.walk();
    obj.named_children(&mut c)
        .filter(|p| p.kind() == "pair")
        .find(|p| {
            p.child_by_field_name("key")
                .is_some_and(|k| string_value(k, src).unwrap_or_else(|| text(k, src)) == key)
        })
        .and_then(|p| p.child_by_field_name("value"))
}

/// MCP tools of a TS/JS file that imports the MCP SDK: `server.tool(name, [description],
/// [schema], [annotations], handler)`, `server.registerTool(name, { inputSchema }, handler)`
/// and tool lists written as `{ name: "x", inputSchema: { type: "object", … } }`.
pub(super) fn js_tools(root: Node<'_>, src: &[u8]) -> Vec<RawTool> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "call_expression" => {
                if let Some(t) = tool_call(n, root, src) {
                    out.push(t);
                }
            }
            "object" => {
                if let (Some(name), Some(schema)) =
                    (prop(n, "name", src), prop(n, "inputSchema", src))
                    && schema.kind() == "object"
                    && prop(schema, "properties", src).is_some()
                    && let Some(name) = string_value(name, src)
                {
                    out.push(RawTool {
                        line: line(n),
                        name: StrExpr(vec![crate::symbols::strexpr::Part::Lit(name)]),
                        params: json_schema_params(schema, src),
                    });
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        for ch in n.named_children(&mut c) {
            stack.push(ch);
        }
    }
    out.sort_by_key(|t| t.line);
    out
}

/// A string, or a module constant of the file holding one (a tool's description).
fn is_text(a: Node<'_>, root: Node<'_>, src: &[u8]) -> bool {
    let stringy =
        |n: Node<'_>| matches!(n.kind(), "string" | "template_string" | "binary_expression");
    match a.kind() {
        "identifier" => module_const(root, &text(a, src), src).is_some_and(stringy),
        _ => stringy(a),
    }
}

/// An object whose values are literals (`{ readOnlyHint: true, label: "x" }`): a tool's
/// annotations, never a schema.
fn annotations_object(obj: Node<'_>) -> bool {
    let mut c = obj.walk();
    let pairs: Vec<Node<'_>> = obj
        .named_children(&mut c)
        .filter(|p| p.kind() != "comment")
        .collect();
    !pairs.is_empty()
        && pairs.iter().all(|p| {
            p.kind() == "pair"
                && p.child_by_field_name("value").is_some_and(|v| {
                    matches!(
                        v.kind(),
                        "true" | "false" | "string" | "number" | "template_string"
                    )
                })
        })
}

fn tool_call(n: Node<'_>, root: Node<'_>, src: &[u8]) -> Option<RawTool> {
    let f = n.child_by_field_name("function")?;
    if f.kind() != "member_expression" {
        return None;
    }
    let method = text(f.child_by_field_name("property")?, src);
    if !matches!(method.as_str(), "tool" | "registerTool") {
        return None;
    }
    let list = arg_nodes(n.child_by_field_name("arguments"));
    let (first, rest) = list.split_first()?;
    let name = super::bridge::str_expr(Lang::Js, *first, src);
    // A literal or a name (read through the file's constants after resolution); a name
    // built at run time counts when a handler is written last (it stays unknown).
    let named = name.0.iter().any(|p| {
        matches!(
            p,
            crate::symbols::strexpr::Part::Lit(_) | crate::symbols::strexpr::Part::Ref(_)
        )
    });
    let handler = rest
        .last()
        .is_some_and(|h| matches!(h.kind(), "arrow_function" | "function_expression"));
    if !named && !handler {
        return None;
    }
    // The handler is last; a schema is an object (or names one) among the middle ones.
    let middle = &rest[..rest.len().saturating_sub(1)];
    let params = if method == "registerTool" {
        middle
            .first()
            .filter(|c| c.kind() == "object")
            .and_then(|c| match prop(*c, "inputSchema", src) {
                Some(s) => zod_params(s, root, src),
                None => Some(Vec::new()),
            })
    } else {
        // `tool(name, description?, schema?, annotations?, handler)`: past the description
        // (a string, or a constant of the file holding one), the schema; an object of
        // literal values is the annotations.
        let mut params = Some(Vec::new());
        for a in middle {
            if is_text(*a, root, src) || (a.kind() == "object" && annotations_object(*a)) {
                continue;
            }
            params = zod_params(*a, root, src);
            break;
        }
        params
    };
    Some(RawTool {
        line: line(n),
        name,
        params,
    })
}

/// The attributes written before `n` (`#[tool(…)]`, `#[serde(default)]`), nearest first.
fn attributes_before(n: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut cur = n.prev_named_sibling();
    while let Some(p) = cur {
        match p.kind() {
            "attribute_item" => out.push(p),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        cur = p.prev_named_sibling();
    }
    out
}

/// The attribute `#[name(…)]` among `attrs`: its arguments' text (empty without any).
fn attribute_args(attrs: &[Node<'_>], name: &str, src: &[u8]) -> Option<String> {
    attrs.iter().find_map(|a| {
        let attr = a.named_child(0)?;
        let path = text(attr.named_child(0)?, src);
        if path != name && !path.ends_with(&format!("::{name}")) {
            return None;
        }
        Some(
            attr.child_by_field_name("arguments")
                .map(|t| text(t, src))
                .unwrap_or_default(),
        )
    })
}

/// `key = "value"` among an attribute's arguments, at their top level (not inside a
/// string or a nested list).
fn attr_value(args: &str, key: &str) -> Option<String> {
    let b = args.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ if depth == 1 && args.get(i..).is_some_and(|r| r.starts_with(key)) => {
                let before = args.get(..i).and_then(|b| b.chars().last());
                let rest = args.get(i + key.len()..).unwrap_or("").trim_start();
                if before.is_some_and(|c| c == '(' || c == ',' || c.is_whitespace())
                    && let Some(v) = rest.strip_prefix('=')
                {
                    let v = v.trim_start().strip_prefix('"')?;
                    return v.split('"').next().map(str::to_string);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The parameters of an rmcp argument struct of the file: its fields, required unless an
/// `Option` or `#[serde(default)]`; `None` when the struct is not in the file, or renames
/// or flattens its fields.
fn rust_struct_params(root: Node<'_>, name: &str, src: &[u8]) -> Option<Vec<ToolParam>> {
    let mut stack = vec![root];
    let mut found = None;
    while let Some(n) = stack.pop() {
        if n.kind() == "struct_item"
            && n.child_by_field_name("name")
                .is_some_and(|x| text(x, src) == name)
        {
            found = Some(n);
            break;
        }
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c));
    }
    let st = found?;
    let serde = attribute_args(&attributes_before(st), "serde", src).unwrap_or_default();
    if serde.contains("rename_all") || serde.contains("flatten") {
        return None;
    }
    let all_default = serde.contains("default");
    let body = st.child_by_field_name("body")?;
    if body.kind() != "field_declaration_list" {
        return None;
    }
    let mut out = Vec::new();
    let mut c = body.walk();
    for f in body.named_children(&mut c) {
        if f.kind() != "field_declaration" {
            continue;
        }
        let fserde = attribute_args(&attributes_before(f), "serde", src).unwrap_or_default();
        if fserde.contains("flatten") {
            return None;
        }
        if fserde.contains("skip") && !fserde.contains("skip_serializing") {
            continue;
        }
        let field = text(f.child_by_field_name("name")?, src);
        let ty = text(f.child_by_field_name("type")?, src);
        let optional = all_default || fserde.contains("default") || ty.starts_with("Option<");
        out.push(ToolParam {
            name: attr_value(&fserde, "rename").unwrap_or(field),
            required: Some(!optional),
        });
    }
    out.sort();
    Some(out)
}

/// rmcp tools of a Rust file: methods marked `#[tool(…)]`, named by the method or the
/// attribute's `name`, their parameters those of the `Parameters<T>` struct of the file.
pub(super) fn rust_tools(root: Node<'_>, src: &[u8]) -> Vec<RawTool> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c));
        if n.kind() != "function_item" {
            continue;
        }
        let Some(args) = attribute_args(&attributes_before(n), "tool", src) else {
            continue;
        };
        let Some(fname) = n.child_by_field_name("name").map(|x| text(x, src)) else {
            continue;
        };
        let name = attr_value(&args, "name").unwrap_or(fname);
        // `Parameters(a): Parameters<T>`: the parameters of `T`.
        let mut params = Some(Vec::new());
        if let Some(ps) = n.child_by_field_name("parameters") {
            let mut pc = ps.walk();
            for p in ps.named_children(&mut pc) {
                let Some(t) = p.child_by_field_name("type") else {
                    continue;
                };
                if t.kind() == "generic_type"
                    && t.child_by_field_name("type")
                        .is_some_and(|x| text(x, src) == "Parameters")
                {
                    params = t
                        .child_by_field_name("type_arguments")
                        .and_then(|a| a.named_child(0))
                        .and_then(|a| rust_struct_params(root, &text(a, src), src));
                }
            }
        }
        out.push(RawTool {
            line: line(n),
            name: StrExpr(vec![crate::symbols::strexpr::Part::Lit(name)]),
            params,
        });
    }
    out.sort_by_key(|t| t.line);
    out
}

#[cfg(test)]
mod tests {
    use crate::grammars::{Grammar, parse};
    use crate::symbols::{BodyExpr, Code, extract};

    fn code(lang: &str, src: &str) -> Code {
        let g = Grammar::for_lang(lang).unwrap();
        let tree = parse(g, src.as_bytes()).unwrap();
        extract(g, &tree, src.as_bytes()).unwrap()
    }

    #[test]
    fn named_types_leave_out_primitives_unions_and_library_types() {
        use super::named_type;
        assert_eq!(named_type("User"), Some("User".into()));
        assert_eq!(named_type("Partial< User >"), Some("Partial<User>".into()));
        assert_eq!(named_type("User[]"), Some("User[]".into()));
        assert_eq!(named_type("A & B"), Some("A&B".into()));
        assert_eq!(named_type("A | B"), None);
        assert_eq!(
            named_type("Pick<A, 'a' | 'b'>"),
            Some("Pick<A,'a'|'b'>".into())
        );
        assert_eq!(named_type("unknown"), None);
        assert_eq!(named_type("Record<string, unknown>"), None);
        assert_eq!(
            named_type("{ a: string; 'b c'?: 'x' | 'y' }"),
            Some("{a:string;'b c'?:'x'|'y'}".into())
        );
        assert_eq!(named_type("{}"), None);
        assert_eq!(
            named_type("{ readonly id: string; label?: string }"),
            Some("{readonly id:string;label?:string}".into())
        );
    }

    /// The body of a wrapper three deep: `fetch` reads the `body` of the options it
    /// spreads, `request` puts its parameter there inside an arrow, `post` passes its own,
    /// and the caller passes a local made by a method of the class with a declared type.
    #[test]
    fn bodies_through_wrappers_locals_and_return_types() {
        let c = code(
            "typescript",
            r#"
interface Payload { label?: string }
export class ApiClient {
  constructor(private readonly baseUrl: string) {}
  private async send(url: string, init: RequestInit) {
    return fetch(url, { ...init, signal: null });
  }
  async request<T>(method: string, endpoint: string, body?: unknown): Promise<T> {
    const url = `${this.baseUrl}${endpoint}`;
    const go = (bearer?: string) =>
      this.send(url, { method, body: body ? JSON.stringify(body) : undefined });
    return (await go()) as T;
  }
  post<T>(endpoint: string, body: unknown) {
    return this.request<T>("POST", endpoint, body);
  }
}
class Users {
  constructor(private api: ApiClient) {}
  private toPayload(name: string): Payload { return { label: name }; }
  async save(name: string, dto: Payload) {
    const payload = this.toPayload(name);
    await this.api.post(`/users`, payload);
    await this.api.post(`/users/raw`, dto);
    await this.api.post(`/users/cast`, { label: name } as Payload);
  }
}
"#,
        );
        assert_eq!(
            c.clients[0].body,
            Some(BodyExpr::Opts(1, "body".into())),
            "{:?}",
            c.clients
        );
        let send = c.calls.iter().find(|x| x.name == "send").unwrap();
        assert_eq!(send.bodies.len(), 1);
        assert_eq!(send.bodies[0].key.as_deref(), Some("body"));
        assert_eq!(send.bodies[0].value, BodyExpr::Param(2, None));
        let posts: Vec<String> = c
            .calls
            .iter()
            .filter(|x| x.name == "post")
            .map(|x| format!("{:?}", x.bodies))
            .collect();
        assert_eq!(posts.len(), 3);
        assert!(posts[0].contains("Type(\"Payload\")"), "{posts:?}");
        assert!(
            posts[1].contains("Param(1, Some(\"Payload\"))"),
            "{posts:?}"
        );
        assert!(posts[2].contains("Type(\"Payload\")"), "{posts:?}");
    }

    #[test]
    fn rmcp_tools_with_their_argument_structs() {
        let c = code(
            "rust",
            r##"
use rmcp::{tool, tool_router, handler::server::wrapper::Parameters};

#[derive(Deserialize, schemars::JsonSchema)]
pub struct FindArgs {
    /// What to find.
    pub query: String,
    #[serde(default)]
    pub limit: u32,
    pub kind: Option<String>,
    #[serde(rename = "in")]
    pub within: String,
}

#[tool_router]
impl Server {
    #[tool(description = "Find (name = \"no\")", annotations(read_only_hint = true))]
    async fn find(&self, Parameters(a): Parameters<FindArgs>) -> String {
        String::new()
    }

    /// Plain.
    #[tool(name = "list_all", description = "List")]
    async fn list(&self) -> String {
        String::new()
    }

    async fn helper(&self) {}
}
"##,
        );
        let tools: Vec<String> = c
            .tools
            .iter()
            .map(|t| {
                let ps: Vec<String> = t
                    .params
                    .iter()
                    .flatten()
                    .map(|x| format!("{}={:?}", x.name, x.required))
                    .collect();
                format!("{:?} {}", t.name.0, ps.join(" "))
            })
            .collect();
        assert_eq!(
            tools,
            vec![
                r#"[Lit("find")] in=Some(true) kind=Some(false) limit=Some(false) query=Some(true)"#,
                r#"[Lit("list_all")] "#,
            ]
        );
    }

    /// axios takes the body as its second argument or `data`, got and ky as `json`; a
    /// Feign method sends its `@RequestBody`; `@Validated(Group.class)` is not the default
    /// validation.
    #[test]
    fn bodies_of_client_libraries_and_feign() {
        let c = code(
            "typescript",
            r#"
import axios from 'axios';
import ky from 'ky';
import got from 'got';
interface User { name: string }
export async function save(user: User, raw: User) {
  await axios.post('/users', user);
  await axios({ url: '/users', method: 'put', data: raw });
  await ky.post('/users', { json: user });
  await got.patch('/users/1', { json: user });
}
"#,
        );
        let bodies: Vec<String> = c
            .clients
            .iter()
            .map(|h| format!("{} {:?}", h.what, h.body))
            .collect();
        assert_eq!(
            bodies,
            vec![
                r#"axios.post Some(Param(0, Some("User")))"#,
                r#"axios Some(Param(1, Some("User")))"#,
                r#"ky.post Some(Param(0, Some("User")))"#,
                r#"got.patch Some(Param(0, Some("User")))"#,
            ]
        );
        let j = code(
            "java",
            r#"
package a;
@FeignClient(name = "api")
interface Users {
    @PostMapping("/users") String create(@RequestBody CreateUser body);
}
@RestController
class UsersController {
    @PostMapping("/users") String create(@Validated(Strict.class) @RequestBody CreateUser body) { return ""; }
    @PutMapping("/users") String put(@Validated @RequestBody List<CreateUser> body) { return ""; }
}
"#,
        );
        assert_eq!(j.clients[0].body, Some(BodyExpr::Type("CreateUser".into())));
        let routes: Vec<String> = j
            .routes
            .iter()
            .map(|r| {
                let b = r.body.as_ref().unwrap();
                format!("{} {}", b.ty, b.valid)
            })
            .collect();
        assert_eq!(routes, vec!["CreateUser false", "List<CreateUser> true"]);
    }

    #[test]
    fn mcp_tools_with_literal_zod_and_json_schemas() {
        let c = code(
            "typescript",
            r#"
import { z } from 'zod';
import type { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
export const toolName = 'create_user';
const description = 'Read ' + 'one';
export const inputSchema = z.object({
  name: z.string().min(1),
  email: z.string().email().optional(),
  team: teamField(),
});
export function register(server: McpServer) {
  server.tool(toolName, 'Create a user', inputSchema.shape, async (a) => a);
  server.tool('ping', async () => ({}));
  server.tool('hint', description, { id: z.string() }, { readOnlyHint: true }, async () => ({}));
  server.registerTool('rename', { inputSchema: { id: z.string(), to: z.optional(z.string()) } }, h);
}
const list = [{ name: 'raw', inputSchema: { type: 'object', properties: { q: {}, n: {} }, required: ['q'] } }];
"#,
        );
        let tools: Vec<String> = c
            .tools
            .iter()
            .map(|t| {
                format!(
                    "{:?} {:?}",
                    t.name.0,
                    t.params.as_ref().map(|p| p
                        .iter()
                        .map(|x| format!("{}={:?}", x.name, x.required))
                        .collect::<Vec<_>>())
                )
            })
            .collect();
        assert_eq!(tools.len(), 5, "{tools:#?}");
        assert!(tools[0].contains("Ref(\"toolName\")"), "{tools:#?}");
        assert!(
            tools[0].contains("[\"email=Some(false)\", \"name=Some(true)\", \"team=None\"]"),
            "{tools:#?}"
        );
        assert!(tools[1].contains("ping") && tools[1].contains("Some([])"));
        assert!(
            tools[2].contains("hint") && tools[2].contains("[\"id=Some(true)\"]"),
            "{tools:#?}"
        );
        assert!(
            tools[3].contains("[\"id=Some(true)\", \"to=Some(false)\"]"),
            "{tools:#?}"
        );
        assert!(
            tools[4].contains("[\"n=Some(false)\", \"q=Some(true)\"]"),
            "{tools:#?}"
        );
    }
}
