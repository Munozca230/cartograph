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

/// A type as written, when it may name the shape of an object: whitespace removed; `None`
/// for a primitive, a union, an inline object type or a library type that is no shape.
pub(super) fn named_type(t: &str) -> Option<String> {
    let t: String = t.chars().filter(|c| !c.is_whitespace()).collect();
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
        let found = match n.kind() {
            "function_declaration" | "method_definition" => n
                .child_by_field_name("name")
                .map(|x| (text(x, src), n.child_by_field_name("return_type"))),
            "variable_declarator" | "public_field_definition" => n
                .child_by_field_name("name")
                .or_else(|| n.child_by_field_name("property"))
                .zip(n.child_by_field_name("value"))
                .filter(|(_, v)| matches!(v.kind(), "arrow_function" | "function_expression"))
                .map(|(x, v)| (text(x, src), v.child_by_field_name("return_type"))),
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
                    text(f.child_by_field_name("property")?, src)
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
/// no, a helper (`summaryField()`, `nameSchema`) not known.
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
    // A literal or a name (read through the file's constants after resolution).
    if !name.0.iter().any(|p| {
        matches!(
            p,
            crate::symbols::strexpr::Part::Lit(_) | crate::symbols::strexpr::Part::Ref(_)
        )
    }) {
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
        // `tool(name, description?, schema?, annotations?, handler)`: the schema is the
        // first argument after the name that is not a string.
        match middle.iter().find(|a| !a.kind().contains("string")) {
            None => Some(Vec::new()),
            Some(a) => zod_params(*a, root, src),
        }
    };
    Some(RawTool {
        line: line(n),
        name,
        params,
    })
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
        assert_eq!(named_type("{ a: string }"), None);
    }

    /// The body of a wrapper three deep: `fetch` reads the `body` of the options it
    /// spreads, `request` puts its parameter there inside an arrow, `post` passes its own,
    /// and the caller passes a local made by a method of the class with a declared type.
    #[test]
    fn bodies_through_wrappers_locals_and_return_types() {
        let c = code(
            "typescript",
            r#"
interface Payload { title?: string }
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
  private toPayload(name: string): Payload { return { title: name }; }
  async save(name: string, dto: Payload) {
    const payload = this.toPayload(name);
    await this.api.post(`/users`, payload);
    await this.api.post(`/users/raw`, dto);
    await this.api.post(`/users/cast`, { title: name } as Payload);
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
    fn mcp_tools_with_literal_zod_and_json_schemas() {
        let c = code(
            "typescript",
            r#"
import { z } from 'zod';
import type { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
export const toolName = 'create_user';
export const inputSchema = z.object({
  name: z.string().min(1),
  email: z.string().email().optional(),
  team: teamField(),
});
export function register(server: McpServer) {
  server.tool(toolName, 'Create a user', inputSchema.shape, async (a) => a);
  server.tool('ping', async () => ({}));
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
        assert_eq!(tools.len(), 4, "{tools:#?}");
        assert!(tools[0].contains("Ref(\"toolName\")"), "{tools:#?}");
        assert!(
            tools[0].contains("[\"email=Some(false)\", \"name=Some(true)\", \"team=None\"]"),
            "{tools:#?}"
        );
        assert!(tools[1].contains("ping") && tools[1].contains("Some([])"));
        assert!(
            tools[2].contains("[\"id=Some(true)\", \"to=Some(false)\"]"),
            "{tools:#?}"
        );
        assert!(
            tools[3].contains("[\"n=Some(false)\", \"q=Some(true)\"]"),
            "{tools:#?}"
        );
    }
}
