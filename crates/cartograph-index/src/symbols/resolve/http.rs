//! HTTP bridges (see [`crate::bridges`]): client calls matched to the routes servers
//! declare, across the members of a workspace or inside one repo.
//!
//! Routes: Spring controllers (class prefixes × method paths × methods, constants of any
//! file read through imports), Express-style registrations (routers mounted with
//! `x.use("/prefix", router)`, through imports, chains of up to three mounts), and the
//! HTTP entry points of other frameworks (Flask, FastAPI, NestJS with `setGlobalPrefix`,
//! Go, axum, ASP.NET) as their entries name them. A member's `[member.http] context_path`
//! prefixes its routes.
//!
//! Clients: `fetch`, axios / got / ky (and instances made with `create({ baseURL })`,
//! imported or not), RestTemplate, WebClient / RestClient, `java.net.http`, Feign
//! interfaces, Python `requests` / `httpx`. A function that passes its parameter into a
//! client's URL (with a base before it, or not) or method is a wrapper: each call to it is a
//! client site with the call's arguments in place, up to four wrappers deep
//! (`service → ApiClient.post → ApiClient.request → ApiClient.send → fetch`). A call a
//! resolver cannot tie to its wrapper (`deps.client.post(…)` through an untyped object) is
//! tied by name when one wrapper of the member has that name (heuristic).
//!
//! Matching: paths are cut at `?` / `#`, `{id}`, `:id`, `${…}` and unknown parts are
//! parameters; a literal segment beats a parameter (`/users/me` before `/users/{id}`);
//! methods are equal or either is any. A base only known at run time (`this.baseUrl`) is
//! an unknown base: the route may have leading literal segments the call lacks. Once two
//! calls through one base match routes of one member under one prefix, that base is mapped
//! there: its other calls match only that member and prefix, and a call that matches
//! nothing is `unmatched`. A literal host other than a local one or a member's `hosts` is
//! `external`, never matched.

use super::bridge::{
    Consts, DYNAMIC, Found, MAX_REF_DEPTH, UNMATCHED, UNUSED, callables, glob_match, member_of,
};
use super::{Bindings, Ctx, GraphIx, Hier, Out, Sid, caller_of, gate_matches, resolve_call};
use crate::symbols::strexpr::{Part, StrExpr};
use crate::symbols::{BindKind, Call, Def, Form, MethodExpr, RustCallPaths};
use cartograph_core::{
    Bridge, BridgeEndpoint, BridgeKind, BridgeRole, Confidence, EntryKind, Graph, NodeKind,
    symbol_id,
};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Passes over the calls: the primitives, then one per wrapper level (four deep).
const MAX_ROUNDS: usize = 5;
/// More routes than this tied for a call: no bridge (`ambiguous`).
const MAX_TIES: usize = 3;

/// Endpoint details of HTTP (see `cartograph_core::bridge`), besides [`UNMATCHED`],
/// [`UNUSED`] and [`DYNAMIC`].
pub const EXTERNAL: &str = "external";
pub const NO_SERVER: &str = "no_server";
pub const NO_CLIENTS: &str = "no_clients";
pub const AMBIGUOUS: &str = "ambiguous";
pub const METHOD_MISMATCH: &str = "method_mismatch";

/// A part of a URL once constants are read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RPart {
    Lit(String),
    /// Unknown before run time: its text and the file it is written in.
    Hole(String, usize),
    Param(u8),
}

/// A URL or path as parts, and whether reading it went through a constant, a base or a
/// wrapper.
#[derive(Debug, Clone, Default)]
struct Url {
    parts: Vec<RPart>,
    resolved: bool,
}

impl Url {
    /// A parameter stands for the path (or its end), with at most a base before it: the
    /// function is a wrapper. A base is anything before the parameter that is not a path of
    /// its own: a hole (`this.base`), a scheme and host with at most a path without a
    /// trailing `/` (`http://h/api`), or nothing, each with a `/` of its own after it
    /// (`${base}/${path}`, `path.join(base, p)`). A parameter after a path ending in `/`
    /// (`/users/${id}`), a `?` or a `=` is a value inside a path of its own: a client site.
    fn wraps(&self) -> bool {
        let Some(i) = self.parts.iter().position(|p| matches!(p, RPart::Param(_))) else {
            return false;
        };
        let Some(RPart::Lit(s)) = i.checked_sub(1).map(|j| &self.parts[j]) else {
            return true;
        };
        if s.contains('?') || s.ends_with('=') {
            return false;
        }
        let path = match s.find("://") {
            // The path after the host.
            Some(k) => s[k + 3..].find('/').map_or("", |j| &s[k + 3 + j..]),
            None => s.as_str(),
        };
        if !path.ends_with('/') {
            return true;
        }
        // Only the separator after a base (or after the host): the parameter is the path.
        let before = path.trim_end_matches('/');
        before.is_empty() && (s.contains("://") || path == "/")
    }

    fn push(&mut self, p: RPart) {
        if let (RPart::Lit(s), Some(RPart::Lit(last))) = (&p, self.parts.last_mut()) {
            last.push_str(s);
            return;
        }
        self.parts.push(p);
    }

    fn extend(&mut self, other: Url) {
        self.resolved |= other.resolved;
        for p in other.parts {
            self.push(p);
        }
    }
}

/// `e` written in file `fi`, its constants read.
fn expand(consts: &Consts<'_, '_>, fi: usize, e: &StrExpr, depth: u8) -> Url {
    let mut out = Url::default();
    for p in &e.0 {
        match p {
            Part::Lit(s) => out.push(RPart::Lit(s.clone())),
            Part::Hole(h) => out.push(RPart::Hole(h.clone(), fi)),
            Part::Param(i) => out.push(RPart::Param(*i)),
            Part::Ref(r) => match (depth < MAX_REF_DEPTH)
                .then(|| consts.lookup(fi, r, depth))
                .flatten()
            {
                Some((t, inner)) => {
                    let mut v = expand(consts, t, inner, depth + 1);
                    v.resolved = true;
                    out.extend(v);
                }
                None => out.push(RPart::Hole(r.clone(), fi)),
            },
        }
    }
    out
}

/// One segment of a path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    Lit(String),
    /// A path variable (`{id}`, `:id`, `${id}`) or an unknown part.
    Param,
    /// Literal text around a variable (`file.{ext}`), as a `*` pattern.
    Glob(String),
}

/// Marks an unknown part inside the text of a path.
const HOLE: char = '\u{1}';

/// The segments of a path's text: cut at `?` / `#`, empty segments dropped; `{x}`, `:x`,
/// `<x>` / `<int:x>` (Flask) and holes are parameters; a last `*` / `**` is a tail wildcard (`true`).
fn segments(text: &str) -> (Vec<Seg>, bool) {
    let mut cut = text.find(['?', '#']).map_or(text, |i| &text[..i]);
    // `/items${query}`: an unknown tail glued to a literal is a query string or a suffix,
    // not a segment of its own.
    while let Some(rest) = cut.strip_suffix(HOLE) {
        if rest.is_empty() || rest.ends_with('/') || rest.ends_with(HOLE) {
            break;
        }
        cut = rest;
    }
    let mut segs = Vec::new();
    let mut tail = false;
    let parts: Vec<&str> = cut.split('/').filter(|s| !s.is_empty()).collect();
    for (i, s) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        if last && (*s == "*" || *s == "**") {
            tail = true;
            break;
        }
        let whole_var = (s.starts_with('{') && s.ends_with('}') && s.matches('{').count() == 1)
            || (s.starts_with('<') && s.ends_with('>') && s.matches('<').count() == 1)
            || (s.starts_with(':') && s.len() > 1)
            || *s == HOLE.to_string()
            || *s == "*";
        if whole_var {
            segs.push(Seg::Param);
        } else if s.contains(HOLE) || s.contains('{') || (s.contains('<') && s.contains('>')) {
            let mut pat = String::new();
            let mut in_var = false;
            for ch in s.chars() {
                match ch {
                    '{' | '<' => {
                        in_var = true;
                        pat.push('*');
                    }
                    '}' | '>' => in_var = false,
                    HOLE => pat.push('*'),
                    c if !in_var => pat.push(c),
                    _ => {}
                }
            }
            segs.push(Seg::Glob(pat));
        } else {
            segs.push(Seg::Lit((*s).to_string()));
        }
    }
    (segs, tail)
}

/// A path as people read it: `/users/{}`.
fn template(segs: &[Seg], tail: bool) -> String {
    let mut s = String::new();
    for g in segs {
        s.push('/');
        match g {
            Seg::Lit(l) => s.push_str(l),
            Seg::Param => s.push_str("{}"),
            Seg::Glob(p) => s.push_str(p),
        }
    }
    if tail {
        s.push_str("/**");
    }
    if s.is_empty() {
        s.push('/');
    }
    s
}

/// The text of parts, holes and parameters as one [`HOLE`] each (adjacent ones as one).
fn text_of(parts: &[RPart]) -> String {
    let mut out = String::new();
    for p in parts {
        match p {
            RPart::Lit(s) => out.push_str(s),
            _ if out.ends_with(HOLE) => {}
            _ => out.push(HOLE),
        }
    }
    out
}

/// A route, one method and one path of a declaration.
#[derive(Debug, Clone)]
struct Route {
    member: String,
    file: String,
    line: u32,
    /// `None`: any method.
    method: Option<String>,
    segs: Vec<Seg>,
    tail: bool,
    label: String,
    symbol: Option<String>,
    /// Through a constant, a class prefix, a mount or a context path.
    resolved: bool,
}

impl Route {
    fn key(&self) -> String {
        format!(
            "{} {}",
            self.method.as_deref().unwrap_or("ANY"),
            template(&self.segs, self.tail)
        )
    }
}

/// Where a call's URL goes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Base {
    /// `/path`: the origin the client runs against (per member).
    Root(String),
    /// A local host (`localhost:8080`) or one a member declares.
    Host(String),
    /// A host no member declares: never matched.
    External(String),
    /// Known only at run time: the text of the base and the file it is written in.
    Unknown(String),
}

/// A client call site, its URL read.
#[derive(Debug, Clone)]
struct Site {
    file: String,
    line: u32,
    symbol: Option<String>,
    label: String,
    subtype: String,
    method: Option<String>,
    base: Base,
    segs: Vec<Seg>,
    /// Some part came from a constant, a base or a wrapper.
    resolved: bool,
    /// Tied to its wrapper by name only.
    guessed: bool,
    /// A member the call is known to target (a Feign client named after it).
    target: Option<String>,
    dynamic: bool,
}

impl Site {
    fn key(&self) -> String {
        let path = template(&self.segs, false);
        let m = self.method.as_deref().unwrap_or("ANY");
        match &self.base {
            Base::External(h) => format!("{m} {h}{path}"),
            _ => format!("{m} {path}"),
        }
    }

    fn endpoint(&self, confidence: Confidence, detail: Option<&str>) -> BridgeEndpoint {
        BridgeEndpoint {
            kind: BridgeKind::Http,
            role: BridgeRole::Consume,
            key: if self.dynamic {
                String::new()
            } else {
                self.key()
            },
            label: self.label.clone(),
            file: self.file.clone(),
            line: self.line,
            symbol: self.symbol.clone(),
            confidence,
            detail: detail.map(str::to_string),
        }
    }
}

/// How the repo serves HTTP.
#[derive(Debug, Clone, Default)]
pub struct HttpServers {
    /// `[member.http]` by member (`""` for a single repo), in name order.
    pub members: BTreeMap<String, cartograph_core::MemberHttp>,
}

fn is_local_host(name: &str) -> bool {
    matches!(
        name,
        "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]" | "host.docker.internal"
    )
}

/// The base and the path text of a URL's parts. An absolute URL written after a base
/// (`${base}https://x/…` once a wrapper's base meets an absolute argument) wins.
fn split_base(
    parts: &[RPart],
    member: &str,
    file_of: &dyn Fn(usize) -> String,
    hosts: &HttpServers,
) -> (Base, String) {
    // An absolute URL counts only before the query: `?next=http://…` is a value.
    let mut abs = None;
    for (i, p) in parts.iter().enumerate() {
        if let RPart::Lit(s) = p {
            let q = s.find(['?', '#']);
            if s.find("://").is_some_and(|k| q.is_none_or(|q| k < q)) {
                abs = Some(i);
            }
            if q.is_some() {
                break;
            }
        }
    }
    let parts = match abs {
        Some(i) => {
            let RPart::Lit(s) = &parts[i] else {
                unreachable!()
            };
            let at = s.find("://").map_or(0, |k| {
                // Back to the scheme's start.
                s[..k]
                    .rfind(|c: char| !c.is_ascii_alphabetic())
                    .map_or(0, |b| b + 1)
            });
            let mut v = vec![RPart::Lit(s[at..].to_string())];
            v.extend(parts[i + 1..].iter().cloned());
            v
        }
        None => parts.to_vec(),
    };
    match parts.first() {
        Some(RPart::Hole(h, f)) => (
            Base::Unknown(format!("{h}@{}", file_of(*f))),
            text_of(&parts[1..]),
        ),
        Some(RPart::Lit(_)) => {
            let text = text_of(&parts);
            let lower = text.to_ascii_lowercase();
            let rest = lower
                .strip_prefix("http://")
                .or_else(|| lower.strip_prefix("https://"))
                .or_else(|| lower.strip_prefix("//"));
            let Some(rest) = rest else {
                return if text.starts_with('/') {
                    (Base::Root(member.to_string()), text)
                } else {
                    // `users/1`: relative to whatever page or base runs it.
                    (Base::Unknown(format!("relative@{member}")), text)
                };
            };
            let skip = text.len() - rest.len();
            let host_end = rest.find('/').unwrap_or(rest.len());
            let path = text[skip + host_end..].to_string();
            let host_text = &rest[..host_end];
            // A port only known at run time is still that host; `[::1]:8080` is IPv6.
            let (name, port) = match host_text.strip_prefix('[').and_then(|r| r.split_once(']')) {
                Some((ip, rest)) => (
                    &host_text[..ip.len() + 2],
                    rest.strip_prefix(':').unwrap_or(""),
                ),
                None => host_text.split_once(':').unwrap_or((host_text, "")),
            };
            if name.contains(HOLE) || name.is_empty() {
                return (Base::Unknown(format!("host@{member}")), path);
            }
            let host = if port.is_empty() || port.contains(HOLE) {
                name.to_string()
            } else {
                format!("{name}:{port}")
            };
            // The member that declares `host:port` itself, else the host name alone; members
            // in name order, so the same member always wins.
            let declares = |want: &str| {
                hosts
                    .members
                    .iter()
                    .find(|(_, m)| m.hosts.iter().any(|h| h == want))
                    .map(|(m, _)| m.clone())
            };
            let known = declares(&host).or_else(|| declares(name));
            let base = match known {
                Some(m) => Base::Host(format!("{host}@{m}")),
                None if is_local_host(name) => Base::Host(host),
                None => Base::External(host),
            };
            (base, path)
        }
        _ => (Base::Unknown(String::new()), text_of(&parts)),
    }
}

/// A wrapper: a function whose parameter reaches a client's URL or method.
#[derive(Debug, Clone)]
struct Wrap {
    url: Url,
    method: MethodExpr,
    lib: String,
    target: Option<String>,
}

/// A concrete call before its URL is split: `(file, line, caller sid, label, subtype, url,
/// method, guessed, target)`.
#[derive(Debug, Clone)]
struct Raw {
    fi: usize,
    line: u32,
    caller: Option<Sid>,
    label: String,
    subtype: String,
    url: Url,
    method: MethodExpr,
    guessed: bool,
    target: Option<String>,
}

/// `e` read as a method.
fn method_from(u: &Url) -> MethodExpr {
    match u.parts.as_slice() {
        [RPart::Lit(s)] => {
            let up = s.to_ascii_uppercase();
            if ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"].contains(&up.as_str()) {
                MethodExpr::Lit(up)
            } else {
                MethodExpr::Unknown
            }
        }
        [RPart::Param(i)] => MethodExpr::Param(*i),
        _ => MethodExpr::Unknown,
    }
}

fn method_has_param(m: &MethodExpr) -> bool {
    matches!(m, MethodExpr::Param(_) | MethodExpr::Opts(_))
}

/// A wrapper's URL and method with a call's arguments in place.
fn substitute(consts: &Consts<'_, '_>, fi: usize, w: &Wrap, c: &Call) -> (Url, MethodExpr) {
    let arg = |i: u8| -> Option<Url> {
        c.args
            .get(i as usize)
            .and_then(|a| a.as_ref())
            .map(|e| expand(consts, fi, e, 0))
    };
    let mut url = Url {
        resolved: true,
        ..Url::default()
    };
    for p in &w.url.parts {
        match p {
            RPart::Param(i) => match arg(*i) {
                Some(u) => url.extend(u),
                None => url.push(RPart::Hole(format!("arg{i}"), fi)),
            },
            other => url.push(other.clone()),
        }
    }
    let method = match &w.method {
        MethodExpr::Param(i) => arg(*i).map_or(MethodExpr::Unknown, |u| method_from(&u)),
        MethodExpr::Opts(i) => {
            let of_arg: Vec<_> = c.opts.iter().filter(|o| o.arg == *i).collect();
            if let Some(m) = of_arg.iter().find(|o| o.key == "method") {
                method_from(&expand(consts, fi, &m.value, 0))
            } else if let Some(s) = of_arg.iter().find(|o| o.key == "...") {
                match s.value.0.as_slice() {
                    [Part::Param(k)] => MethodExpr::Opts(*k),
                    _ => MethodExpr::Unknown,
                }
            } else if !of_arg.is_empty() {
                // An object without a method: the client's default.
                MethodExpr::Lit("GET".into())
            } else {
                match arg(*i).map(|u| u.parts) {
                    Some(p) if matches!(p.as_slice(), [RPart::Param(_)]) => match p[0] {
                        RPart::Param(k) => MethodExpr::Opts(k),
                        _ => MethodExpr::Unknown,
                    },
                    // Nothing passed there: the client's default.
                    None if c.args.len() <= *i as usize => MethodExpr::Lit("GET".into()),
                    _ => MethodExpr::Unknown,
                }
            }
        }
        other => other.clone(),
    };
    (url, method)
}

/// Inputs shared by the passes.
struct Cx<'c, 'a> {
    ctx: &'c Ctx<'a>,
    binds: &'c [Bindings<'a>],
    h: &'c Hier<'a>,
    g: &'c GraphIx<'a>,
    rust_paths: &'c RustCallPaths,
    consts: Consts<'c, 'a>,
    members: HashSet<&'c str>,
    hosts: &'c HttpServers,
    calls: Vec<Vec<(u32, u32, &'a Def, u32)>>,
}

impl Cx<'_, '_> {
    fn member(&self, fi: usize) -> String {
        member_of(&self.members, self.ctx.files[fi].path).to_string()
    }

    fn caller(&self, fi: usize, line: u32) -> Option<(&Def, Sid)> {
        caller_of(&self.calls[fi], line)
    }

    fn sym(&self, sid: Sid) -> String {
        self.ctx.ids[sid as usize].clone()
    }
}

/// Match the HTTP bridges of a repo or workspace.
#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    ctx: &Ctx<'_>,
    binds: &[Bindings<'_>],
    h: &Hier<'_>,
    g: &GraphIx<'_>,
    rust_paths: &RustCallPaths,
    graph: &Graph,
    servers: &HttpServers,
    found: &mut Found,
) {
    if !ctx.files.iter().any(|f| {
        !f.code.clients.is_empty() || !f.code.routes.is_empty() || !f.code.entries.is_empty()
    }) {
        return;
    }
    let members: HashSet<&str> = graph
        .nodes_of(NodeKind::Dir)
        .filter(|n| !n.path.contains('/') && n.meta.get(crate::MEMBER_META).is_some())
        .map(|n| n.path.as_str())
        .collect();
    let cx = Cx {
        ctx,
        binds,
        h,
        g,
        rust_paths,
        consts: Consts::new(ctx, binds),
        members,
        hosts: servers,
        calls: (0..ctx.files.len()).map(|fi| callables(ctx, fi)).collect(),
    };
    let routes = routes(&cx);
    let raws = clients(&cx);
    let sites: Vec<Site> = raws.iter().map(|r| site(&cx, r)).collect();
    matching(&routes, &sites, servers, found);
}

// ---------------------------------------------------------------------------------------
// Routes

/// Every route of the repo.
fn routes(cx: &Cx<'_, '_>) -> Vec<Route> {
    let ctx = cx.ctx;
    let mut out = Vec::new();
    let mounted = mount_prefixes(cx);
    // NestJS `app.setGlobalPrefix("api")`, per member.
    let mut nest_prefix: HashMap<String, String> = HashMap::new();
    for (fi, f) in ctx.files.iter().enumerate() {
        for c in &f.code.calls {
            if c.name == "setGlobalPrefix"
                && let Some(Some(e)) = c.args.first()
                && let Some(p) = e.literal()
            {
                nest_prefix.insert(cx.member(fi), p);
            }
        }
    }
    for (fi, f) in ctx.files.iter().enumerate() {
        if f.code.routes.is_empty() && f.code.entries.is_empty() {
            continue;
        }
        let member = cx.member(fi);
        let context = cx
            .hosts
            .members
            .get(&member)
            .and_then(|m| m.context_path.clone())
            .unwrap_or_default();
        let libs = cx.g.libraries(f.path);
        let express_ok = gate_matches("js-http", &libs);
        let mut push = |method: Option<String>,
                        mut url: Url,
                        line: u32,
                        label: String,
                        symbol: Option<String>| {
            if !context.is_empty() {
                let mut c = Url {
                    parts: vec![RPart::Lit(context.clone())],
                    resolved: true,
                };
                c.push(RPart::Lit("/".into()));
                c.extend(url);
                url = c;
            }
            let text = text_of(&url.parts);
            // `:id?`: with and without the segment.
            let variants: Vec<String> = if text.contains('?') && text.contains(':') {
                let without: String = text
                    .split('/')
                    .filter(|s| !(s.starts_with(':') && s.ends_with('?')))
                    .collect::<Vec<_>>()
                    .join("/");
                vec![text.replace('?', ""), without]
            } else {
                vec![text]
            };
            for t in variants {
                let (segs, tail) = segments(&t);
                out.push(Route {
                    member: member.clone(),
                    file: f.path.to_string(),
                    line,
                    method: method.clone(),
                    segs,
                    tail,
                    label: label.clone(),
                    symbol: symbol.clone(),
                    resolved: url.resolved,
                });
            }
        };
        for r in &f.code.routes {
            let is_express = r.lib == "express";
            if is_express && !express_ok {
                continue;
            }
            let symbol = route_symbol(cx, fi, r.def.as_deref(), r.handler.as_ref(), r.line);
            let prefixes: Vec<Url> = if is_express {
                let key = (fi, r.recv.clone().unwrap_or_default());
                match mounted.get(&key) {
                    Some(ps) => ps.clone(),
                    None => vec![Url::default()],
                }
            } else if r.prefixes.is_empty() {
                vec![Url::default()]
            } else {
                r.prefixes
                    .iter()
                    .map(|p| {
                        let mut u = expand(&cx.consts, fi, p, 0);
                        u.resolved = true;
                        u
                    })
                    .collect()
            };
            let paths: Vec<Url> = if r.paths.is_empty() {
                vec![Url::default()]
            } else {
                r.paths
                    .iter()
                    .map(|p| expand(&cx.consts, fi, p, 0))
                    .collect()
            };
            let methods: Vec<Option<String>> = if r.methods.is_empty() {
                vec![None]
            } else {
                r.methods.iter().cloned().map(Some).collect()
            };
            let label = r.what.clone();
            for pre in &prefixes {
                for path in &paths {
                    let mut url = Url {
                        parts: vec![RPart::Lit("/".into())],
                        resolved: false,
                    };
                    url.extend(pre.clone());
                    url.push(RPart::Lit("/".into()));
                    url.extend(path.clone());
                    for m in &methods {
                        push(
                            m.clone(),
                            url.clone(),
                            r.line,
                            label.clone(),
                            symbol.clone(),
                        );
                    }
                }
            }
        }
        // Other frameworks, as their entries name them (literal paths).
        let java = f.fam == super::Fam::Java;
        for e in &f.code.entries {
            if e.kind != EntryKind::Http || java {
                continue;
            }
            if let Some(gate) = &e.gate {
                if gate == "js-http" || !gate_matches(gate, &libs) {
                    continue;
                }
            } else if f.fam == super::Fam::Js {
                continue;
            }
            let Some((verb, path)) = e.name.split_once(' ') else {
                continue;
            };
            let method = (verb != "ANY").then(|| verb.to_string());
            let mut url = Url::default();
            if e.gate.as_deref() == Some("js-nest")
                && let Some(p) = nest_prefix.get(&member)
            {
                url.push(RPart::Lit(format!("/{p}/")));
                url.resolved = true;
            }
            url.push(RPart::Lit(path.to_string()));
            let symbol = route_symbol(cx, fi, e.def.as_deref(), e.handler.as_ref(), e.line);
            push(method, url, e.line, "route".to_string(), symbol);
        }
    }
    out
}

/// The symbol that serves a route: the definition it names, the handler it passes, else
/// the function around the registration.
fn route_symbol(
    cx: &Cx<'_, '_>,
    fi: usize,
    def: Option<&str>,
    handler: Option<&Call>,
    line: u32,
) -> Option<String> {
    let f = &cx.ctx.files[fi];
    if let Some(d) = def {
        return f
            .code
            .defs
            .iter()
            .any(|x| x.qualified() == d)
            .then(|| symbol_id(f.path, d));
    }
    if let Some(hc) = handler
        && let Out::Hit(sid, _) = resolve_call(cx.ctx, cx.binds, cx.h, cx.rust_paths, fi, hc, None)
    {
        return Some(cx.sym(sid));
    }
    cx.caller(fi, line).map(|(_, s)| cx.sym(s))
}

/// A router: the file and the receiver its routes are registered on.
type Router = (usize, String);

/// The prefixes each router (`(file, receiver)`) is mounted under: `app.use("/v1", r)`,
/// through imports, chains of up to three mounts; a router mounted twice has two.
fn mount_prefixes(cx: &Cx<'_, '_>) -> HashMap<(usize, String), Vec<Url>> {
    let ctx = cx.ctx;
    // (mounting file, receiver) → (prefix, (target file, target receiver)).
    let mut edges: Vec<(Router, Url, Router)> = Vec::new();
    for (fi, f) in ctx.files.iter().enumerate() {
        for m in &f.code.mounts {
            let prefix = expand(&cx.consts, fi, &m.prefix, 0);
            for target in mount_targets(cx, fi, &m.target) {
                edges.push(((fi, m.recv.clone()), prefix.clone(), target));
            }
        }
    }
    if edges.is_empty() {
        return HashMap::new();
    }
    let targets: HashSet<(usize, String)> = edges.iter().map(|e| e.2.clone()).collect();
    let mut out: HashMap<(usize, String), Vec<Url>> = HashMap::new();
    // Roots: mounting routers nobody mounts.
    let mut frontier: Vec<((usize, String), Url)> = edges
        .iter()
        .filter(|e| !targets.contains(&e.0))
        .map(|e| (e.0.clone(), Url::default()))
        .collect();
    frontier.sort_by(|a, b| a.0.cmp(&b.0));
    frontier.dedup_by(|a, b| a.0 == b.0);
    for _ in 0..3 {
        let mut next = Vec::new();
        for (from, pre) in &frontier {
            for (src, prefix, target) in &edges {
                if src != from {
                    continue;
                }
                let mut u = pre.clone();
                u.push(RPart::Lit("/".into()));
                u.extend(prefix.clone());
                u.resolved = true;
                out.entry(target.clone()).or_default().push(u.clone());
                next.push((target.clone(), u));
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    out
}

/// The routers a mount target names: a receiver of the same file, or one an import brings.
fn mount_targets(cx: &Cx<'_, '_>, fi: usize, target: &str) -> Vec<(usize, String)> {
    let ctx = cx.ctx;
    let own = &ctx.files[fi].code;
    if own.routes.iter().any(|r| r.recv.as_deref() == Some(target))
        || own.mounts.iter().any(|m| m.recv == target)
    {
        return vec![(fi, target.to_string())];
    }
    let Some(bd) = cx.binds[fi].by_name.get(target) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for &t in &bd.files {
        let code = &ctx.files[t].code;
        let name = match bd.kind {
            BindKind::Named => bd.imported.unwrap_or(target).to_string(),
            BindKind::Default => match &code.default_export {
                Some(d) => d.clone(),
                None => {
                    // `export default router()`-like: the file's only router.
                    let recvs: HashSet<&str> = code
                        .routes
                        .iter()
                        .filter_map(|r| r.recv.as_deref())
                        .collect();
                    match recvs.into_iter().collect::<Vec<_>>().as_slice() {
                        [one] => one.to_string(),
                        _ => continue,
                    }
                }
            },
            _ => continue,
        };
        out.push((t, name));
    }
    out
}

// ---------------------------------------------------------------------------------------
// Clients

/// The base of an instance a receiver imported into `fi` names, when the import leads to
/// one (`import { api } from "./http"` where `export const api = axios.create(…)`).
fn imported_instance(cx: &Cx<'_, '_>, fi: usize, recv: &str) -> Option<(String, Url)> {
    let bd = cx.binds[fi].by_name.get(recv)?;
    for &t in &bd.files {
        let code = &cx.ctx.files[t].code;
        let name = match bd.kind {
            BindKind::Named => bd.imported.unwrap_or(recv).to_string(),
            BindKind::Default => code.default_export.clone()?,
            _ => continue,
        };
        if let Some(i) = code.instances.iter().find(|i| i.name == name) {
            let base = i
                .base
                .as_ref()
                .map(|b| {
                    let mut u = expand(&cx.consts, t, b, 0);
                    u.resolved = true;
                    u
                })
                .unwrap_or_default();
            return Some((i.lib.clone(), base));
        }
    }
    None
}

/// Every concrete client call: primitives, then calls of wrappers, round by round.
fn clients(cx: &Cx<'_, '_>) -> Vec<Raw> {
    let ctx = cx.ctx;
    let mut raws: Vec<Raw> = Vec::new();
    // Wrappers found in the last round, by canonical sid.
    let mut fresh: HashMap<Sid, Vec<Wrap>> = HashMap::new();
    let add_wrap = |fresh: &mut HashMap<Sid, Vec<Wrap>>, sid: Sid, w: Wrap| {
        let v = fresh.entry(ctx.canon[sid as usize]).or_default();
        if v.len() < 4 {
            v.push(w);
        }
    };
    for (fi, f) in ctx.files.iter().enumerate() {
        for h in &f.code.clients {
            let mut url = Url::default();
            let mut lib = h.lib.clone();
            let mut target = None;
            if h.lib == "feign" {
                target = h.recv.clone();
            } else if let Some(recv) = &h.recv {
                let Some((l, base)) = imported_instance(cx, fi, recv) else {
                    continue;
                };
                lib = l;
                url.extend(base);
            }
            let own = expand(&cx.consts, fi, &h.url, 0);
            let absolute = matches!(own.parts.first(), Some(RPart::Lit(s)) if s.contains("://"));
            if let Some(b) = &h.base
                && !absolute
            {
                let mut b = expand(&cx.consts, fi, b, 0);
                b.resolved = true;
                url.extend(b);
            }
            if absolute {
                url = Url::default();
            }
            url.extend(own);
            let caller = cx.caller(fi, h.line);
            let wraps = url.wraps() || method_has_param(&h.method);
            if wraps
                && let Some((d, sid)) = caller
                && h.frame == Some(d.end)
            {
                add_wrap(
                    &mut fresh,
                    sid,
                    Wrap {
                        url,
                        method: h.method.clone(),
                        lib,
                        target,
                    },
                );
                continue;
            }
            // A Feign method is its own call site: the interface method stands for it.
            let caller_sid = match &h.def {
                Some(d) => f
                    .code
                    .defs
                    .iter()
                    .position(|x| x.qualified() == *d)
                    .map(|i| f.first + i as Sid),
                None => caller.map(|(_, s)| s),
            };
            raws.push(Raw {
                fi,
                line: h.line,
                caller: caller_sid,
                label: h.what.clone(),
                subtype: lib,
                url,
                method: h.method.clone(),
                guessed: false,
                target,
            });
        }
    }
    let mut seen: HashSet<(usize, u32, Sid)> = HashSet::new();
    for _ in 1..MAX_ROUNDS {
        if fresh.is_empty() {
            break;
        }
        let names: HashSet<&str> = fresh
            .keys()
            .map(|s| ctx.symbols[*s as usize].name.as_str())
            .collect();
        // Wrappers by member and name, for calls no resolver ties to them.
        let mut by_name: HashMap<(String, &str), Vec<Sid>> = HashMap::new();
        for s in fresh.keys() {
            let owner = ctx.owner[*s as usize];
            by_name
                .entry((cx.member(owner), ctx.symbols[*s as usize].name.as_str()))
                .or_default()
                .push(*s);
        }
        let mut next: HashMap<Sid, Vec<Wrap>> = HashMap::new();
        for (fi, f) in ctx.files.iter().enumerate() {
            for c in &f.code.calls {
                if !names.contains(c.name.as_str()) {
                    continue;
                }
                let caller = cx.caller(fi, c.line);
                let (sid, guessed) = match resolve_call(
                    ctx,
                    cx.binds,
                    cx.h,
                    cx.rust_paths,
                    fi,
                    c,
                    caller.map(|x| x.0),
                ) {
                    Out::Hit(sid, _) => (ctx.canon[sid as usize], false),
                    _ => {
                        // `deps.client.post("/x")`: one wrapper of that name in the member,
                        // and a literal where the URL goes. Not through an import (a
                        // library's `http.post(…)`): the resolver had its chance there.
                        if !matches!(c.form, Form::Member)
                            || c.qualifier
                                .as_deref()
                                .is_some_and(|q| cx.binds[fi].by_name.contains_key(q))
                        {
                            continue;
                        }
                        match by_name
                            .get(&(cx.member(fi), c.name.as_str()))
                            .map(Vec::as_slice)
                        {
                            Some([one]) => (*one, true),
                            _ => continue,
                        }
                    }
                };
                let Some(ws) = fresh.get(&sid) else { continue };
                if !seen.insert((fi, c.line, sid)) {
                    continue;
                }
                for w in ws {
                    // Tied by name: the URL argument must read as a path (a literal or a
                    // constant holding one).
                    let path_arg = |i: u8| {
                        c.args
                            .get(i as usize)
                            .and_then(|a| a.as_ref())
                            .is_some_and(|e| {
                                expand(&cx.consts, fi, e, 0)
                                    .parts
                                    .iter()
                                    .any(|x| matches!(x, RPart::Lit(l) if l.contains('/')))
                            })
                    };
                    if guessed
                        && !w.url.parts.iter().any(|p| match p {
                            RPart::Param(i) => path_arg(*i),
                            _ => false,
                        })
                    {
                        continue;
                    }
                    let (url, method) = substitute(&cx.consts, fi, w, c);
                    let wraps = url.wraps() || method_has_param(&method);
                    if wraps
                        && let Some((d, csid)) = caller
                        && c.frame == Some(d.end)
                    {
                        let v = next.entry(ctx.canon[csid as usize]).or_default();
                        if v.len() < 4 {
                            v.push(Wrap {
                                url,
                                method,
                                lib: w.lib.clone(),
                                target: w.target.clone(),
                            });
                        }
                        continue;
                    }
                    raws.push(Raw {
                        fi,
                        line: c.line,
                        caller: caller.map(|(_, s)| s),
                        label: ctx.symbols[sid as usize].qualified(),
                        subtype: "wrapper".into(),
                        url,
                        method,
                        guessed,
                        target: w.target.clone(),
                    });
                }
            }
        }
        fresh = next;
    }
    raws
}

/// A raw call with its URL split.
fn site(cx: &Cx<'_, '_>, r: &Raw) -> Site {
    let member = cx.member(r.fi);
    // Parameters left (a wrapper nobody calls with a value): unknown parts.
    let parts: Vec<RPart> = r
        .url
        .parts
        .iter()
        .map(|p| match p {
            RPart::Param(i) => RPart::Hole(format!("arg{i}"), r.fi),
            other => other.clone(),
        })
        .collect();
    let file_of = |f: usize| cx.ctx.files[f].path.to_string();
    let (base, path) = split_base(&parts, &member, &file_of, cx.hosts);
    let (segs, _) = segments(&path);
    let literal = segs.iter().filter(|s| matches!(s, Seg::Lit(_))).count();
    let dynamic = literal == 0 || matches!(&base, Base::Unknown(b) if b.is_empty());
    let method = match &r.method {
        MethodExpr::Lit(m) => Some(m.clone()),
        _ => None,
    };
    let target = r.target.clone().filter(|t| cx.members.contains(t.as_str()));
    Site {
        file: cx.ctx.files[r.fi].path.to_string(),
        line: r.line,
        symbol: r.caller.map(|s| cx.sym(s)),
        label: r.label.clone(),
        subtype: r.subtype.clone(),
        method,
        base,
        segs,
        resolved: r.url.resolved,
        guessed: r.guessed,
        target,
        dynamic,
    }
}

// ---------------------------------------------------------------------------------------
// Matching

/// How well route segments match a call's: one rank per segment (2 literal = literal, 1 a
/// parameter or a pattern), the route's leading segments the call lacks (unknown base).
fn path_match(route: &Route, site: &Site, unknown_base: bool) -> Option<(Vec<u8>, usize)> {
    let (rs, cs) = (&route.segs, &site.segs);
    let offset = if unknown_base {
        if rs.len() < cs.len() {
            return None;
        }
        let off = rs.len() - cs.len();
        if !rs[..off].iter().all(|s| matches!(s, Seg::Lit(_))) {
            return None;
        }
        off
    } else {
        if route.tail {
            if cs.len() < rs.len() {
                return None;
            }
        } else if rs.len() != cs.len() {
            return None;
        }
        0
    };
    let mut ranks = Vec::with_capacity(cs.len());
    for (r, c) in rs[offset..].iter().zip(cs.iter()) {
        let rank = match (r, c) {
            (Seg::Lit(a), Seg::Lit(b)) => (a == b).then_some(2),
            (Seg::Param, _) => Some(1),
            (Seg::Lit(_), Seg::Param) => None,
            (Seg::Glob(p), Seg::Lit(b)) | (Seg::Lit(b), Seg::Glob(p)) => {
                glob_match(p, b).then_some(1)
            }
            (Seg::Glob(_), _) => Some(1),
        }?;
        ranks.push(rank);
    }
    Some((ranks, offset))
}

/// A literal that reads as a value, not a name: `42`, a UUID or a long hex id.
fn id_like(s: &str) -> bool {
    !s.is_empty()
        && (s.chars().all(|c| c.is_ascii_digit())
            || (s.len() >= 8 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')))
}

fn methods_agree(route: &Route, site: &Site) -> bool {
    match (&route.method, &site.method) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// The literal segments of a route before `offset`, as a prefix: `/api`.
fn prefix_of(route: &Route, offset: usize) -> String {
    template(&route.segs[..offset], false)
}

/// The best routes for a site among `cands`: indices, and whether a route matched the
/// path with another method.
fn best(
    routes: &[Route],
    cands: &[usize],
    site: &Site,
    unknown_base: bool,
    prefix: Option<&str>,
) -> (Vec<(usize, usize)>, bool) {
    let mut scored: Vec<(Vec<u8>, usize, usize)> = Vec::new();
    let mut other_method = false;
    for &i in cands {
        let r = &routes[i];
        let Some((ranks, off)) = path_match(r, site, unknown_base) else {
            continue;
        };
        if let Some(p) = prefix
            && prefix_of(r, off) != p
        {
            continue;
        }
        if !methods_agree(r, site) {
            // The same path, literal for literal: the call uses the wrong method (not a
            // `/users/clone` read as `/users/{id}`).
            other_method |= site
                .segs
                .iter()
                .zip(&ranks)
                .all(|(c, rank)| *rank == 2 || !matches!(c, Seg::Lit(l) if !id_like(l)));
            continue;
        }
        scored.push((ranks, off, i));
    }
    // Most literal segments first, then the shortest prefix left out.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let Some(top) = scored.first().map(|s| (s.0.clone(), s.1)) else {
        return (Vec::new(), other_method);
    };
    let ties: Vec<(usize, usize)> = scored
        .iter()
        .filter(|s| s.0 == top.0 && s.1 == top.1)
        .map(|s| (s.2, s.1))
        .collect();
    let mismatch = other_method && ties.is_empty();
    (ties, mismatch)
}

fn matching(routes: &[Route], sites: &[Site], servers: &HttpServers, found: &mut Found) {
    let all: Vec<usize> = (0..routes.len()).collect();
    let mut by_member: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, r) in routes.iter().enumerate() {
        by_member.entry(r.member.as_str()).or_default().push(i);
    }
    let cands_of = |s: &Site| -> Vec<usize> {
        let m = match (&s.target, &s.base) {
            (Some(t), _) => Some(t.as_str()),
            (None, Base::Host(h)) => h.split_once('@').map(|(_, m)| m),
            _ => None,
        };
        match m {
            Some(m) => by_member.get(m).cloned().unwrap_or_default(),
            None => all.clone(),
        }
    };
    // First pass: every site against its candidates.
    let first: Vec<(Vec<(usize, usize)>, bool)> = sites
        .iter()
        .map(|s| {
            if s.dynamic || matches!(s.base, Base::External(_)) {
                return (Vec::new(), false);
            }
            let unknown = matches!(s.base, Base::Unknown(_));
            // Without a base, one segment is too little to tell a route from a suffix.
            if unknown && s.segs.len() < 2 {
                return (Vec::new(), false);
            }
            best(routes, &cands_of(s), s, unknown, None)
        })
        .collect();
    // Bases: which member and prefix their matched calls reach.
    let mut votes: BTreeMap<&Base, BTreeMap<(&str, String), usize>> = BTreeMap::new();
    for (s, (hits, _)) in sites.iter().zip(&first) {
        if let [(i, off)] = hits.as_slice() {
            let r = &routes[*i];
            *votes
                .entry(&s.base)
                .or_default()
                .entry((r.member.as_str(), prefix_of(r, *off)))
                .or_default() += 1;
        }
    }
    let mapped: HashMap<&Base, (&str, String)> = votes
        .into_iter()
        .filter_map(|(b, v)| {
            let total: usize = v.values().sum();
            let ((m, p), n) = v
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))?;
            let need = if matches!(b, Base::Unknown(_)) { 2 } else { 1 };
            // Most of the base's matched calls agree on the member and prefix.
            (n >= need && n * 2 > total).then_some((b, (m, p)))
        })
        .collect();
    // Files whose calls through a base reach routes: evidence that the base serves them.
    let file_hits: HashSet<(&Base, &str)> = sites
        .iter()
        .zip(&first)
        .filter(|(_, (h, _))| !h.is_empty() && h.len() <= MAX_TIES)
        .map(|(s, _)| (&s.base, s.file.as_str()))
        .collect();
    let mut used: HashSet<usize> = HashSet::new();
    let mut member_hit: HashSet<&str> = HashSet::new();
    for (si, s) in sites.iter().enumerate() {
        if s.dynamic {
            found
                .endpoints
                .push(s.endpoint(Confidence::Heuristic, Some(DYNAMIC)));
            continue;
        }
        if matches!(s.base, Base::External(_)) {
            found
                .endpoints
                .push(s.endpoint(Confidence::Exact, Some(EXTERNAL)));
            continue;
        }
        let mut map = mapped.get(&s.base);
        let (hits, mismatch) = match map {
            Some((m, p)) if matches!(s.base, Base::Unknown(_)) && s.target.is_none() => {
                let cands = by_member.get(m).cloned().unwrap_or_default();
                let pinned = best(routes, &cands, s, true, Some(p.as_str()));
                if pinned.0.is_empty() && !first[si].0.is_empty() {
                    // Another server behind the same base: what the first pass found, as
                    // a guess.
                    map = None;
                    first[si].clone()
                } else {
                    pinned
                }
            }
            _ => first[si].clone(),
        };
        // A call left without a route is a finding when its server is known: a member it
        // targets, a member's or a local host, or a base whose calls from this file reach
        // routes (another client of the same class may serve another server).
        let declared_host = matches!(&s.base, Base::Host(h) if h.contains('@'));
        let known_server = s.target.is_some()
            || declared_host
            || (map.is_some() && file_hits.contains(&(&s.base, s.file.as_str())));
        let site_conf = if s.resolved || s.guessed || matches!(s.base, Base::Unknown(_)) {
            Confidence::Resolved
        } else {
            Confidence::Exact
        };
        if hits.is_empty() || hits.len() > MAX_TIES {
            let detail = if hits.len() > MAX_TIES {
                AMBIGUOUS
            } else if mismatch {
                METHOD_MISMATCH
            } else if known_server {
                UNMATCHED
            } else {
                NO_SERVER
            };
            found.endpoints.push(s.endpoint(site_conf, Some(detail)));
            continue;
        }
        found.endpoints.push(s.endpoint(site_conf, None));
        let heuristic = s.guessed
            || hits.len() > 1
            || s.method.is_none()
            || (matches!(s.base, Base::Unknown(_)) && map.is_none());
        for (i, _) in &hits {
            let r = &routes[*i];
            used.insert(*i);
            member_hit.insert(r.member.as_str());
            let glob = r
                .segs
                .iter()
                .chain(&s.segs)
                .any(|g| matches!(g, Seg::Glob(_)));
            let confidence = if heuristic || glob || r.method.is_none() {
                Confidence::Heuristic
            } else if site_conf == Confidence::Exact && !r.resolved {
                Confidence::Exact
            } else {
                Confidence::Resolved
            };
            found.bridges.push(Bridge {
                kind: BridgeKind::Http,
                subtype: s.subtype.clone(),
                src_file: s.file.clone(),
                src_line: s.line,
                src: s.symbol.clone(),
                dst_file: r.file.clone(),
                dst_line: r.line,
                dst: r.symbol.clone(),
                key: r.key(),
                confidence,
            });
        }
    }
    for (i, r) in routes.iter().enumerate() {
        // A public member serves clients outside the workspace: a route no member calls is
        // not unused.
        let public = servers.members.get(&r.member).is_some_and(|m| m.public);
        let detail = if used.contains(&i) {
            None
        } else if member_hit.contains(r.member.as_str()) && !public {
            Some(UNUSED)
        } else {
            Some(NO_CLIENTS)
        };
        found.endpoints.push(BridgeEndpoint {
            kind: BridgeKind::Http,
            role: BridgeRole::Provide,
            key: r.key(),
            label: r.label.clone(),
            file: r.file.clone(),
            line: r.line,
            symbol: r.symbol.clone(),
            confidence: if r.resolved {
                Confidence::Resolved
            } else {
                Confidence::Exact
            },
            detail: detail.map(str::to_string),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(s: &str) -> RPart {
        RPart::Lit(s.into())
    }

    fn hole(s: &str) -> RPart {
        RPart::Hole(s.into(), 0)
    }

    fn url(parts: Vec<RPart>) -> Url {
        Url {
            parts,
            resolved: false,
        }
    }

    /// `${base}/${path}` and `path.join(base, p)` pass the path on: a wrapper. A value after
    /// a path of its own (`/users/${id}`) is a path variable.
    #[test]
    fn a_parameter_after_a_base_and_a_slash_is_a_path() {
        assert!(url(vec![hole("this.base"), lit("/"), RPart::Param(0)]).wraps());
        assert!(url(vec![lit("/"), RPart::Param(0)]).wraps());
        assert!(url(vec![lit("http://localhost:8080/"), RPart::Param(0)]).wraps());
        assert!(url(vec![lit("http://localhost:8080/api"), RPart::Param(0)]).wraps());
        assert!(!url(vec![lit("/users/"), RPart::Param(0)]).wraps());
        assert!(!url(vec![hole("b"), lit("/users/"), RPart::Param(0)]).wraps());
        assert!(!url(vec![lit("/users?id="), RPart::Param(0)]).wraps());
    }

    #[test]
    fn flask_variables_are_path_variables() {
        assert_eq!(template(&segments("/users/<id>").0, false), "/users/{}");
        assert_eq!(
            template(&segments("/users/<int:id>/posts").0, false),
            "/users/{}/posts"
        );
        assert_eq!(
            template(&segments("/files/<name>.json").0, false),
            "/files/*.json"
        );
    }

    fn servers(list: &[(&str, &[&str])]) -> HttpServers {
        let mut s = HttpServers::default();
        for (m, hosts) in list {
            s.members.insert(
                m.to_string(),
                cartograph_core::MemberHttp {
                    hosts: hosts.iter().map(|h| h.to_string()).collect(),
                    ..Default::default()
                },
            );
        }
        s
    }

    fn base_of(parts: Vec<RPart>, s: &HttpServers) -> (Base, String) {
        split_base(&parts, "web", &|_| "a.ts".to_string(), s)
    }

    /// An absolute URL inside a query string is a value, not where the call goes.
    #[test]
    fn a_url_in_the_query_is_not_the_host() {
        let s = HttpServers::default();
        let (b, path) = base_of(vec![lit("/auth/login?next=http://localhost:3000/home")], &s);
        assert_eq!(b, Base::Root("web".into()));
        assert!(path.starts_with("/auth/login"), "{path}");
        // After a base, an absolute argument wins.
        let (b, path) = base_of(vec![hole("base"), lit("https://x.example.com/v1/a")], &s);
        assert_eq!(b, Base::External("x.example.com".into()));
        assert_eq!(path, "/v1/a");
    }

    /// Hosts of members: an exact `host:port` first, then the host name alone, the same
    /// whatever the map's order; `[::1]` is a local host.
    #[test]
    fn hosts_pick_the_exact_member_and_read_ipv6() {
        let s = servers(&[("b", &["localhost"]), ("a", &["localhost:8080"])]);
        for _ in 0..8 {
            let (b, _) = base_of(vec![lit("http://localhost:8080/x")], &s);
            assert_eq!(b, Base::Host("localhost:8080@a".into()));
            let (b, _) = base_of(vec![lit("http://localhost:9000/x")], &s);
            assert_eq!(b, Base::Host("localhost:9000@b".into()));
        }
        let none = HttpServers::default();
        let (b, path) = base_of(vec![lit("http://[::1]:8080/api/x")], &none);
        assert_eq!(b, Base::Host("[::1]:8080".into()));
        assert_eq!(path, "/api/x");
    }

    fn route(method: Option<&str>, path: &str) -> Route {
        let (segs, tail) = segments(path);
        Route {
            member: "api".into(),
            file: "a.java".into(),
            line: 1,
            method: method.map(str::to_string),
            segs,
            tail,
            label: String::new(),
            symbol: None,
            resolved: false,
        }
    }

    fn site(method: Option<&str>, base: Base, path: &str) -> Site {
        Site {
            file: "a.ts".into(),
            line: 1,
            symbol: None,
            label: String::new(),
            subtype: "fetch".into(),
            method: method.map(str::to_string),
            base,
            segs: segments(path).0,
            resolved: false,
            guessed: false,
            target: None,
            dynamic: false,
        }
    }

    #[test]
    fn segments_normalise_variables_queries_and_slashes() {
        assert_eq!(template(&segments("/users/{id}/").0, false), "/users/{}");
        assert_eq!(template(&segments("users/:id?x=1").0, false), "/users/{}");
        assert_eq!(
            template(&segments(&format!("//users/{HOLE}#top")).0, false),
            "/users/{}"
        );
        assert_eq!(
            template(&segments("/files/{name}.{ext}").0, false),
            "/files/*.*"
        );
        let (s, tail) = segments("/static/**");
        assert_eq!((template(&s, tail), tail), ("/static/**".to_string(), true));
    }

    #[test]
    fn a_literal_segment_beats_a_variable() {
        let routes = vec![
            route(Some("GET"), "/users/{id}"),
            route(Some("GET"), "/users/me"),
        ];
        let s = site(Some("GET"), Base::Root("web".into()), "/users/me");
        let (hits, _) = best(&routes, &[0, 1], &s, false, None);
        assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), vec![1]);
        let s = site(
            Some("GET"),
            Base::Root("web".into()),
            &format!("/users/{HOLE}"),
        );
        let (hits, _) = best(&routes, &[0, 1], &s, false, None);
        assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), vec![0]);
    }

    #[test]
    fn another_method_is_a_mismatch_not_a_match() {
        let routes = vec![route(Some("POST"), "/users")];
        let s = site(Some("PATCH"), Base::Root("web".into()), "/users");
        let (hits, mismatch) = best(&routes, &[0], &s, false, None);
        assert!(hits.is_empty() && mismatch);
    }

    #[test]
    fn an_unknown_base_may_leave_out_leading_literal_segments() {
        let routes = vec![route(Some("GET"), "/api/v1/jobs/{id}")];
        let s = site(
            Some("GET"),
            Base::Unknown("x".into()),
            &format!("/jobs/{HOLE}"),
        );
        let (hits, _) = best(&routes, &[0], &s, true, None);
        assert_eq!(hits, vec![(0, 2)]);
        assert_eq!(prefix_of(&routes[0], 2), "/api/v1");
        // Pinned to a prefix: another one is no match.
        let (hits, _) = best(&routes, &[0], &s, true, Some("/api"));
        assert!(hits.is_empty());
    }
}
