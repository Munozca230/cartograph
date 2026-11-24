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
    /// Server fields the client type lacks: `category (optional)`, `label (required)`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub server_only: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub server_only_more: usize,
    /// Client fields the server type does not read, with the route that does when the
    /// client type goes to several: `tags (UpdateItemDto of PATCH /api/items/{} reads it)`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub client_only: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub client_only_more: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// A name as a naming strategy would leave it: `ownerId`, `owner_id` and `OwnerID`
/// alike.
fn loose(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A route and its client and server types.
type Pair = (String, Option<TypeRef>, Option<TypeRef>);

/// The rows of the bodies whose client or server type is declared in `files`: routes with
/// a difference first.
pub fn rows(bodies: &[HttpBody], files: &BTreeSet<String>) -> Vec<ContractRow> {
    let in_files = |t: &Option<BodyType>| {
        t.as_ref()
            .and_then(|t| t.file.as_ref())
            .is_some_and(|f| files.contains(f))
    };
    // One row per route and pair of types (several call sites may send the same).
    let mut groups: BTreeMap<Pair, Vec<&HttpBody>> = BTreeMap::new();
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

/// The first `n` rows, each list cut to `fields` names (the rest counted): `(rows, rows
/// left out)`.
pub fn cut(rows: &[ContractRow], n: usize, fields: usize) -> (Vec<ContractRow>, usize) {
    let shown = rows
        .iter()
        .take(n)
        .map(|r| {
            let mut r = r.clone();
            let keep = |v: &mut Vec<String>, more: &mut usize| {
                *more += v.len().saturating_sub(fields);
                v.truncate(fields);
            };
            keep(&mut r.server_only, &mut r.server_only_more);
            keep(&mut r.client_only, &mut r.client_only_more);
            r
        })
        .collect();
    (shown, rows.len().saturating_sub(n))
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
        // A client field that differs only in case or separators may be the same one under
        // a naming strategy Cartograph does not read: not certain, not reported.
        let near: BTreeSet<String> = bs
            .iter()
            .filter_map(|b| b.client.as_ref()?.fields.as_ref())
            .flatten()
            .map(|f| loose(&f.name))
            .collect();
        let missing: Vec<String> = fields
            .iter()
            .filter(|f| f.required)
            .filter(|f| {
                cmps.iter()
                    .all(|c| c.server_only.iter().any(|x| x.name == f.name))
                    && !near.contains(&loose(&f.name))
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
/// `member` gives the workspace member of a path (`""` in a single repo): a tool is the
/// same tool anywhere in its member.
pub fn breaks(
    base: &[HttpBody],
    head: &[HttpBody],
    tb: &[McpTool],
    th: &[McpTool],
    member: &dyn Fn(&str) -> String,
) -> Vec<String> {
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
    let tools = |v: &[McpTool]| -> BTreeMap<(String, String), McpTool> {
        v.iter()
            .filter(|t| !t.name.is_empty())
            .map(|t| ((member(&t.file), t.name.clone()), t.clone()))
            .collect()
    };
    let (bt, ht) = (tools(tb), tools(th));
    // Members with a tool whose name is not known: a tool of theirs may have that name.
    let unnamed: BTreeSet<String> = th
        .iter()
        .filter(|t| t.name.is_empty())
        .map(|t| member(&t.file))
        .collect();
    for ((m, name), t) in &bt {
        let Some(h) = ht.get(&(m.clone(), name.clone())) else {
            if unnamed.contains(m) {
                continue;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use cartograph_core::ToolParam;

    fn field(name: &str, required: bool) -> ContractField {
        ContractField {
            name: name.into(),
            aliases: Vec::new(),
            required,
            optional: false,
        }
    }

    fn ty(name: &str, file: &str, fields: Option<Vec<ContractField>>) -> BodyType {
        BodyType {
            written: name.into(),
            name: Some(name.into()),
            file: Some(file.into()),
            line: 1,
            array: false,
            fields,
        }
    }

    fn body(key: &str, client: Option<BodyType>, server: BodyType) -> HttpBody {
        HttpBody {
            src_file: "web/api.ts".into(),
            src_line: 3,
            dst_file: "api/Items.java".into(),
            dst_line: 9,
            key: key.into(),
            client,
            server: Some(server),
        }
    }

    fn payload(fields: &[&str]) -> BodyType {
        ty(
            "Payload",
            "web/types.ts",
            Some(fields.iter().map(|f| field(f, false)).collect()),
        )
    }

    /// One client type for the create and the update: a field only the update reads is
    /// named with its route, never a missing field.
    #[test]
    fn a_client_type_shared_by_two_routes_names_the_one_that_reads_a_field() {
        let create = ty(
            "CreateDto",
            "api/CreateDto.java",
            Some(vec![field("label", true), field("category", false)]),
        );
        let update = ty(
            "UpdateDto",
            "api/UpdateDto.java",
            Some(vec![field("label", false), field("tags", false)]),
        );
        let bodies = vec![
            body("POST /items", Some(payload(&["label", "tags"])), create),
            body("PATCH /items/{}", Some(payload(&["label", "tags"])), update),
        ];
        let files = BTreeSet::from(["api/CreateDto.java".to_string()]);
        let r = rows(&bodies, &files);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].server_only, vec!["category (optional)"]);
        assert_eq!(
            r[0].client_only,
            vec!["tags (UpdateDto of PATCH /items/{} reads it)"]
        );
        assert!(missing_required(&bodies).is_empty());
    }

    /// A required field no client type declares is certain only when every client shape of
    /// the route is known.
    #[test]
    fn missing_required_needs_every_client_shape() {
        let dto = ty("TagDto", "api/TagDto.java", Some(vec![field("name", true)]));
        let known = body("POST /tags", Some(payload(&["hex"])), dto.clone());
        let got = missing_required(std::slice::from_ref(&known));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].2, vec!["name"]);
        // `owner_id` on the server, `ownerId` in the client: maybe a naming strategy.
        let snake = body(
            "POST /p",
            Some(payload(&["ownerId"])),
            ty("P", "api/P.java", Some(vec![field("owner_id", true)])),
        );
        assert!(missing_required(&[snake]).is_empty());
        let mut unknown = known.clone();
        unknown.src_line = 7;
        unknown.client = None;
        assert!(missing_required(&[known, unknown]).is_empty());
    }

    /// A tool whose name is not known may be the one that seems gone: no removal is
    /// reported in its member.
    #[test]
    fn a_tool_of_unknown_name_hides_removals_in_its_member() {
        let tool = |name: &str, file: &str| McpTool {
            name: name.into(),
            file: file.into(),
            line: 4,
            params: None,
        };
        let member = |p: &str| p.split('/').next().unwrap_or("").to_string();
        let tb = vec![tool("archive", "mcp/a.ts"), tool("list", "other/b.ts")];
        let th = vec![tool("", "mcp/a.ts")];
        assert_eq!(
            breaks(&[], &[], &tb, &th, &member),
            vec!["MCP tool `list` removed (was other/b.ts:4)"]
        );
    }

    #[test]
    fn breaks_of_fields_and_tools() {
        let before = body(
            "POST /items",
            Some(payload(&["label", "notes"])),
            ty(
                "CreateDto",
                "api/CreateDto.java",
                Some(vec![field("label", true), field("notes", false)]),
            ),
        );
        let mut after = before.clone();
        after.server = Some(ty(
            "CreateDto",
            "api/CreateDto.java",
            Some(vec![field("label", true), field("details", false)]),
        ));
        let tool = |name: &str, params: &[(&str, Option<bool>)]| McpTool {
            name: name.into(),
            file: "mcp/src/tools.ts".into(),
            line: 4,
            params: Some(
                params
                    .iter()
                    .map(|(n, r)| ToolParam {
                        name: n.to_string(),
                        required: *r,
                    })
                    .collect(),
            ),
        };
        let tb = vec![
            tool(
                "create",
                &[
                    ("label", Some(true)),
                    ("notes", Some(false)),
                    ("team", None),
                ],
            ),
            tool("list", &[]),
        ];
        let th = vec![tool(
            "create",
            &[("label", Some(true)), ("team", Some(true))],
        )];
        let none = |_: &str| String::new();
        let got = breaks(std::slice::from_ref(&before), &[after], &tb, &th, &none);
        assert_eq!(
            got,
            vec![
                "MCP tool `create`: parameter `notes` removed (mcp/src/tools.ts:4)",
                "MCP tool `list` removed (was mcp/src/tools.ts:4)",
                "POST /items api/Items.java: CreateDto no longer reads `notes` (now details?), which Payload (web/api.ts:3) still sends",
            ]
        );
        // Nothing changed, nothing broken.
        let same = std::slice::from_ref(&before);
        assert!(breaks(same, same, &tb, &tb, &none).is_empty());
    }
}
