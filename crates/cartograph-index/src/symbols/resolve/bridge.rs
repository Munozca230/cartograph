//! Bridge matching (see [`crate::bridges`]): runs after calls resolve, with the same
//! bindings, so a channel written as a constant of another file is read through the import
//! that brings it, and a call to a wrapper (`this.handle("save", h)` over a method that
//! calls `ipcMain.handle(channel, …)`) is read through the call resolver.
//!
//! Electron IPC, per member (a single repo is one):
//! - main: `ipcMain.handle|handleOnce` serves `invoke`, `ipcMain.on|once` serves `send`;
//!   `webContents.send` pushes to a renderer listener;
//! - preload: `ipcRenderer.invoke|send|sendSync` use a channel, `ipcRenderer.on|once`
//!   listens to a push; `contextBridge.exposeInMainWorld(W, obj)` exposes `W.a.b`;
//! - renderer: `window.W.a.b(…)` (and aliases) runs an exposure.
//!
//! Bridges: renderer → exposure (`expose`), use → handler (`invoke`, `send`), push → listener
//! (`push`). Endpoints left unmatched carry why (`unmatched`, `unused`, `unexposed`,
//! `dynamic`); findings read them.

use super::{Bindings, Ctx, GraphIx, Hier, Out, caller_of, gate_matches, resolve_call};
use crate::symbols::strexpr::{Part, StrExpr};
use crate::symbols::{BindKind, Call, Def, IpcOp, RustCallPaths};
use cartograph_core::{
    Bridge, BridgeEndpoint, BridgeKind, BridgeRole, Bridges, Confidence, Graph, NodeKind, symbol_id,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// What the matching found: every endpoint, and the bridges matched.
#[derive(Debug, Default)]
pub(super) struct Found {
    pub endpoints: Vec<BridgeEndpoint>,
    pub bridges: Vec<Bridge>,
}

/// Rounds of constant references followed (a constant built from another…).
const MAX_REF_DEPTH: u8 = 4;

/// Endpoint details read by findings (see `cartograph_core::bridge`).
pub const UNMATCHED: &str = "unmatched";
pub const UNUSED: &str = "unused";
pub const UNEXPOSED: &str = "unexposed";
pub const DYNAMIC: &str = "dynamic";

/// The value of a string expression once constants are read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Val {
    /// The text, `*` for each unknown part; empty when the start is unknown.
    key: String,
    confidence: Confidence,
}

impl Val {
    fn dynamic() -> Val {
        Val {
            key: String::new(),
            confidence: Confidence::Heuristic,
        }
    }

    fn known(&self) -> bool {
        !self.key.is_empty()
    }
}

/// Reads constants through imports.
struct Consts<'c, 'a> {
    ctx: &'c Ctx<'a>,
    binds: &'c [Bindings<'a>],
    by_file: Vec<HashMap<&'a str, &'a StrExpr>>,
}

impl<'c, 'a> Consts<'c, 'a> {
    fn new(ctx: &'c Ctx<'a>, binds: &'c [Bindings<'a>]) -> Self {
        let by_file = ctx
            .files
            .iter()
            .map(|f| {
                f.code
                    .consts
                    .iter()
                    .map(|k| (k.name.as_str(), &k.value))
                    .collect()
            })
            .collect();
        Consts {
            ctx,
            binds,
            by_file,
        }
    }

    /// The constant `path` as file `fi` sees it: its own, or one an import brings.
    fn lookup(&self, fi: usize, path: &str, depth: u8) -> Option<(usize, &'a StrExpr)> {
        if depth > MAX_REF_DEPTH {
            return None;
        }
        let own = &self.by_file[fi];
        if let Some(e) = own.get(path) {
            return Some((fi, e));
        }
        // Java: `X` or `Inner.X` inside the class that declares it (`Routes.X`).
        if self.ctx.files[fi].fam == super::Fam::Java {
            let suffix = format!(".{path}");
            let mut by_suffix = own.iter().filter(|(k, _)| k.ends_with(&suffix));
            if let (Some((_, e)), None) = (by_suffix.next(), by_suffix.next()) {
                return Some((fi, e));
            }
        }
        let (head, rest) = match path.split_once('.') {
            Some((h, r)) => (h, Some(r)),
            None => (path, None),
        };
        let join = |name: &str| match rest {
            Some(r) => format!("{name}.{r}"),
            None => name.to_string(),
        };
        let b = &self.binds[fi];
        if let Some(bd) = b.by_name.get(head)
            && !bd.external
        {
            for &t in &bd.files {
                let in_target = match bd.kind {
                    BindKind::Named => match bd.container {
                        Some(c) => format!("{c}.{}", join(bd.imported.unwrap_or(head))),
                        None => join(bd.imported.unwrap_or(head)),
                    },
                    BindKind::Namespace => match rest {
                        Some(r) => r.to_string(),
                        None => continue,
                    },
                    BindKind::Default => match &self.ctx.files[t].code.default_export {
                        Some(d) => join(d),
                        None => continue,
                    },
                    BindKind::Glob => continue,
                };
                if let Some(hit) = self.lookup(t, &in_target, depth + 1) {
                    return Some(hit);
                }
            }
        }
        // Glob imports, and for Java the files of the package (no import needed).
        for &t in b.globs.iter().chain(&b.scope) {
            if let Some(e) = self.by_file[t].get(path) {
                return Some((t, e));
            }
        }
        None
    }

    /// The value of `e` written in file `fi`.
    fn eval(&self, fi: usize, e: &StrExpr, depth: u8) -> Val {
        let mut key = String::new();
        let mut confidence = Confidence::Exact;
        for (i, p) in e.0.iter().enumerate() {
            let part = match p {
                Part::Lit(s) => Some(s.clone()),
                Part::Ref(r) if depth < MAX_REF_DEPTH => match self.lookup(fi, r, depth) {
                    Some((t, inner)) => {
                        let v = self.eval(t, inner, depth + 1);
                        confidence = confidence.max(Confidence::Resolved).max(v.confidence);
                        v.known().then_some(v.key)
                    }
                    None => None,
                },
                _ => None,
            };
            match part {
                Some(s) => key.push_str(&s),
                None if i == 0 => return Val::dynamic(),
                None => {
                    key.push('*');
                    confidence = Confidence::Heuristic;
                }
            }
        }
        if key.is_empty() {
            return Val::dynamic();
        }
        Val { key, confidence }
    }
}

/// `*` in `pattern` stands for any text.
fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut rest = text;
    for (i, p) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(p) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(p);
        } else {
            match rest.find(p) {
                Some(at) => rest = &rest[at + p.len()..],
                None => return false,
            }
        }
    }
    true
}

fn keys_match(a: &str, b: &str) -> bool {
    a == b || (a.contains('*') && glob_match(a, b)) || (b.contains('*') && glob_match(b, a))
}

/// One IPC site once its channel is read.
#[derive(Debug, Clone)]
struct Site {
    member: String,
    op: IpcOp,
    key: String,
    confidence: Confidence,
    /// What made the call: the method, or a wrapper's name.
    label: String,
    file: String,
    line: u32,
    symbol: Option<String>,
}

impl Site {
    fn known(&self) -> bool {
        !self.key.is_empty()
    }

    fn endpoint(&self, detail: Option<&str>) -> BridgeEndpoint {
        BridgeEndpoint {
            kind: BridgeKind::Ipc,
            role: if self.op.provides() {
                BridgeRole::Provide
            } else {
                BridgeRole::Consume
            },
            key: self.key.clone(),
            label: self.label.clone(),
            file: self.file.clone(),
            line: self.line,
            symbol: self.symbol.clone(),
            confidence: self.confidence,
            detail: detail.map(str::to_string),
        }
    }
}

/// The member of a path: its first folder when that is a member, else `""`.
fn member_of<'p>(members: &HashSet<&str>, path: &'p str) -> &'p str {
    match path.split_once('/') {
        Some((m, _)) if members.contains(m) => m,
        _ => "",
    }
}

/// Callable definitions of a file as `(start, end, def, sid)`, for [`caller_of`].
fn callables<'d>(ctx: &Ctx<'d>, fi: usize) -> Vec<(u32, u32, &'d Def, u32)> {
    let f = &ctx.files[fi];
    let mut v: Vec<(u32, u32, &Def, u32)> = f
        .code
        .defs
        .iter()
        .enumerate()
        .filter(|(_, d)| d.kind.is_callable())
        .map(|(i, d)| (d.start, d.end, d, f.first + i as u32))
        .collect();
    v.sort_by_key(|c| (c.0, std::cmp::Reverse(c.1)));
    v
}

/// Match the bridges `on` asks for.
#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    ctx: &Ctx<'_>,
    binds: &[Bindings<'_>],
    h: &Hier<'_>,
    g: &GraphIx<'_>,
    rust_paths: &RustCallPaths,
    graph: &Graph,
    on: Bridges,
) -> Found {
    let mut found = Found::default();
    if on.ipc {
        ipc(ctx, binds, h, g, rust_paths, graph, &mut found);
    }
    found
}

/// `W.path` of a member → `(file, line, symbol)` of the exposed function.
type Exposed = BTreeMap<(String, String, String), (String, u32, Option<String>)>;

fn ipc(
    ctx: &Ctx<'_>,
    binds: &[Bindings<'_>],
    h: &Hier<'_>,
    g: &GraphIx<'_>,
    rust_paths: &RustCallPaths,
    graph: &Graph,
    found: &mut Found,
) {
    let members: HashSet<&str> = graph
        .nodes_of(NodeKind::Dir)
        .filter(|n| !n.path.contains('/') && n.meta.get("member").is_some())
        .map(|n| n.path.as_str())
        .collect();
    let consts = Consts::new(ctx, binds);
    let electron: Vec<bool> = ctx
        .files
        .iter()
        .map(|f| gate_matches("js-ipc", &g.libraries(f.path)))
        .collect();
    let resolve_ref = |fi: usize, c: &Call, caller: Option<&Def>| -> Option<String> {
        match resolve_call(ctx, binds, h, rust_paths, fi, c, caller) {
            Out::Hit(sid, _) => Some(ctx.ids[sid as usize].clone()),
            _ => None,
        }
    };
    let all_callables: Vec<_> = (0..ctx.files.len()).map(|fi| callables(ctx, fi)).collect();

    let mut sites: Vec<Site> = Vec::new();
    // Wrappers: a function passing its parameter `i` as the channel of an IPC call.
    let mut wrappers: HashMap<u32, Vec<(IpcOp, u8)>> = HashMap::new();
    let mut wrapper_names: HashSet<&str> = HashSet::new();
    // The same by symbol id, for exposed functions (`on(channel, cb)` in a preload).
    let mut passthrough: HashMap<String, Vec<(IpcOp, u8)>> = HashMap::new();
    for (fi, f) in ctx.files.iter().enumerate() {
        if f.code.ipc.is_empty() || !electron[fi] {
            continue;
        }
        let member = member_of(&members, f.path).to_string();
        let calls = &all_callables[fi];
        for s in &f.code.ipc {
            let caller = caller_of(calls, s.line);
            if let [Part::Param(i)] = s.channel.0.as_slice() {
                if let Some((d, sid)) = caller {
                    wrappers
                        .entry(ctx.canon[sid as usize])
                        .or_default()
                        .push((s.op, *i));
                    wrapper_names.insert(d.name.as_str());
                    passthrough
                        .entry(ctx.ids[sid as usize].clone())
                        .or_default()
                        .push((s.op, *i));
                }
                continue;
            }
            let v = consts.eval(fi, &s.channel, 0);
            let symbol = s
                .handler
                .as_ref()
                .and_then(|hc| resolve_ref(fi, hc, caller.map(|c| c.0)))
                .or_else(|| caller.map(|(_, sid)| ctx.ids[sid as usize].clone()));
            sites.push(Site {
                member: member.clone(),
                op: s.op,
                key: v.key,
                confidence: v.confidence,
                label: s.op.label().to_string(),
                file: f.path.to_string(),
                line: s.line,
                symbol,
            });
        }
    }
    // Calls to wrappers, in any file.
    if !wrappers.is_empty() {
        for (fi, f) in ctx.files.iter().enumerate() {
            let calls = &all_callables[fi];
            for c in &f.code.calls {
                if c.args.is_empty() || !wrapper_names.contains(c.name.as_str()) {
                    continue;
                }
                let caller = caller_of(calls, c.line);
                let Out::Hit(sid, conf) =
                    resolve_call(ctx, binds, h, rust_paths, fi, c, caller.map(|x| x.0))
                else {
                    continue;
                };
                let Some(ops) = wrappers.get(&ctx.canon[sid as usize]) else {
                    continue;
                };
                for &(op, i) in ops {
                    let Some(Some(arg)) = c.args.get(i as usize) else {
                        continue;
                    };
                    let v = consts.eval(fi, arg, 0);
                    if !v.known() {
                        continue;
                    }
                    let symbol = c
                        .handler
                        .as_ref()
                        .and_then(|hc| resolve_ref(fi, hc, caller.map(|x| x.0)))
                        .or_else(|| caller.map(|(_, s)| ctx.ids[s as usize].clone()));
                    sites.push(Site {
                        member: member_of(&members, f.path).to_string(),
                        op,
                        key: v.key,
                        confidence: v.confidence.max(conf).max(Confidence::Resolved),
                        label: ctx.symbols[sid as usize].qualified(),
                        file: f.path.to_string(),
                        line: c.line,
                        symbol,
                    });
                }
            }
        }
    }

    let exposed = exposures(ctx, &members, &electron);
    for ((_, w, p), (file, line, symbol)) in &exposed {
        found.endpoints.push(BridgeEndpoint {
            kind: BridgeKind::Ipc,
            role: BridgeRole::Provide,
            key: format!("window.{w}.{p}"),
            label: "exposeInMainWorld".into(),
            file: file.clone(),
            line: *line,
            symbol: symbol.clone(),
            confidence: Confidence::Exact,
            detail: None,
        });
    }
    renderer_uses(
        ctx,
        &members,
        &exposed,
        &passthrough,
        &consts,
        &all_callables,
        &mut sites,
        found,
    );
    match_channels(&sites, found);
}

/// Every exposed function, per member.
fn exposures(ctx: &Ctx<'_>, members: &HashSet<&str>, electron: &[bool]) -> Exposed {
    let mut exposed = Exposed::new();
    for (fi, f) in ctx.files.iter().enumerate() {
        if f.code.exposes.is_empty() || !electron[fi] {
            continue;
        }
        let member = member_of(members, f.path).to_string();
        for e in &f.code.exposes {
            let symbol = e.def.as_ref().map(|d| symbol_id(f.path, d));
            exposed
                .entry((member.clone(), e.world.clone(), e.path.clone()))
                .or_insert((f.path.to_string(), e.line, symbol));
        }
    }
    exposed
}

/// Renderer calls on an exposed world: a bridge to the exposed function, or an endpoint
/// that names what is not exposed. An exposed function that passes a parameter on as a
/// channel (`on(channel, cb)`) makes the renderer's argument a channel site.
#[allow(clippy::too_many_arguments)]
fn renderer_uses(
    ctx: &Ctx<'_>,
    members: &HashSet<&str>,
    exposed: &Exposed,
    passthrough: &HashMap<String, Vec<(IpcOp, u8)>>,
    consts: &Consts<'_, '_>,
    all_callables: &[Vec<(u32, u32, &Def, u32)>],
    sites: &mut Vec<Site>,
    found: &mut Found,
) {
    let worlds: BTreeSet<(&str, &str)> = exposed
        .keys()
        .map(|(m, w, _)| (m.as_str(), w.as_str()))
        .collect();
    for (fi, f) in ctx.files.iter().enumerate() {
        if f.code.worlds.is_empty() {
            continue;
        }
        let member = member_of(members, f.path).to_string();
        let calls = &all_callables[fi];
        for u in &f.code.worlds {
            if !worlds.contains(&(member.as_str(), u.world.as_str())) {
                continue;
            }
            let src_sym = caller_of(calls, u.line).map(|(_, s)| ctx.ids[s as usize].clone());
            let key = format!("window.{}.{}", u.world, u.path);
            let confidence = if u.alias {
                Confidence::Resolved
            } else {
                Confidence::Exact
            };
            let endpoint = |detail: Option<&str>| BridgeEndpoint {
                kind: BridgeKind::Ipc,
                role: BridgeRole::Consume,
                key: key.clone(),
                label: format!("window.{}", u.world),
                file: f.path.to_string(),
                line: u.line,
                symbol: src_sym.clone(),
                confidence,
                detail: detail.map(str::to_string),
            };
            let Some((dfile, dline, dsym)) =
                exposed.get(&(member.clone(), u.world.clone(), u.path.clone()))
            else {
                // A property of an exposed object (`api.version.toString()`), or a call on
                // the object itself: no function of its own, not a missing one.
                let near = exposed.keys().any(|(m, w, p)| {
                    *m == member
                        && *w == u.world
                        && (p.starts_with(&format!("{}.", u.path))
                            || u.path.starts_with(&format!("{p}.")))
                });
                if !near {
                    found.endpoints.push(endpoint(Some(UNEXPOSED)));
                }
                continue;
            };
            found.endpoints.push(endpoint(None));
            found.bridges.push(Bridge {
                kind: BridgeKind::Ipc,
                subtype: "expose".into(),
                src_file: f.path.to_string(),
                src_line: u.line,
                src: src_sym.clone(),
                dst_file: dfile.clone(),
                dst_line: *dline,
                dst: dsym.clone(),
                key: key.clone(),
                confidence,
            });
            for (op, i) in dsym
                .as_ref()
                .and_then(|s| passthrough.get(s))
                .into_iter()
                .flatten()
            {
                let Some(Some(arg)) = u.args.get(*i as usize) else {
                    continue;
                };
                let v = consts.eval(fi, arg, 0);
                if !v.known() {
                    continue;
                }
                sites.push(Site {
                    member: member.clone(),
                    op: *op,
                    key: v.key,
                    confidence: v.confidence.max(Confidence::Resolved),
                    label: key.clone(),
                    file: f.path.to_string(),
                    line: u.line,
                    symbol: src_sym.clone(),
                });
            }
        }
    }
}

/// Uses → handlers, pushes → listeners, per member and channel.
fn match_channels(sites: &[Site], found: &mut Found) {
    let serves = |use_op: IpcOp, p: IpcOp| match use_op {
        IpcOp::Invoke => p == IpcOp::Handle,
        IpcOp::Send | IpcOp::SendSync => p == IpcOp::On,
        IpcOp::Push => p == IpcOp::Listen,
        _ => false,
    };
    let providers: Vec<&Site> = sites
        .iter()
        .filter(|s| s.op.provides() && s.known())
        .collect();
    let mut used: HashSet<usize> = HashSet::new();
    for s in sites.iter().filter(|s| !s.op.provides()) {
        if !s.known() {
            found.endpoints.push(s.endpoint(Some(DYNAMIC)));
            continue;
        }
        let same: Vec<(usize, &Site)> = providers
            .iter()
            .enumerate()
            .filter(|(_, p)| p.member == s.member && keys_match(&s.key, &p.key))
            .map(|(i, p)| (i, *p))
            .collect();
        // The method that serves this use; else a handler of the channel registered with
        // the other method (still the code that runs). A push only reaches a listener.
        let exact: Vec<(usize, &Site)> = same
            .iter()
            .copied()
            .filter(|(_, p)| serves(s.op, p.op))
            .collect();
        let hits = if exact.is_empty() && s.op != IpcOp::Push {
            same.into_iter()
                .filter(|(_, p)| p.op != IpcOp::Listen)
                .collect()
        } else {
            exact
        };
        if hits.is_empty() {
            let detail = (s.op != IpcOp::Push).then_some(UNMATCHED);
            found.endpoints.push(s.endpoint(detail));
            continue;
        }
        found.endpoints.push(s.endpoint(None));
        for (i, p) in hits {
            used.insert(i);
            let subtype = match s.op {
                IpcOp::Push => "push",
                IpcOp::Invoke => "invoke",
                _ => "send",
            };
            let glob = s.key.contains('*') || p.key.contains('*');
            found.bridges.push(Bridge {
                kind: BridgeKind::Ipc,
                subtype: subtype.into(),
                src_file: s.file.clone(),
                src_line: s.line,
                src: s.symbol.clone(),
                dst_file: p.file.clone(),
                dst_line: p.line,
                dst: p.symbol.clone(),
                key: if p.key.contains('*') {
                    s.key.clone()
                } else {
                    p.key.clone()
                },
                confidence: if glob {
                    Confidence::Heuristic
                } else {
                    s.confidence.max(p.confidence)
                },
            });
        }
    }
    for (i, p) in providers.iter().enumerate() {
        // A handler nobody calls is reported; a listener nobody pushes to is not.
        let detail = (!used.contains(&i) && p.op != IpcOp::Listen).then_some(UNUSED);
        found.endpoints.push(p.endpoint(detail));
    }
    for s in sites.iter().filter(|s| s.op.provides() && !s.known()) {
        found.endpoints.push(s.endpoint(Some(DYNAMIC)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_either_way() {
        assert!(glob_match("progress:*", "progress:12"));
        assert!(glob_match("a*c*e", "abcde"));
        assert!(!glob_match("a*x", "abc"));
        assert!(keys_match("users:get", "users:get"));
        assert!(keys_match("job:*", "job:done"));
        assert!(keys_match("job:done", "job:*"));
        assert!(!keys_match("job:done", "jobs:*"));
    }
}
