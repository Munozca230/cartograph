//! Contracts as queries see them (see [`cartograph_core::contract`]): the bodies of HTTP
//! bridges compared field by field, for `explain` of a body type's file; the findings a
//! comparison makes certain; and what a change breaks, for `diff`.
//!
//! A comparison needs both shapes known whole and alike in being lists or not; anything
//! else is shown as not known, never as a mismatch. A field only the client sends is never
//! a problem (Jackson ignores unknown properties by default): when the same client type
//! goes to several routes (a create and an update), the route whose type does read it is
//! named.

use crate::bridges::at;
use crate::budget::clip;
use cartograph_core::{BodyType, ContractField, HttpBody, McpTool};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Field names shown per list.
const MAX_FIELDS: usize = 8;

/// A type of a contract as answers show it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct TypeRef {
    pub name: String,
    /// `path:line` of its declaration, when it is a type of the repo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub list: bool,
    /// Its fields are known.
    pub known: bool,
}

impl TypeRef {
    fn of(t: &BodyType) -> TypeRef {
        TypeRef {
            name: clip(t.label(), 120),
            at: t.file.as_ref().map(|f| at(f, t.line)),
            list: t.array,
            known: t.fields.is_some(),
        }
    }

    fn show(&self) -> String {
        let list = if self.list { "[]" } else { "" };
        match &self.at {
            Some(a) => format!("{}{list} ({})", self.name, clip(a, 140)),
            None => format!("{}{list}", self.name),
        }
    }
}

/// One route's bodies seen from a type's file: the client type and the server type, and
/// what each has that the other lacks.
#[derive(Debug, Clone, Serialize)]
pub struct ContractRow {
    /// The route: `POST /api/users`.
    pub route: String,
    /// `None`: the client's body type is not known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<TypeRef>,
    /// `None`: the route declares no typed body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<TypeRef>,
    /// Client call sites with this pair.
    pub calls: usize,
    /// Both shapes known and comparable.
    pub compared: bool,
    /// Server fields the client type lacks: `technology (optional)`, `title (required)`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub server_only: Vec<String>,
    #[serde(skip_serializing_if = "crate::explain::is_zero")]
    pub server_only_more: usize,
    /// Client fields the server type does not read, with the route that does when the
    /// client type goes to several: `tags (UpdateItemDto of PATCH /api/items/{} reads it)`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub client_only: Vec<String>,
    #[serde(skip_serializing_if = "crate::explain::is_zero")]
    pub client_only_more: usize,
}

/// The rows of the bodies whose client or server type is declared in `files`: routes with
/// a difference first.
pub fn rows(bodies: &[HttpBody], files: &BTreeSet<String>) -> Vec<ContractRow> {
    let in_files = |t: &Option<BodyType>| {
        t.as_ref()
            .and_then(|t| t.file.as_ref())
            .is_some_and(|f| files.contains(f))
    };
    // One row per route and pair of types (several call sites may send the same).
    let mut groups: BTreeMap<(String, Option<TypeRef>, Option<TypeRef>), Vec<&HttpBody>> =
        BTreeMap::new();
    for b in bodies
        .iter()
        .filter(|b| in_files(&b.client) || in_files(&b.server))
    {
        let key = (
            b.key.clone(),
            b.client.as_ref().map(TypeRef::of),
            b.server.as_ref().map(TypeRef::of),
        );
        groups.entry(key).or_default().push(b);
    }
    let mut out: Vec<ContractRow> = groups
        .into_iter()
        .map(|((route, client, server), bs)| {
            let b = bs[0];
            let cmp = b.compare();
            let (server_only, client_only) = match &cmp {
                Some(c) => (
                    c.server_only
                        .iter()
                        .map(|f| {
                            format!(
                                "{} ({})",
                                f.name,
                                if f.required { "required" } else { "optional" }
                            )
                        })
                        .collect(),
                    c.client_only
                        .iter()
                        .map(|f| match read_elsewhere(bodies, b, f) {
                            Some(w) => format!("{} ({w} reads it)", f.name),
                            None => f.name.clone(),
                        })
                        .collect(),
                ),
                None => (Vec::new(), Vec::new()),
            };
            let cut = |v: Vec<String>| -> (Vec<String>, usize) {
                let more = v.len().saturating_sub(MAX_FIELDS);
                (v.into_iter().take(MAX_FIELDS).collect(), more)
            };
            let (server_only, server_only_more) = cut(server_only);
            let (client_only, client_only_more) = cut(client_only);
            ContractRow {
                route,
                client,
                server,
                calls: bs.len(),
                compared: cmp.is_some(),
                server_only,
                server_only_more,
                client_only,
                client_only_more,
            }
        })
        .collect();
    out.sort_by_key(|r| {
        let differs = !r.server_only.is_empty() || !r.client_only.is_empty();
        (!differs, !r.compared, r.route.clone())
    });
    out
}

/// Another route the same client type goes to whose server type reads `f`: `UpdateDto of
/// PATCH /x`.
fn read_elsewhere(bodies: &[HttpBody], b: &HttpBody, f: &ContractField) -> Option<String> {
    let c = b.client.as_ref()?;
    // The same type, sent alone or in a list.
    let same = |x: &BodyType| match (&c.name, &c.file) {
        (Some(n), Some(f)) => x.name.as_ref() == Some(n) && x.file.as_ref() == Some(f),
        _ => x.written == c.written,
    };
    bodies
        .iter()
        .filter(|o| o.key != b.key || o.server != b.server)
        .filter(|o| o.client.as_ref().is_some_and(same))
        .find_map(|o| {
            let s = o.server.as_ref()?;
            s.fields
                .as_ref()?
                .iter()
                .any(|x| x.answers(&f.name))
                .then(|| format!("{} of {}", s.label(), o.key))
        })
}

/// The lines of [`rows`] for an answer.
pub fn render(s: &mut String, rows: &[ContractRow], more: usize) {
    if rows.is_empty() {
        return;
    }
    s.push_str("contracts (HTTP bodies, client → server):\n");
    for r in rows {
        let client = r
            .client
            .as_ref()
            .map_or("a body of no known type".to_string(), TypeRef::show);
        let server = r
            .server
            .as_ref()
            .map_or("no typed body".to_string(), TypeRef::show);
        let calls = if r.calls == 1 {
            String::new()
        } else {
            format!(", {} calls", r.calls)
        };
        s.push_str(&format!("  {}: {client} → {server}{calls}\n", r.route));
        let list = |v: &[String], more: usize| {
            let mut x = v.join(", ");
            if more > 0 {
                x.push_str(&format!(", +{more} more"));
            }
            x
        };
        if !r.compared {
            if r.client.is_some() && r.server.is_some() {
                s.push_str(
                    "    not compared: a shape is not known whole, or one side is a list
",
                );
            }
            continue;
        }
        if r.server_only.is_empty() && r.client_only.is_empty() {
            s.push_str("    same fields\n");
            continue;
        }
        if !r.server_only.is_empty() {
            s.push_str(&format!(
                "    only the server type has: {}\n",
                list(&r.server_only, r.server_only_more)
            ));
        }
        if !r.client_only.is_empty() {
            s.push_str(&format!(
                "    only the client type has: {}\n",
                list(&r.client_only, r.client_only_more)
            ));
        }
    }
    if more > 0 {
        s.push_str(&format!("  +{more} more routes\n"));
    }
}

/// Required server fields that no client type of the route declares, every client shape of
/// the route known: `(route, server type, fields, the bodies)`.
pub fn missing_required(
    bodies: &[HttpBody],
) -> Vec<(String, &BodyType, Vec<String>, Vec<&HttpBody>)> {
    let mut by_route: BTreeMap<(&str, &str), Vec<&HttpBody>> = BTreeMap::new();
    for b in bodies {
        by_route
            .entry((b.dst_file.as_str(), b.key.as_str()))
            .or_default()
            .push(b);
    }
    let mut out = Vec::new();
    for ((_, key), bs) in by_route {
        // Every body of the route compared: both shapes whole.
        let Some(cmps) = bs.iter().map(|b| b.compare()).collect::<Option<Vec<_>>>() else {
            continue;
        };
        let Some(server) = bs[0].server.as_ref() else {
            continue;
        };
        let Some(fields) = server.fields.as_ref() else {
            continue;
        };
        let missing: Vec<String> = fields
            .iter()
            .filter(|f| f.required)
            .filter(|f| {
                cmps.iter()
                    .all(|c| c.server_only.iter().any(|x| x.name == f.name))
            })
            .map(|f| f.name.clone())
            .collect();
        if !missing.is_empty() {
            out.push((key.to_string(), server, missing, bs));
        }
    }
    out
}

/// What a change breaks in the contracts, against the base: a field the server type no
/// longer reads that a client type still sends, an MCP tool or parameter gone, a parameter
/// newly required.
pub fn breaks(base: &[HttpBody], head: &[HttpBody], tb: &[McpTool], th: &[McpTool]) -> Vec<String> {
    let mut out = BTreeSet::new();
    // Bodies: the same bridge (client file, route, server file) on both sides.
    let key = |b: &HttpBody| (b.src_file.clone(), b.key.clone(), b.dst_file.clone());
    let before: BTreeMap<_, &HttpBody> = base.iter().map(|b| (key(b), b)).collect();
    for h in head {
        let Some(b) = before.get(&key(h)) else {
            continue;
        };
        let (Some(bs), Some(hs)) = (b.server.as_ref(), h.server.as_ref()) else {
            continue;
        };
        if bs.name != hs.name {
            continue;
        }
        let (Some(bf), Some(hf)) = (bs.fields.as_ref(), hs.fields.as_ref()) else {
            continue;
        };
        let Some(cf) = h.client.as_ref().and_then(|c| c.fields.as_ref()) else {
            continue;
        };
        let added: Vec<&str> = hf
            .iter()
            .filter(|f| !bf.iter().any(|x| x.name == f.name))
            .map(|f| f.name.as_str())
            .collect();
        for f in bf {
            let gone = !hf.iter().any(|x| x.answers(&f.name));
            if gone && cf.iter().any(|c| f.answers(&c.name)) {
                let renamed = match added.as_slice() {
                    [one] => format!(" (now {one}?)"),
                    _ => String::new(),
                };
                out.insert(format!(
                    "{} {}: {} no longer reads `{}`{renamed}, which {} ({}) still sends",
                    h.key,
                    h.dst_file,
                    hs.label(),
                    f.name,
                    h.client.as_ref().map_or("", |c| c.label()),
                    at(&h.src_file, h.src_line)
                ));
            }
        }
    }
    // MCP tools, by member and name.
    let member = |f: &str| f.split('/').next().unwrap_or("").to_string();
    let tools = |v: &[McpTool]| -> BTreeMap<(String, String), McpTool> {
        v.iter()
            .map(|t| ((member(&t.file), t.name.clone()), t.clone()))
            .collect()
    };
    let (bt, ht) = (tools(tb), tools(th));
    for ((m, name), t) in &bt {
        let Some(h) = ht.get(&(m.clone(), name.clone())) else {
            // Gone from its file and from every file of the member.
            out.insert(format!(
                "MCP tool `{name}` removed (was {})",
                at(&t.file, t.line)
            ));
            continue;
        };
        let (Some(bp), Some(hp)) = (t.params.as_ref(), h.params.as_ref()) else {
            continue;
        };
        for p in bp {
            match hp.iter().find(|x| x.name == p.name) {
                None => {
                    out.insert(format!(
                        "MCP tool `{name}`: parameter `{}` removed ({})",
                        p.name,
                        at(&h.file, h.line)
                    ));
                }
                Some(x) if p.required == Some(false) && x.required == Some(true) => {
                    out.insert(format!(
                        "MCP tool `{name}`: parameter `{}` is now required ({})",
                        p.name,
                        at(&h.file, h.line)
                    ));
                }
                _ => {}
            }
        }
        for x in hp {
            if x.required == Some(true) && !bp.iter().any(|p| p.name == x.name) {
                out.insert(format!(
                    "MCP tool `{name}`: new required parameter `{}` ({})",
                    x.name,
                    at(&h.file, h.line)
                ));
            }
        }
    }
    out.into_iter().collect()
}
