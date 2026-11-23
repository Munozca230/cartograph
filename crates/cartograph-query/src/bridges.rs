//! Bridges as queries see them (see [`cartograph_core::bridge`]): the endpoints and the
//! matched bridges of the symbol layer, indexed by symbol and by file.

use cartograph_core::{Bridge, BridgeEndpoint, BridgeKind, Symbols};
use std::collections::HashMap;

/// Endpoint details the indexer writes (`cartograph_index::symbols::resolve::bridge`).
pub const UNMATCHED: &str = "unmatched";
pub const UNUSED: &str = "unused";
pub const UNEXPOSED: &str = "unexposed";
pub const DYNAMIC: &str = "dynamic";

/// The bridges of an index, indexed.
#[derive(Debug, Default)]
pub struct BridgeTable {
    pub endpoints: Vec<BridgeEndpoint>,
    pub bridges: Vec<Bridge>,
    /// Destination symbol → bridges.
    into_sym: HashMap<String, Vec<usize>>,
    /// Source file → bridges.
    from_file: HashMap<String, Vec<usize>>,
    /// Destination file → bridges.
    into_file: HashMap<String, Vec<usize>>,
    /// File → endpoints.
    endpoints_of: HashMap<String, Vec<usize>>,
}

impl BridgeTable {
    pub fn build(s: &Symbols) -> BridgeTable {
        let mut t = BridgeTable {
            endpoints: s.endpoints.clone(),
            bridges: s.bridges.clone(),
            ..BridgeTable::default()
        };
        for (i, b) in t.bridges.iter().enumerate() {
            if let Some(d) = &b.dst {
                t.into_sym.entry(d.clone()).or_default().push(i);
            }
            t.from_file.entry(b.src_file.clone()).or_default().push(i);
            t.into_file.entry(b.dst_file.clone()).or_default().push(i);
        }
        for (i, e) in t.endpoints.iter().enumerate() {
            t.endpoints_of.entry(e.file.clone()).or_default().push(i);
        }
        t
    }

    pub fn is_empty(&self) -> bool {
        self.endpoints.is_empty() && self.bridges.is_empty()
    }

    /// Bridges that reach symbol `id`.
    pub fn into_symbol(&self, id: &str) -> impl Iterator<Item = &Bridge> {
        self.into_sym
            .get(id)
            .into_iter()
            .flatten()
            .map(|&i| &self.bridges[i])
    }

    /// Bridges that leave file `path`.
    pub fn from_file(&self, path: &str) -> impl Iterator<Item = &Bridge> {
        self.from_file
            .get(path)
            .into_iter()
            .flatten()
            .map(|&i| &self.bridges[i])
    }

    /// Bridges that reach file `path`.
    pub fn into_file(&self, path: &str) -> impl Iterator<Item = &Bridge> {
        self.into_file
            .get(path)
            .into_iter()
            .flatten()
            .map(|&i| &self.bridges[i])
    }

    /// Endpoints found in file `path`.
    pub fn endpoints_of(&self, path: &str) -> impl Iterator<Item = &BridgeEndpoint> {
        self.endpoints_of
            .get(path)
            .into_iter()
            .flatten()
            .map(|&i| &self.endpoints[i])
    }

    /// The bridges from `src` to `dst` (files).
    pub fn between(&self, src: &str, dst: &str) -> impl Iterator<Item = &Bridge> {
        self.from_file(src).filter(move |b| b.dst_file == dst)
    }
}

/// `ipc`, `http` or `package`, as people read it: `IPC`, `HTTP`, `package`.
pub fn kind_label(k: BridgeKind) -> &'static str {
    match k {
        BridgeKind::Http => "HTTP",
        BridgeKind::Ipc => "IPC",
        BridgeKind::Package => "package",
    }
}

/// One step over a bridge, for paths and dependency lists: `ipc invoke users:get`.
pub fn step_label(b: &Bridge) -> String {
    format!("{} {} {}", b.kind, b.subtype, b.key)
}

use crate::budget::{Answer, clip, fit, limits};
use crate::graph::NodeRef;
use crate::{QueryError, Repo};
use cartograph_core::BridgeRole;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Options of [`bridges`].
#[derive(Debug, Clone, Default)]
pub struct BridgeQuery {
    /// Only bridges with an end in this file or folder.
    pub node: Option<String>,
    /// `http`, `ipc` or `package`.
    pub kind: Option<BridgeKind>,
    /// Only bridges with an end in this member.
    pub member: Option<String>,
    /// List what is left unmatched instead of the bridges.
    pub unmatched: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BridgeRow {
    pub kind: BridgeKind,
    pub subtype: String,
    pub key: String,
    /// `path:line`.
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_symbol: Option<String>,
    pub to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_symbol: Option<String>,
    pub confidence: cartograph_core::Confidence,
}

#[derive(Debug, Clone, Serialize)]
pub struct EndpointRow {
    pub kind: BridgeKind,
    pub role: BridgeRole,
    pub key: String,
    pub label: String,
    /// `path:line`.
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// `unmatched`, `unused`, `unexposed`, `dynamic`, `version_skew …`.
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BridgesResult {
    /// What was asked for (`node`, `kind`, `member`), as one line.
    pub scope: String,
    /// Bridges in scope, by `kind subtype`.
    pub counts: BTreeMap<String, usize>,
    /// Endpoints in scope left unmatched, by `kind detail`.
    pub unmatched_counts: BTreeMap<String, usize>,
    pub rows: Vec<BridgeRow>,
    pub rows_more: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unmatched: Vec<EndpointRow>,
    #[serde(skip_serializing_if = "is_zero")]
    pub unmatched_more: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// `path:line` (`path` for a whole file).
pub fn at(file: &str, line: u32) -> String {
    if line == 0 {
        file.to_string()
    } else {
        format!("{file}:{line}")
    }
}

/// `bridges(node?, kind?, member?, unmatched?)` within [`limits::BRIDGES`]: the bridges with
/// an end in scope (or, with `unmatched`, the endpoints left without one), most certain
/// first.
pub fn bridges(repo: &Repo, q: &BridgeQuery) -> Result<Answer<BridgesResult>, QueryError> {
    let g = &repo.graph;
    let t = &repo.symbols.bridges;
    let files: Option<BTreeSet<String>> = match &q.node {
        Some(n) => {
            let r = repo.resolve(n)?;
            if let NodeRef::Ext(_) = r {
                return Err(QueryError::BadArgument(format!(
                    "{} is a library: bridges link files of the repo",
                    g.label(r)
                )));
            }
            Some(
                g.files_of(r)
                    .into_iter()
                    .map(|f| g.path(f).to_string())
                    .collect(),
            )
        }
        None => None,
    };
    if let Some(m) = &q.member
        && !g.members.names.iter().any(|x| x == m)
    {
        return Err(QueryError::BadArgument(format!(
            "no member {m:?} (members: {})",
            if g.members.names.is_empty() {
                "none: this is a single repo".to_string()
            } else {
                g.members.names.join(", ")
            }
        )));
    }
    let in_scope = |path: &str| {
        files.as_ref().is_none_or(|s| s.contains(path))
            && q.member
                .as_deref()
                .is_none_or(|m| g.members.member_of(path) == Some(m))
    };
    let kind_ok = |k: BridgeKind| q.kind.is_none_or(|x| x == k);
    let mut rows: Vec<&Bridge> = t
        .bridges
        .iter()
        .filter(|b| kind_ok(b.kind) && (in_scope(&b.src_file) || in_scope(&b.dst_file)))
        .collect();
    rows.sort_by(|a, b| {
        (a.confidence, a.kind, &a.key, &a.src_file, a.src_line).cmp(&(
            b.confidence,
            b.kind,
            &b.key,
            &b.src_file,
            b.src_line,
        ))
    });
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for b in &rows {
        *counts
            .entry(format!("{} {}", b.kind, b.subtype))
            .or_default() += 1;
    }
    let mut left: Vec<&BridgeEndpoint> = t
        .endpoints
        .iter()
        .filter(|e| kind_ok(e.kind) && e.detail.is_some() && in_scope(&e.file))
        .collect();
    left.sort_by(|a, b| {
        (&a.detail, &a.key, &a.file, a.line).cmp(&(&b.detail, &b.key, &b.file, b.line))
    });
    let mut unmatched_counts: BTreeMap<String, usize> = BTreeMap::new();
    for e in &left {
        let d = e.detail.as_deref().unwrap_or("");
        let d = d.split_whitespace().next().unwrap_or(d);
        *unmatched_counts
            .entry(format!("{} {d}", e.kind))
            .or_default() += 1;
    }
    let scope = {
        let mut parts = Vec::new();
        if let Some(n) = &q.node {
            parts.push(format!("node {n}"));
        }
        if let Some(k) = q.kind {
            parts.push(format!("kind {k}"));
        }
        if let Some(m) = &q.member {
            parts.push(format!("member {m}"));
        }
        if parts.is_empty() {
            "whole repo".to_string()
        } else {
            parts.join(", ")
        }
    };
    Ok(fit(
        limits::BRIDGES,
        crate::budget::STEPS,
        |n| {
            let (nr, nu) = if q.unmatched { (0, n) } else { (n, 0) };
            BridgesResult {
                scope: scope.clone(),
                counts: counts.clone(),
                unmatched_counts: unmatched_counts.clone(),
                rows: rows
                    .iter()
                    .take(nr)
                    .map(|b| BridgeRow {
                        kind: b.kind,
                        subtype: b.subtype.clone(),
                        key: clip(&b.key, 160),
                        from: at(&b.src_file, b.src_line),
                        from_symbol: b.src.clone(),
                        to: at(&b.dst_file, b.dst_line),
                        to_symbol: b.dst.clone(),
                        confidence: b.confidence,
                    })
                    .collect(),
                rows_more: if q.unmatched {
                    0
                } else {
                    rows.len().saturating_sub(nr)
                },
                unmatched: left
                    .iter()
                    .take(nu)
                    .map(|e| EndpointRow {
                        kind: e.kind,
                        role: e.role,
                        key: clip(&e.key, 160),
                        label: clip(&e.label, 80),
                        at: at(&e.file, e.line),
                        symbol: e.symbol.clone(),
                        detail: e.detail.clone().unwrap_or_default(),
                    })
                    .collect(),
                unmatched_more: if q.unmatched {
                    left.len().saturating_sub(nu)
                } else {
                    0
                },
            }
        },
        render,
    ))
}

/// ` Name.of.symbol` of a symbol id, or nothing.
pub fn sym_name(s: &Option<String>) -> String {
    s.as_deref()
        .and_then(cartograph_core::split_symbol_id)
        .map_or(String::new(), |(_, n)| format!(" {}", clip(n, 80)))
}

fn render(r: &BridgesResult) -> String {
    let mut s = String::new();
    let total: usize = r.counts.values().sum();
    let counts: Vec<String> = r.counts.iter().map(|(k, v)| format!("{k} {v}")).collect();
    s.push_str(&format!(
        "bridges ({}): {total}{}\n",
        r.scope,
        if counts.is_empty() {
            String::new()
        } else {
            format!(" · {}", counts.join(", "))
        }
    ));
    if !r.unmatched_counts.is_empty() {
        let u: Vec<String> = r
            .unmatched_counts
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect();
        s.push_str(&format!("left unmatched: {}\n", u.join(", ")));
    }
    for b in &r.rows {
        s.push_str(&format!(
            "{} {} \"{}\": {}{} → {}{} [{}]\n",
            b.kind,
            b.subtype,
            b.key,
            clip(&b.from, 160),
            sym_name(&b.from_symbol),
            clip(&b.to, 160),
            sym_name(&b.to_symbol),
            b.confidence
        ));
    }
    if r.rows_more > 0 {
        s.push_str(&format!(
            "+{} more (narrow with node, kind or member)\n",
            r.rows_more
        ));
    }
    for e in &r.unmatched {
        s.push_str(&format!(
            "{} {} \"{}\" at {}{} ({}): {}\n",
            e.kind,
            e.role,
            e.key,
            clip(&e.at, 160),
            sym_name(&e.symbol),
            e.label,
            e.detail
        ));
    }
    if r.unmatched_more > 0 {
        s.push_str(&format!("+{} more unmatched\n", r.unmatched_more));
    }
    if r.rows.is_empty() && r.unmatched.is_empty() && total == 0 {
        s.push_str("no bridges here (none found, or [bridges] turns them off)\n");
    }
    s
}

/// One bridge seen from a set of files (see [`links`]).
#[derive(Debug, Clone, Serialize)]
pub struct Link {
    /// `out`: from one of the files; `in`: into one of them.
    pub dir: &'static str,
    pub kind: BridgeKind,
    pub subtype: String,
    pub key: String,
    /// The end in the files, `path:line`.
    pub here: String,
    /// The other end, `path:line`.
    pub there: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub there_symbol: Option<String>,
    pub confidence: cartograph_core::Confidence,
}

/// The bridges that leave (`out`) or reach (`inn`) the files `paths`, other end outside them;
/// most certain first.
pub fn links(t: &BridgeTable, paths: &BTreeSet<String>, out: bool, inn: bool) -> Vec<Link> {
    let mut v: Vec<Link> = Vec::new();
    for p in paths {
        if out {
            for b in t.from_file(p).filter(|b| !paths.contains(&b.dst_file)) {
                v.push(Link {
                    dir: "out",
                    kind: b.kind,
                    subtype: b.subtype.clone(),
                    key: b.key.clone(),
                    here: at(&b.src_file, b.src_line),
                    there: at(&b.dst_file, b.dst_line),
                    there_symbol: b.dst.clone(),
                    confidence: b.confidence,
                });
            }
        }
        if inn {
            for b in t.into_file(p).filter(|b| !paths.contains(&b.src_file)) {
                v.push(Link {
                    dir: "in",
                    kind: b.kind,
                    subtype: b.subtype.clone(),
                    key: b.key.clone(),
                    here: at(&b.dst_file, b.dst_line),
                    there: at(&b.src_file, b.src_line),
                    there_symbol: b.src.clone(),
                    confidence: b.confidence,
                });
            }
        }
    }
    v.sort_by(|a, b| {
        (a.dir, a.confidence, &a.key, &a.there).cmp(&(b.dir, b.confidence, &b.key, &b.there))
    });
    v
}

/// The lines of [`links`] for an answer: `out ipc invoke "users:get" → path:line Name`.
pub fn render_links(s: &mut String, links: &[Link], more: usize) {
    if links.is_empty() {
        return;
    }
    s.push_str("over bridges (not imports):\n");
    for l in links {
        let arrow = if l.dir == "out" { "→" } else { "←" };
        s.push_str(&format!(
            "  {} {} \"{}\" {arrow} {}{} (at {}) [{}]\n",
            kind_label(l.kind),
            l.subtype,
            clip(&l.key, 120),
            clip(&l.there, 160),
            sym_name(&l.there_symbol),
            clip(&l.here, 160),
            l.confidence
        ));
    }
    if more > 0 {
        s.push_str(&format!("  +{more} more (see `bridges`)\n"));
    }
}
