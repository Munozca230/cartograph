//! Raw records for HTTP bridges (see [`crate::bridges`]): routes a server declares
//! ([`HttpRoute`]: Spring mappings, Express-style registrations), routers mounted under a
//! prefix ([`HttpMount`]), client calls ([`HttpClient`]: `fetch`, axios / got / ky and
//! their instances, RestTemplate, WebClient / RestClient, `java.net.http`, Feign
//! interfaces, Python `requests` / `httpx`) and client instances with a base URL
//! ([`HttpInstance`]). URLs and paths are kept as [`StrExpr`]: constants, parameters (a
//! wrapper) and bases are read after resolution (`symbols::resolve::http`).

use super::bridge::{
    arg_nodes, dotted, frame_of, function_ref, object_opts, str_expr, unwrap_expr,
};
use super::{CallCap, Lang, end_line, line, string_value, text};
use crate::symbols::strexpr::{Part, StrExpr};
use crate::symbols::{
    BindKind, Binding, HttpClient, HttpInstance, HttpMount, HttpRoute, MethodExpr, OptArg,
};
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

/// HTTP verbs as client methods name them.
const VERBS: &[&str] = &["get", "post", "put", "patch", "delete", "head", "options"];
/// Methods of an Express-style app or router that register a route.
const ROUTE_METHODS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "all", "options", "head",
];
/// JS client libraries: the module, as imported.
const JS_LIBS: &[&str] = &["axios", "got", "ky"];

/// What a file holds for HTTP bridges.
#[derive(Debug, Default)]
pub(super) struct Http {
    pub routes: Vec<HttpRoute>,
    pub clients: Vec<HttpClient>,
    pub mounts: Vec<HttpMount>,
    pub instances: Vec<HttpInstance>,
}

/// `GET` for `get`; `None` for what is not a verb.
fn verb(s: &str) -> Option<String> {
    let l = s.to_ascii_lowercase();
    VERBS.contains(&l.as_str()).then(|| l.to_ascii_uppercase())
}

/// A method given as an expression: a verb literal, a parameter, else unknown.
fn method_of(e: &StrExpr) -> MethodExpr {
    match e.0.as_slice() {
        [Part::Lit(s)] => verb(s).map_or(MethodExpr::Unknown, MethodExpr::Lit),
        [Part::Param(i)] => MethodExpr::Param(*i),
        _ => MethodExpr::Unknown,
    }
}

/// The method of an options argument (`fetch(url, init)`): its `method`, the `method` of a
/// parameter it is or spreads, else `GET` (no `method`, no spread).
fn init_method(lang: Lang, init: Option<Node<'_>>, src: &[u8]) -> MethodExpr {
    let Some(init) = init else {
        return MethodExpr::Lit("GET".into());
    };
    let init = unwrap_expr(init);
    if init.kind() == "object" {
        let mut opts: Vec<OptArg> = Vec::new();
        object_opts(init, 0, src, &mut opts);
        if let Some(m) = opts.iter().find(|o| o.key == "method") {
            return method_of(&m.value);
        }
        if let Some(s) = opts.iter().find(|o| o.key == "...") {
            return match s.value.0.as_slice() {
                [Part::Param(i)] => MethodExpr::Opts(*i),
                _ => MethodExpr::Unknown,
            };
        }
        return MethodExpr::Lit("GET".into());
    }
    match str_expr(lang, init, src).0.as_slice() {
        [Part::Param(i)] => MethodExpr::Opts(*i),
        _ => MethodExpr::Unknown,
    }
}

/// `{ url, method }` of a config object (`axios({ url, method })`).
fn config(obj: Node<'_>, src: &[u8]) -> Option<(StrExpr, MethodExpr)> {
    let mut opts: Vec<OptArg> = Vec::new();
    object_opts(obj, 0, src, &mut opts);
    let url = opts.iter().find(|o| o.key == "url")?.value.clone();
    let method = opts
        .iter()
        .find(|o| o.key == "method")
        .map_or(MethodExpr::Lit("GET".into()), |m| method_of(&m.value));
    Some((url, method))
}

fn has_param(m: &MethodExpr) -> bool {
    matches!(m, MethodExpr::Param(_) | MethodExpr::Opts(_))
}

/// A client record; `frame` when the URL or the method comes from a parameter.
#[allow(clippy::too_many_arguments)]
fn client(
    lang: Lang,
    at: Node<'_>,
    call: Node<'_>,
    lib: &str,
    what: String,
    method: MethodExpr,
    url: StrExpr,
    base: Option<StrExpr>,
) -> HttpClient {
    let frame = (url.has_param() || has_param(&method))
        .then(|| frame_of(lang, call).map(end_line))
        .flatten();
    HttpClient {
        line: line(at),
        lib: lib.to_string(),
        what,
        method,
        url,
        base,
        recv: None,
        def: None,
        frame,
    }
}

/// A route or mount path: a literal starting with `/` (or `*`), or a constant.
fn pathish(e: &StrExpr) -> bool {
    match e.0.first() {
        Some(Part::Lit(s)) => s.starts_with('/') || s == "*",
        Some(Part::Ref(r)) => crate::symbols::strexpr::looks_constant(r),
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------
// JavaScript / TypeScript

/// Local names bound to a client library (`import axios from "axios"`), by name.
fn js_lib_names(bindings: &[Binding]) -> HashMap<String, &'static str> {
    let mut out = HashMap::new();
    for b in bindings {
        let Some(spec) = b.spec.as_deref() else {
            continue;
        };
        let Some(lib) = JS_LIBS.iter().find(|l| spec == **l) else {
            continue;
        };
        if matches!(b.kind, BindKind::Default | BindKind::Namespace)
            || (b.kind == BindKind::Named && b.imported.as_deref() == Some("default"))
        {
            out.insert(b.local.clone(), *lib);
        }
    }
    out
}

/// `X.create({ baseURL })` / `X.extend({ prefixUrl })` of a client library: its base.
fn js_instance_of(
    v: Node<'_>,
    src: &[u8],
    libs: &HashMap<String, &'static str>,
) -> Option<(&'static str, Option<StrExpr>)> {
    let v = unwrap_expr(v);
    if v.kind() != "call_expression" {
        return None;
    }
    let f = v.child_by_field_name("function")?;
    if f.kind() != "member_expression" {
        return None;
    }
    let obj = text(f.child_by_field_name("object")?, src);
    let lib = *libs.get(&obj)?;
    let prop = text(f.child_by_field_name("property")?, src);
    if !matches!(prop.as_str(), "create" | "extend") {
        return None;
    }
    let base = arg_nodes(v.child_by_field_name("arguments"))
        .first()
        .map(|a| unwrap_expr(*a))
        .filter(|a| a.kind() == "object")
        .and_then(|obj| {
            let mut c = obj.walk();
            obj.named_children(&mut c).find_map(|p| {
                let k = p.child_by_field_name("key")?;
                matches!(
                    text(k, src).as_str(),
                    "baseURL" | "baseUrl" | "prefixUrl" | "prefixURL"
                )
                .then(|| p.child_by_field_name("value"))
                .flatten()
                .map(|val| str_expr(Lang::Js, val, src))
            })
        });
    Some((lib, base))
}

/// Client instances of a file: `const api = axios.create(…)`, `this.http = axios.create(…)`,
/// a class field `http = axios.create(…)` (as `this.http`).
fn js_instances(
    root: Node<'_>,
    src: &[u8],
    libs: &HashMap<String, &'static str>,
) -> Vec<HttpInstance> {
    let mut out = Vec::new();
    if libs.is_empty() {
        return out;
    }
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let found = match n.kind() {
            "variable_declarator" => n
                .child_by_field_name("name")
                .filter(|x| x.kind() == "identifier")
                .zip(n.child_by_field_name("value"))
                .map(|(name, v)| (text(name, src), v)),
            "assignment_expression" => n
                .child_by_field_name("left")
                .and_then(|l| dotted(l, src))
                .zip(n.child_by_field_name("right"))
                .map(|(l, v)| (l.join("."), v)),
            "public_field_definition" | "field_definition" => n
                .child_by_field_name("name")
                .or_else(|| n.child_by_field_name("property"))
                .zip(n.child_by_field_name("value"))
                .map(|(name, v)| (format!("this.{}", text(name, src)), v)),
            _ => None,
        };
        if let Some((name, v)) = found
            && let Some((lib, base)) = js_instance_of(v, src, libs)
        {
            out.push(HttpInstance {
                line: line(n),
                name,
                lib: lib.to_string(),
                base,
            });
        }
        let mut c = n.walk();
        for ch in n.named_children(&mut c) {
            stack.push(ch);
        }
    }
    out.sort_by_key(|i| i.line);
    out
}

/// A client call through a library or an instance: `axios.get(url)`,
/// `axios.request({ url, method })`, `axios(url, { method })`, `got.post(url)`.
fn js_lib_call(
    c: &CallCap<'_>,
    lib: &str,
    what: String,
    method_name: Option<&str>,
    list: &[Node<'_>],
    src: &[u8],
    base: Option<StrExpr>,
) -> Option<HttpClient> {
    let first = list.first().map(|a| unwrap_expr(*a))?;
    let (url, method) = match method_name.and_then(verb) {
        Some(v) => (str_expr(Lang::Js, first, src), MethodExpr::Lit(v)),
        None if first.kind() == "object" => config(first, src)?,
        None => (
            str_expr(Lang::Js, first, src),
            init_method(Lang::Js, list.get(1).copied(), src),
        ),
    };
    Some(client(
        Lang::Js,
        c.name,
        c.node,
        lib,
        what,
        method,
        url,
        base,
    ))
}

/// HTTP records of a JS/TS file. `bindings` are the file's imports (a receiver imported
/// from another file may be an instance it creates).
pub(super) fn js(calls: &[CallCap<'_>], root: Node<'_>, src: &[u8], bindings: &[Binding]) -> Http {
    let libs = js_lib_names(bindings);
    let imported: HashSet<&str> = bindings
        .iter()
        .filter(|b| !b.local.is_empty() && !libs.contains_key(&b.local))
        .map(|b| b.local.as_str())
        .collect();
    let instances = js_instances(root, src, &libs);
    let mut out = Http::default();
    let inst: HashMap<&str, &HttpInstance> =
        instances.iter().map(|i| (i.name.as_str(), i)).collect();
    let mut clients = Vec::new();
    for c in calls {
        let name = text(c.name, src);
        match c.cap.as_str() {
            "call" => {
                let args = c
                    .node
                    .child_by_field_name("arguments")
                    .filter(|a| a.kind() == "arguments");
                let list = arg_nodes(args);
                if name == "fetch" {
                    let Some(first) = list.first() else { continue };
                    clients.push(client(
                        Lang::Js,
                        c.name,
                        c.node,
                        "fetch",
                        "fetch".into(),
                        init_method(Lang::Js, list.get(1).copied(), src),
                        str_expr(Lang::Js, *first, src),
                        None,
                    ));
                } else if let Some(lib) = libs.get(&name)
                    && let Some(h) = js_lib_call(c, lib, name.clone(), None, &list, src, None)
                {
                    clients.push(h);
                }
            }
            "call.member" => {
                let Some(recv) = c.recv else { continue };
                let Some(r) = dotted(recv, src).map(|v| v.join(".")) else {
                    continue;
                };
                let list = arg_nodes(c.args);
                let what = format!("{r}.{name}");
                let client_method = VERBS.contains(&name.as_str()) || name == "request";
                if name == "fetch" && matches!(r.as_str(), "window" | "globalThis" | "self") {
                    if let Some(first) = list.first() {
                        clients.push(client(
                            Lang::Js,
                            c.name,
                            c.node,
                            "fetch",
                            "fetch".into(),
                            init_method(Lang::Js, list.get(1).copied(), src),
                            str_expr(Lang::Js, *first, src),
                            None,
                        ));
                    }
                    continue;
                }
                if let Some(lib) = libs.get(&r) {
                    if client_method
                        && let Some(h) = js_lib_call(c, lib, what, Some(&name), &list, src, None)
                    {
                        clients.push(h);
                    }
                    continue;
                }
                if let Some(i) = inst.get(r.as_str()) {
                    if client_method
                        && let Some(h) =
                            js_lib_call(c, &i.lib, what, Some(&name), &list, src, i.base.clone())
                    {
                        clients.push(h);
                    }
                    continue;
                }
                if client_method
                    && imported.contains(r.as_str())
                    && let Some(mut h) =
                        js_lib_call(c, "axios", what, Some(&name), &list, src, None)
                {
                    // Confirmed after resolution: the import must lead to an instance.
                    h.recv = Some(r.clone());
                    clients.push(h);
                }
                js_route(c, &r, &name, &list, src, &mut out);
            }
            _ => {}
        }
    }
    out.clients = clients;
    out.instances = instances;
    out
}

/// `recv.get("/path", …, handler)` and `recv.use("/prefix", router)`.
fn js_route(
    c: &CallCap<'_>,
    recv: &str,
    name: &str,
    list: &[Node<'_>],
    src: &[u8],
    out: &mut Http,
) {
    if list.len() < 2 {
        return;
    }
    let path = str_expr(Lang::Js, list[0], src);
    if !pathish(&path) {
        return;
    }
    if name == "use" {
        if let Some(target) = list.last().and_then(|t| dotted(*t, src)) {
            out.mounts.push(HttpMount {
                line: line(c.name),
                recv: recv.to_string(),
                prefix: path,
                target: target.join("."),
            });
        }
        return;
    }
    // The last argument handles the route: a function, or a name of one.
    let handler_like = list.last().is_some_and(|h| {
        matches!(
            unwrap_expr(*h).kind(),
            "identifier"
                | "member_expression"
                | "arrow_function"
                | "function_expression"
                | "function"
                | "call_expression"
        )
    });
    if !ROUTE_METHODS.contains(&name) || !handler_like {
        return;
    }
    let methods = if name == "all" {
        Vec::new()
    } else {
        vec![name.to_ascii_uppercase()]
    };
    out.routes.push(HttpRoute {
        line: line(c.name),
        lib: "express".into(),
        what: format!("{recv}.{name}"),
        methods,
        paths: vec![path],
        prefixes: Vec::new(),
        recv: Some(recv.to_string()),
        def: None,
        handler: list.last().and_then(|h| function_ref(Lang::Js, *h, src)),
    });
}

// ---------------------------------------------------------------------------------------
// Java

/// Annotations of a declaration: `(name, arguments)`.
fn annotations<'t>(decl: Node<'t>, src: &[u8]) -> Vec<(String, Option<Node<'t>>)> {
    let mut out = Vec::new();
    let mut c = decl.walk();
    for ch in decl.children(&mut c) {
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
            let name = name.rsplit('.').next().unwrap_or(&name).to_string();
            out.push((name, a.child_by_field_name("arguments")));
        }
    }
    out
}

/// The values of an annotation element: each string of `{"/a", "/b"}`, or the one.
fn values(v: Node<'_>, src: &[u8]) -> Vec<StrExpr> {
    if v.kind() == "element_value_array_initializer" {
        let mut c = v.walk();
        v.named_children(&mut c)
            .filter(|x| !x.kind().contains("comment"))
            .map(|x| str_expr(Lang::Java, x, src))
            .collect()
    } else {
        vec![str_expr(Lang::Java, v, src)]
    }
}

/// `RequestMethod.GET`, `{RequestMethod.GET, RequestMethod.POST}`, `HttpMethod.PUT`.
fn java_methods(v: Node<'_>, src: &[u8]) -> Vec<String> {
    text(v, src)
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter_map(verb_upper)
        .collect()
}

fn verb_upper(s: &str) -> Option<String> {
    (s.chars().all(|c| c.is_ascii_uppercase()) && VERBS.contains(&s.to_ascii_lowercase().as_str()))
        .then(|| s.to_string())
}

/// An element of an annotation's arguments: `(key, value)`, `value` for a lone one.
fn elements<'t>(args: Option<Node<'t>>, src: &[u8]) -> Vec<(String, Node<'t>)> {
    let Some(args) = args else { return Vec::new() };
    let mut out = Vec::new();
    let mut c = args.walk();
    for ch in args.named_children(&mut c) {
        if ch.kind().contains("comment") {
            continue;
        }
        if ch.kind() == "element_value_pair" {
            if let (Some(k), Some(v)) = (
                ch.child_by_field_name("key"),
                ch.child_by_field_name("value"),
            ) {
                out.push((text(k, src), v));
            }
        } else {
            out.push(("value".to_string(), ch));
        }
    }
    out
}

/// A mapping annotation: its methods (empty for any) and paths.
fn mapping(name: &str, args: Option<Node<'_>>, src: &[u8]) -> Option<(Vec<String>, Vec<StrExpr>)> {
    let fixed = match name {
        "GetMapping" => Some("GET"),
        "PostMapping" => Some("POST"),
        "PutMapping" => Some("PUT"),
        "DeleteMapping" => Some("DELETE"),
        "PatchMapping" => Some("PATCH"),
        "RequestMapping" => None,
        _ => return None,
    };
    let mut methods: Vec<String> = fixed.map(|m| vec![m.to_string()]).unwrap_or_default();
    let mut paths = Vec::new();
    for (k, v) in elements(args, src) {
        match k.as_str() {
            "value" | "path" => paths.extend(values(v, src)),
            "method" if fixed.is_none() => methods.extend(java_methods(v, src)),
            _ => {}
        }
    }
    Some((methods, paths))
}

/// `@FeignClient`: `(name, base)`: its `name` / `value`, and `url` followed by `path`.
fn feign(args: Option<Node<'_>>, src: &[u8]) -> (Option<String>, StrExpr) {
    let mut name = None;
    let mut url = None;
    let mut path = None;
    for (k, v) in elements(args, src) {
        match k.as_str() {
            "name" | "value" => name = string_value(v, src),
            "url" => url = Some(str_expr(Lang::Java, v, src)),
            "path" => path = Some(str_expr(Lang::Java, v, src)),
            _ => {}
        }
    }
    let mut base = url.unwrap_or_else(|| StrExpr::hole("feign"));
    if let Some(p) = path {
        base.extend(p);
    }
    (name, base)
}

/// Routes of Spring controllers and clients of Feign interfaces, in the type declarations
/// under `n`.
fn java_types(n: Node<'_>, src: &[u8], out: &mut Http) {
    let mut c = n.walk();
    for ch in n.named_children(&mut c) {
        match ch.kind() {
            "class_declaration" | "interface_declaration" | "record_declaration" => {
                java_type(ch, src, out);
                if let Some(body) = ch.child_by_field_name("body") {
                    java_types(body, src, out);
                }
            }
            "program" | "class_body" | "interface_body" => java_types(ch, src, out),
            _ => {}
        }
    }
}

fn java_type(decl: Node<'_>, src: &[u8], out: &mut Http) {
    let Some(cname) = decl.child_by_field_name("name").map(|n| text(n, src)) else {
        return;
    };
    let annots = annotations(decl, src);
    let has = |n: &str| annots.iter().any(|(a, _)| a == n);
    let iface = decl.kind() == "interface_declaration";
    let feign_client = iface && has("FeignClient");
    let controller =
        !iface && (has("RestController") || has("Controller") || has("RequestMapping"));
    if !feign_client && !controller {
        return;
    }
    let prefixes: Vec<StrExpr> = annots
        .iter()
        .filter(|(a, _)| a == "RequestMapping")
        .flat_map(|(_, args)| {
            mapping("RequestMapping", *args, src)
                .map(|m| m.1)
                .unwrap_or_default()
        })
        .collect();
    let feign_info = feign_client.then(|| {
        let args = annots
            .iter()
            .find(|(a, _)| a == "FeignClient")
            .and_then(|(_, a)| *a);
        feign(args, src)
    });
    let Some(body) = decl.child_by_field_name("body") else {
        return;
    };
    let mut c = body.walk();
    for m in body.named_children(&mut c) {
        if m.kind() != "method_declaration" {
            continue;
        }
        let Some(mname) = m.child_by_field_name("name").map(|n| text(n, src)) else {
            continue;
        };
        for (aname, args) in annotations(m, src) {
            let Some((methods, paths)) = mapping(&aname, args, src) else {
                continue;
            };
            let def = format!("{cname}.{mname}");
            match &feign_info {
                Some((service, base)) => {
                    let method = match methods.as_slice() {
                        [one] => MethodExpr::Lit(one.clone()),
                        _ => MethodExpr::Unknown,
                    };
                    // A class-level `@RequestMapping` on a Feign interface prefixes its paths.
                    let mut url = prefixes.first().cloned().unwrap_or_default();
                    url.extend(paths.first().cloned().unwrap_or_default());
                    out.clients.push(HttpClient {
                        line: line(m),
                        lib: "feign".into(),
                        what: format!("@FeignClient {cname}"),
                        method,
                        url,
                        base: Some(base.clone()),
                        recv: service.clone(),
                        def: Some(def),
                        frame: None,
                    });
                }
                None => out.routes.push(HttpRoute {
                    line: line(m),
                    lib: "spring".into(),
                    what: format!("@{aname}"),
                    methods,
                    paths,
                    prefixes: prefixes.clone(),
                    recv: None,
                    def: Some(def),
                    handler: None,
                }),
            }
        }
    }
}

/// RestTemplate methods: the method each one sends (`None`: given as the second argument).
fn rest_template(m: &str) -> Option<Option<&'static str>> {
    Some(match m {
        "getForObject" | "getForEntity" => Some("GET"),
        "postForObject" | "postForEntity" | "postForLocation" => Some("POST"),
        "put" => Some("PUT"),
        "delete" => Some("DELETE"),
        "patchForObject" => Some("PATCH"),
        "exchange" | "execute" => None,
        _ => return None,
    })
}

/// A method given as `HttpMethod.X`, a string, or a parameter.
fn java_method_arg(a: Option<Node<'_>>, src: &[u8]) -> MethodExpr {
    let Some(a) = a else {
        return MethodExpr::Unknown;
    };
    let t = text(a, src);
    let last = t.rsplit('.').next().unwrap_or(&t);
    if matches!(a.kind(), "field_access" | "identifier")
        && let Some(v) = verb_upper(last)
    {
        return MethodExpr::Lit(v);
    }
    method_of(&str_expr(Lang::Java, a, src))
}

/// Java client instances: a field or variable built with `.baseUrl(x)` / `.rootUri(x)`.
fn java_instances(calls: &[CallCap<'_>], src: &[u8]) -> Vec<HttpInstance> {
    let mut out = Vec::new();
    for c in calls {
        let name = text(c.name, src);
        if !matches!(name.as_str(), "baseUrl" | "rootUri") {
            continue;
        }
        let Some(arg) = arg_nodes(c.node.child_by_field_name("arguments"))
            .first()
            .copied()
        else {
            continue;
        };
        // Up the builder chain to what it is stored in.
        let mut cur = c.node;
        let mut target = None;
        while let Some(p) = cur.parent() {
            match p.kind() {
                "method_invocation" | "parenthesized_expression" | "cast_expression" => cur = p,
                "variable_declarator" => {
                    target = p.child_by_field_name("name").map(|n| text(n, src));
                    break;
                }
                "assignment_expression" => {
                    target = p.child_by_field_name("left").map(|l| {
                        let t = text(l, src);
                        t.strip_prefix("this.").unwrap_or(&t).to_string()
                    });
                    break;
                }
                _ => break,
            }
        }
        if let Some(t) = target {
            out.push(HttpInstance {
                line: line(c.name),
                name: t,
                lib: if name == "rootUri" {
                    "resttemplate".into()
                } else {
                    "webclient".into()
                },
                base: Some(str_expr(Lang::Java, arg, src)),
            });
        }
    }
    out
}

/// HTTP records of a Java file. `recv_types`: declared types of call receivers.
pub(super) fn java(
    calls: &[CallCap<'_>],
    root: Node<'_>,
    src: &[u8],
    recv_types: &HashMap<usize, String>,
) -> Http {
    let mut out = Http::default();
    java_types(root, src, &mut out);
    out.instances = java_instances(calls, src);
    let inst: HashMap<&str, &HttpInstance> =
        out.instances.iter().map(|i| (i.name.as_str(), i)).collect();
    let mut clients = Vec::new();
    for c in calls {
        if c.cap != "call.member" {
            continue;
        }
        let Some(recv) = c.recv else { continue };
        let name = text(c.name, src);
        let list = arg_nodes(c.node.child_by_field_name("arguments"));
        let recv_text = text(recv, src);
        let recv_name = recv_text.strip_prefix("this.").unwrap_or(&recv_text);
        // RestTemplate (and TestRestTemplate, RestOperations).
        if let Some(fixed) = rest_template(&name) {
            let ty = recv_types.get(&c.node.id()).map(String::as_str);
            let lower = recv_text.to_ascii_lowercase();
            let is_rt = matches!(
                ty,
                Some("RestTemplate" | "TestRestTemplate" | "RestOperations")
            ) || lower.contains("resttemplate")
                || lower.contains("restoperations");
            if is_rt && let Some(first) = list.first() {
                let method = match fixed {
                    Some(m) => MethodExpr::Lit(m.into()),
                    None => java_method_arg(list.get(1).copied(), src),
                };
                clients.push(client(
                    Lang::Java,
                    c.name,
                    c.node,
                    "resttemplate",
                    format!("{recv_name}.{name}"),
                    method,
                    str_expr(Lang::Java, *first, src),
                    inst.get(recv_name).and_then(|i| i.base.clone()),
                ));
            }
            continue;
        }
        // WebClient / RestClient: `x.get().uri(url, …)`, `x.method(HttpMethod.X).uri(…)`.
        if name == "uri"
            && recv.kind() == "method_invocation"
            && let Some(first) = list.first()
        {
            let vname = recv
                .child_by_field_name("name")
                .map(|n| text(n, src))
                .unwrap_or_default();
            let vargs = arg_nodes(recv.child_by_field_name("arguments"));
            let method = match (verb(&vname), vname.as_str(), vargs.as_slice()) {
                (Some(v), _, []) if vname.chars().all(|c| c.is_ascii_lowercase()) => {
                    MethodExpr::Lit(v)
                }
                (_, "method", [a]) => java_method_arg(Some(*a), src),
                _ => continue,
            };
            let owner = recv
                .child_by_field_name("object")
                .map(|o| text(o, src))
                .unwrap_or_default();
            let owner = owner.strip_prefix("this.").unwrap_or(&owner).to_string();
            clients.push(client(
                Lang::Java,
                c.name,
                c.node,
                "webclient",
                format!("{owner}.{vname}().uri"),
                method,
                str_expr(Lang::Java, *first, src),
                inst.get(owner.as_str()).and_then(|i| i.base.clone()),
            ));
            continue;
        }
        // java.net.http: `HttpRequest.newBuilder(uri)…uri(u)…GET()` / `.POST(…)`.
        if name == "newBuilder" && recv_text == "HttpRequest" {
            let mut url = list.first().map(|a| str_expr(Lang::Java, *a, src));
            let mut method = MethodExpr::Lit("GET".into());
            let mut cur = c.node;
            while let Some(p) = cur.parent().filter(|p| p.kind() == "method_invocation") {
                if p.child_by_field_name("object") != Some(cur) {
                    break;
                }
                let pn = p
                    .child_by_field_name("name")
                    .map(|n| text(n, src))
                    .unwrap_or_default();
                let pargs = arg_nodes(p.child_by_field_name("arguments"));
                match pn.as_str() {
                    "uri" => url = pargs.first().map(|a| str_expr(Lang::Java, *a, src)),
                    "GET" | "POST" | "PUT" | "DELETE" => method = MethodExpr::Lit(pn.clone()),
                    "method" => {
                        method = pargs.first().map_or(MethodExpr::Unknown, |a| {
                            method_of(&str_expr(Lang::Java, *a, src))
                        })
                    }
                    _ => {}
                }
                cur = p;
            }
            if let Some(url) = url {
                clients.push(client(
                    Lang::Java,
                    c.name,
                    c.node,
                    "httpclient",
                    "HttpRequest.newBuilder".into(),
                    method,
                    url,
                    None,
                ));
            }
        }
    }
    out.clients.extend(clients);
    out.clients.sort_by_key(|h| h.line);
    out
}

// ---------------------------------------------------------------------------------------
// Python

/// `requests.get(url)`, `httpx.post(url)`, `requests.request("GET", url)`.
pub(super) fn python(calls: &[CallCap<'_>], src: &[u8]) -> Http {
    let mut out = Http::default();
    for c in calls {
        if c.cap != "call.member" {
            continue;
        }
        let Some(recv) = c.recv else { continue };
        let lib = text(recv, src);
        if !matches!(lib.as_str(), "requests" | "httpx") {
            continue;
        }
        let name = text(c.name, src);
        let list = arg_nodes(c.args);
        let (method, url) = match (verb(&name), name.as_str(), list.as_slice()) {
            (Some(v), _, [u, ..]) => (MethodExpr::Lit(v), *u),
            (None, "request", [m, u, ..]) => (method_of(&str_expr(Lang::Python, *m, src)), *u),
            _ => continue,
        };
        out.clients.push(client(
            Lang::Python,
            c.name,
            c.node,
            &lib,
            format!("{lib}.{name}"),
            method,
            str_expr(Lang::Python, url, src),
            None,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::grammars::{Grammar, parse};
    use crate::symbols::strexpr::{Part, StrExpr};
    use crate::symbols::{Code, MethodExpr, extract};

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

    fn method(m: &MethodExpr) -> String {
        match m {
            MethodExpr::Lit(v) => v.clone(),
            MethodExpr::Param(i) => format!("${i}"),
            MethodExpr::Opts(i) => format!("${i}.method"),
            MethodExpr::Unknown => "?".into(),
        }
    }

    fn clients(c: &Code) -> Vec<String> {
        c.clients
            .iter()
            .map(|h| {
                format!(
                    "{} {} {} L{}{}{}",
                    h.what,
                    method(&h.method),
                    show(&h.url),
                    h.line,
                    h.base
                        .as_ref()
                        .map_or(String::new(), |b| format!(" base {}", show(b))),
                    h.frame.map_or(String::new(), |f| format!(" frame {f}"))
                )
            })
            .collect()
    }

    /// A base known at run time, three wrappers deep: the URL of `send` is its parameter,
    /// its method the `method` of the options it spreads; `request` builds the URL in a
    /// local and calls `send` from an arrow (its own parameters are not the arrow's).
    #[test]
    fn a_fetch_wrapper_chain_keeps_parameters_of_the_definition() {
        let c = code(
            "typescript",
            r#"
export class ApiClient {
  constructor(private readonly baseUrl: string) {}
  private async send(url: string, init: RequestInit) {
    return fetch(url, { ...init, signal: null });
  }
  async request<T>(method: string, endpoint: string, body?: unknown): Promise<T> {
    const url = `${this.baseUrl}${endpoint}`;
    const go = (bearer?: string) => this.send(url, { method, headers: { a: bearer } });
    return (await go()) as T;
  }
  get<T>(endpoint: string) {
    return this.request<T>("GET", endpoint);
  }
}
"#,
        );
        assert_eq!(clients(&c), vec!["fetch $1.method $0 L5 frame 6"]);
        let send = c.calls.iter().find(|x| x.name == "send").unwrap();
        assert_eq!(show(send.args[0].as_ref().unwrap()), "@this.baseUrl $1");
        let m: Vec<String> = send
            .opts
            .iter()
            .map(|o| format!("{} {} {}", o.arg, o.key, show(&o.value)))
            .collect();
        assert_eq!(m, vec!["1 method $0"]);
        assert_eq!(send.frame, Some(11));
        let request = c.calls.iter().find(|x| x.name == "request").unwrap();
        let args: Vec<String> = request
            .args
            .iter()
            .map(|a| a.as_ref().map_or("-".into(), show))
            .collect();
        assert_eq!(args, vec!["'GET'", "$0"]);
    }

    /// A URL helper around the parameter (`this.buildUrl(path)`) is an unknown base, then
    /// the parameter; a constant passed to it stays a name to read later.
    #[test]
    fn a_url_helper_is_a_base_before_its_argument() {
        let c = code(
            "typescript",
            r#"
const FINISH = '/jobs/finish';
export class Client {
  private buildUrl(path: string) { return new URL(path, this.base).toString(); }
  private async fetchWithTimeout(url: string, options: RequestInit) {
    return fetch(url, { ...options });
  }
  async post(path: string, body: unknown) {
    const url = this.buildUrl(path);
    return this.retry(async () => this.fetchWithTimeout(url, { method: 'POST', body }));
  }
  async finish() {
    return this.fetchWithTimeout(this.buildUrl(FINISH), { method: 'POST' });
  }
}
"#,
        );
        let calls: Vec<String> = c
            .calls
            .iter()
            .filter(|x| x.name == "fetchWithTimeout")
            .map(|x| {
                format!(
                    "{} {:?}",
                    show(x.args[0].as_ref().unwrap()),
                    x.opts.iter().map(|o| show(&o.value)).collect::<Vec<_>>()
                )
            })
            .collect();
        assert_eq!(
            calls,
            vec![
                "<this.buildUrl> $0 [\"'POST'\"]",
                "<this.buildUrl> @FINISH [\"'POST'\"]"
            ]
        );
    }

    /// An encoded value inside a path is a hole there, not a base: the function is a
    /// client site of its own, not a wrapper.
    #[test]
    fn an_encoded_value_is_a_hole_not_a_base() {
        let c = code(
            "typescript",
            "export function history(id: string) {
  return fetch(`/chat/${encodeURIComponent(id)}/items`);
}
",
        );
        assert_eq!(
            clients(&c),
            vec!["fetch GET '/chat/' <encodeURIComponent(id)> '/items' L2"]
        );
    }

    #[test]
    fn spring_routes_with_constants_arrays_and_methods() {
        let c = code(
            "java",
            r#"
package a;
@RestController
@RequestMapping(Paths.USERS)
public class Users {
    @GetMapping("/{id}") public String get(long id) { return ""; }
    @PostMapping public String create() { return ""; }
    @RequestMapping(value = {"/a", "/b"}, method = {RequestMethod.GET, RequestMethod.PUT})
    public String both() { return ""; }
    @GetMapping(path = Paths.ME, produces = "application/json") public String me() { return ""; }
    public String helper() { return ""; }
}
"#,
        );
        let routes: Vec<String> = c
            .routes
            .iter()
            .map(|r| {
                format!(
                    "{} {:?} {:?} {:?} {}",
                    r.what,
                    r.methods,
                    r.prefixes.iter().map(show).collect::<Vec<_>>(),
                    r.paths.iter().map(show).collect::<Vec<_>>(),
                    r.def.as_deref().unwrap_or("-")
                )
            })
            .collect();
        assert_eq!(
            routes,
            vec![
                "@GetMapping [\"GET\"] [\"@Paths.USERS\"] [\"'/{id}'\"] Users.get",
                "@PostMapping [\"POST\"] [\"@Paths.USERS\"] [] Users.create",
                "@RequestMapping [\"GET\", \"PUT\"] [\"@Paths.USERS\"] [\"'/a'\", \"'/b'\"] Users.both",
                "@GetMapping [\"GET\"] [\"@Paths.USERS\"] [\"@Paths.ME\"] Users.me",
            ]
        );
    }

    #[test]
    fn java_clients_rest_template_web_client_http_client_and_feign() {
        let c = code(
            "java",
            r#"
package a;
@FeignClient(name = "api", url = "${api.url}", path = "/api")
interface UsersClient {
    @GetMapping("/users/{id}") String get(@PathVariable long id);
}
class Calls {
    private final RestTemplate rest;
    private final WebClient web = WebClient.builder().baseUrl("http://localhost:8080").build();
    void run(long id, String base) throws Exception {
        rest.getForObject("/api/users/{id}", String.class, id);
        rest.exchange(base + "/api/users", HttpMethod.POST, null, String.class);
        web.post().uri("/api/users").retrieve();
        HttpRequest.newBuilder().uri(URI.create("http://localhost:8080/api/users/me")).GET().build();
    }
}
"#,
        );
        assert_eq!(
            clients(&c),
            vec![
                "@FeignClient UsersClient GET '/users/{id}' L5 base <\"${api.url}\"> '/api'",
                "rest.getForObject GET '/api/users/{id}' L11",
                "rest.exchange POST $1 '/api/users' L12 frame 15",
                "web.post().uri POST '/api/users' L13 base 'http://localhost:8080'",
                "HttpRequest.newBuilder GET 'http://localhost:8080/api/users/me' L14",
            ]
        );
        assert_eq!(c.clients[0].recv.as_deref(), Some("api"));
        assert_eq!(c.clients[0].def.as_deref(), Some("UsersClient.get"));
    }

    #[test]
    fn express_routes_mounts_axios_instances_and_imported_ones() {
        let c = code(
            "typescript",
            r#"
import express, { Router } from 'express';
import axios from 'axios';
import { api } from './http';
const BASE = process.env.API ?? 'http://localhost:3000';
const local = axios.create({ baseURL: BASE + '/v1' });
const app = express();
const r = Router();
r.get('/jobs/:id', show);
app.use('/v1', r);
app.all('*', (_q, s) => s.end());
function show() {}
export async function calls(id: string) {
  await local.get(`/jobs/${id}`);
  await api.post('/users', {});
  await axios({ url: '/x', method: 'delete' });
  await fetch(BASE + '/health');
}
"#,
        );
        let routes: Vec<String> = c
            .routes
            .iter()
            .map(|r| {
                format!(
                    "{} {:?} {} {}",
                    r.what,
                    r.methods,
                    r.paths.iter().map(show).collect::<Vec<_>>().join(","),
                    r.handler.as_ref().map_or("-", |h| h.name.as_str())
                )
            })
            .collect();
        assert_eq!(
            routes,
            vec!["r.get [\"GET\"] '/jobs/:id' show", "app.all [] '*' -"]
        );
        let mounts: Vec<String> = c
            .mounts
            .iter()
            .map(|m| format!("{} {} {}", m.recv, show(&m.prefix), m.target))
            .collect();
        assert_eq!(mounts, vec!["app '/v1' r"]);
        let inst: Vec<String> = c
            .instances
            .iter()
            .map(|i| format!("{} {}", i.name, i.base.as_ref().map_or("-".into(), show)))
            .collect();
        assert_eq!(inst, vec!["local @BASE '/v1'"]);
        assert_eq!(
            clients(&c),
            vec![
                "local.get GET '/jobs/' $0 L14 base @BASE '/v1' frame 18",
                "api.post POST '/users' L15",
                "axios DELETE '/x' L16",
                "fetch GET @BASE '/health' L17",
            ]
        );
        assert_eq!(c.clients[1].recv.as_deref(), Some("api"));
        assert_eq!(
            c.consts
                .iter()
                .find(|k| k.name == "BASE")
                .map(|k| show(&k.value)),
            Some("'http://localhost:3000'".into())
        );
    }

    #[test]
    fn python_requests_and_httpx() {
        let c = code(
            "python",
            "import requests, httpx\nBASE = 'http://localhost:8000'\nclass C:\n    def get(self, path):\n        return requests.get(BASE + path)\ndef run():\n    httpx.request('PATCH', f'{BASE}/items/1')\n",
        );
        assert_eq!(
            clients(&c),
            vec![
                "requests.get GET @BASE $0 L5 frame 5",
                "httpx.request PATCH @BASE '/items/1' L7",
            ]
        );
    }
}
