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

