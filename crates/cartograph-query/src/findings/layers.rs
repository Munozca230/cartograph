//! Rules of `cartograph.toml`: layer dependencies and `max_fan_out`.

use super::{Ctx, Finding, FindingKind, Severity};
use crate::graph::{FileIx, NodeRef};
use std::collections::BTreeMap;

/// One finding per forbidden layer pair (`ui → infra`), with every crossing edge.
pub(super) fn layer_violations(cx: &Ctx<'_>) -> Vec<Finding> {
    let g = cx.g;
    let cfg = cx.cfg;
    if cfg.layers.is_empty() {
        return Vec::new();
    }
    let layer: Vec<Option<usize>> = g
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            if cx.active[i] {
                cfg.layer_of(&f.path)
            } else {
                None
            }
        })
        .collect();
    let mut pairs: BTreeMap<(usize, usize), Vec<(FileIx, FileIx)>> = BTreeMap::new();
    for (a, deps) in cx.adj.iter().enumerate() {
        let Some(la) = layer[a] else { continue };
        for &b in deps {
            let Some(lb) = layer[b as usize] else {
                continue;
            };
            if !cfg.allowed(la, lb) {
                pairs.entry((la, lb)).or_default().push((a as FileIx, b));
            }
        }
    }
    pairs
        .into_iter()
        .map(|((la, lb), edges)| {
            let (from, to) = (&cfg.layers[la].name, &cfg.layers[lb].name);
            let why = if cfg.layers[la].forbid.contains(&lb) {
                "forbidden"
            } else {
                "not in its allow list"
            };
            let n = edges.len();
            let title = format!(
                "layer {from} depends on {to} ({why}): {n} {}",
                if n == 1 { "import" } else { "imports" }
            );
            let mut f = Finding::new(
                FindingKind::LayerViolation,
                Severity::Error,
                &format!("{from}->{to}"),
                title,
            );
            f.violation = true;
            for &(a, b) in &edges {
                f.line(g.edge_evidence(a, b));
                f.edge(g, a, b);
                f.node(g.id(NodeRef::File(a)));
            }
            f.metric("imports", n as f64);
            f
        })
        .collect()
}

/// Files that depend on more files than `rules.max_fan_out`.
pub(super) fn fan_out_limits(cx: &Ctx<'_>) -> Vec<Finding> {
    let g = cx.g;
    let Some(max) = cx.cfg.max_fan_out else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (a, deps) in cx.adj.iter().enumerate() {
        if deps.len() <= max {
            continue;
        }
        let a = a as FileIx;
        let id = g.id(NodeRef::File(a));
        let mut f = Finding::new(
            FindingKind::FanOutLimit,
            Severity::Error,
            &id,
            format!(
                "{} depends on {} files (max_fan_out = {max})",
                g.path(a),
                deps.len()
            ),
        );
        f.violation = true;
        f.node(id);
        // Evidence: its dependencies by folder, biggest first.
        let mut by_dir: BTreeMap<String, usize> = BTreeMap::new();
        for &b in deps {
            *by_dir
                .entry(g.label(NodeRef::Dir(g.files[b as usize].dir)))
                .or_default() += 1;
        }
        let mut rows: Vec<_> = by_dir.into_iter().collect();
        rows.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
        for (d, n) in rows {
            f.line(format!("{d} {n}"));
        }
        f.metric("fan_out", deps.len() as f64);
        f.metric("max", max as f64);
        out.push(f);
    }
    out
}
