//! Findings about bridges (see [`crate::bridges`]), from the endpoints the indexer left
//! unmatched: an IPC channel no handler serves, a renderer call to a function the preload
//! does not expose, a handler nothing uses.

use super::{Ctx, Finding, FindingKind, Severity};
use crate::bridges::{BridgeTable, UNEXPOSED, UNMATCHED, UNUSED};
use crate::graph::NodeRef;
use cartograph_core::{BridgeEndpoint, BridgeKind, BridgeRole};
use std::collections::BTreeMap;

/// The member a path belongs to (`""` in a single repo).
fn member_of<'p>(cx: &Ctx<'_>, path: &'p str) -> &'p str {
    match path.split_once('/') {
        Some((m, _)) if cx.g.members.names.iter().any(|n| n == m) => m,
        _ => "",
    }
}

/// `path:line label` of an endpoint, with the symbol that holds it.
fn site(e: &BridgeEndpoint) -> String {
    let who = e
        .symbol
        .as_deref()
        .and_then(cartograph_core::split_symbol_id)
        .map_or(String::new(), |(_, n)| format!(" in {n}"));
    format!("{}:{} {}{who}", e.file, e.line, e.label)
}

/// One finding per (member, key) of the endpoints `pick` selects.
fn per_key(
    cx: &Ctx<'_>,
    bridges: &BridgeTable,
    kind: FindingKind,
    severity: Severity,
    pick: impl Fn(&BridgeEndpoint) -> bool,
    title: impl Fn(&str, usize) -> String,
) -> Vec<Finding> {
    let mut by_key: BTreeMap<(&str, &str), Vec<&BridgeEndpoint>> = BTreeMap::new();
    for e in bridges.endpoints.iter().filter(|e| pick(e)) {
        if !cx
            .g
            .file_by_path(&e.file)
            .is_some_and(|f| cx.active[f as usize])
        {
            continue;
        }
        by_key
            .entry((member_of(cx, &e.file), e.key.as_str()))
            .or_default()
            .push(e);
    }
    by_key
        .into_iter()
        .map(|((member, key), sites)| {
            let id = if member.is_empty() {
                key.to_string()
            } else {
                format!("{member}:{key}")
            };
            let mut f = Finding::new(kind, severity, &id, title(key, sites.len()));
            for e in &sites {
                f.line(site(e));
                if let Some(fx) = cx.g.file_by_path(&e.file) {
                    f.node(cx.g.id(NodeRef::File(fx)));
                }
            }
            f.metric("sites", sites.len() as f64);
            f
        })
        .collect()
}

fn ipc(e: &BridgeEndpoint, role: BridgeRole, detail: &str) -> bool {
    e.kind == BridgeKind::Ipc && e.role == role && e.detail.as_deref() == Some(detail)
}

fn sites(n: usize) -> String {
    if n == 1 {
        "1 site".into()
    } else {
        format!("{n} sites")
    }
}

/// IPC channels used that no handler serves.
pub(super) fn ipc_no_handler(cx: &Ctx<'_>, b: &BridgeTable) -> Vec<Finding> {
    per_key(
        cx,
        b,
        FindingKind::IpcNoHandler,
        Severity::Warning,
        |e| ipc(e, BridgeRole::Consume, UNMATCHED),
        |key, n| {
            format!(
                "IPC channel \"{key}\" is used but no main-process handler serves it ({})",
                sites(n)
            )
        },
    )
}

/// Renderer calls of a function the preload does not expose.
pub(super) fn ipc_unexposed(cx: &Ctx<'_>, b: &BridgeTable) -> Vec<Finding> {
    per_key(
        cx,
        b,
        FindingKind::IpcUnexposed,
        Severity::Warning,
        |e| ipc(e, BridgeRole::Consume, UNEXPOSED),
        |key, n| {
            format!(
                "{key} is called but the preload does not expose it ({})",
                sites(n)
            )
        },
    )
}

/// IPC handlers nothing uses.
pub(super) fn ipc_unused_handler(cx: &Ctx<'_>, b: &BridgeTable) -> Vec<Finding> {
    per_key(
        cx,
        b,
        FindingKind::IpcUnusedHandler,
        Severity::Info,
        |e| ipc(e, BridgeRole::Provide, UNUSED),
        |key, _| format!("IPC handler of \"{key}\" is never invoked or sent to"),
    )
}

/// Endpoint detail of a dependency on another version than its member builds (written by
/// `cartograph_index::packages`): `version_skew <the producer's version>`.
const VERSION_SKEW: &str = "version_skew";

/// Dependencies on another member's package at another version than it builds.
pub(super) fn version_skew(cx: &Ctx<'_>, b: &BridgeTable) -> Vec<Finding> {
    let mut out = Vec::new();
    for e in &b.endpoints {
        let Some(have) = e
            .detail
            .as_deref()
            .and_then(|d| d.strip_prefix(VERSION_SKEW))
            .map(str::trim)
        else {
            continue;
        };
        if e.kind != BridgeKind::Package || e.role != BridgeRole::Consume {
            continue;
        }
        let producers: Vec<&str> = b
            .from_file(&e.file)
            .filter(|x| x.kind == BridgeKind::Package && x.key == e.key)
            .map(|x| member_of(cx, &x.dst_file))
            .collect();
        let who = if producers.is_empty() {
            "its member".to_string()
        } else {
            producers.join(", ")
        };
        let mut f = Finding::new(
            FindingKind::VersionSkew,
            Severity::Info,
            &format!("{}:{}", e.file, e.key),
            format!(
                "{} depends on {} {} but {who} builds {have}",
                e.file, e.key, e.label
            ),
        );
        f.line(format!("{}:{} {} {}", e.file, e.line, e.key, e.label));
        if let Some(fx) = cx.g.file_by_path(&e.file) {
            f.node(cx.g.id(NodeRef::File(fx)));
        }
        out.push(f);
    }
    out
}

/// Members whose code another member imports without declaring its package (Java, where
/// the build would not find the class).
pub(super) fn undeclared_member_dependency(cx: &Ctx<'_>, b: &BridgeTable) -> Vec<Finding> {
    use std::collections::{BTreeSet, HashMap};
    let g = cx.g;
    if g.members.names.is_empty() {
        return Vec::new();
    }
    let package = |e: &&BridgeEndpoint| e.kind == BridgeKind::Package;
    let known: BTreeSet<&str> = b
        .endpoints
        .iter()
        .filter(package)
        .map(|e| member_of(cx, &e.file))
        .collect();
    let producers: BTreeSet<&str> = b
        .endpoints
        .iter()
        .filter(package)
        .filter(|e| e.role == BridgeRole::Provide)
        .map(|e| member_of(cx, &e.file))
        .collect();
    let mut direct: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for x in b.bridges.iter().filter(|x| x.kind == BridgeKind::Package) {
        direct
            .entry(member_of(cx, &x.src_file))
            .or_default()
            .insert(member_of(cx, &x.dst_file));
    }
    let reaches = |from: &str, to: &str| {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut stack: Vec<&str> = vec![from];
        while let Some(m) = stack.pop() {
            for &n in direct.get(m).into_iter().flatten() {
                if n == to {
                    return true;
                }
                if seen.insert(n) {
                    stack.push(n);
                }
            }
        }
        false
    };
    let mut pairs: BTreeMap<(&str, &str), Vec<(u32, u32)>> = BTreeMap::new();
    for (i, deps) in g.out.iter().enumerate() {
        let f = &g.files[i];
        if !cx.active[i] || f.lang.as_deref() != Some("java") {
            continue;
        }
        let a = member_of(cx, &f.path);
        if !known.contains(a) {
            continue;
        }
        for d in deps {
            if d.kinds & crate::graph::dep_kind::IMPORT == 0 {
                continue;
            }
            let bm = member_of(cx, g.path(d.file));
            if bm.is_empty() || bm == a || !producers.contains(bm) || reaches(a, bm) {
                continue;
            }
            pairs.entry((a, bm)).or_default().push((i as u32, d.file));
        }
    }
    pairs
        .into_iter()
        .map(|((a, bm), edges)| {
            let n = edges.len();
            let mut f = Finding::new(
                FindingKind::UndeclaredMemberDependency,
                Severity::Warning,
                &format!("{a}->{bm}"),
                format!(
                    "{a} imports code of {bm} without declaring its package ({n} {})",
                    if n == 1 { "import" } else { "imports" }
                ),
            );
            for (x, y) in edges {
                f.line(g.edge_evidence(x, y));
                f.edge(g, x, y);
                f.node(g.id(NodeRef::File(x)));
            }
            f.metric("imports", n as f64);
            f
        })
        .collect()
}
