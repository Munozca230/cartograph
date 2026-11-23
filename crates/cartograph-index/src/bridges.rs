//! Bridges (see [`cartograph_core::bridge`]): links between files that no import states.
//! IPC endpoints are extracted with the symbols and matched in [`crate::symbols::resolve`];
//! package links come from the manifests ([`crate::packages`]). This module turns the
//! matched bridges into derived graph edges.

use cartograph_core::{Bridge, BridgeKind, Confidence, Edge, EdgeKind, NodeKind, node_id};
use std::collections::BTreeMap;

/// The derived file → file edges of matched HTTP and IPC bridges (a package bridge explains
/// import edges, it adds none): one per pair of files, `weight` = bridged sites, the best
/// confidence. A bridge inside one file adds nothing.
pub fn edges(bridges: &[Bridge]) -> Vec<Edge> {
    let mut by_pair: BTreeMap<(&str, &str), (u64, Confidence)> = BTreeMap::new();
    for b in bridges {
        if b.kind == BridgeKind::Package || b.src_file == b.dst_file {
            continue;
        }
        let e = by_pair
            .entry((&b.src_file, &b.dst_file))
            .or_insert((0, Confidence::Heuristic));
        e.0 += 1;
        e.1 = e.1.min(b.confidence);
    }
    by_pair
        .into_iter()
        .map(|((s, d), (w, c))| Edge {
            weight: w,
            ..Edge::new(
                node_id(NodeKind::File, s),
                node_id(NodeKind::File, d),
                EdgeKind::Bridge,
                c,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(kind: BridgeKind, src: &str, dst: &str, c: Confidence) -> Bridge {
        Bridge {
            kind,
            subtype: "invoke".into(),
            src_file: src.into(),
            src_line: 1,
            src: None,
            dst_file: dst.into(),
            dst_line: 2,
            dst: None,
            key: "k".into(),
            confidence: c,
        }
    }

    #[test]
    fn one_edge_per_pair_with_the_best_confidence() {
        let e = edges(&[
            b(BridgeKind::Ipc, "a.ts", "b.ts", Confidence::Heuristic),
            b(BridgeKind::Ipc, "a.ts", "b.ts", Confidence::Resolved),
            b(BridgeKind::Ipc, "a.ts", "a.ts", Confidence::Exact),
            b(
                BridgeKind::Package,
                "x/pom.xml",
                "y/pom.xml",
                Confidence::Exact,
            ),
        ]);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].src, "file:a.ts");
        assert_eq!(e[0].dst, "file:b.ts");
        assert_eq!(e[0].weight, 2);
        assert_eq!(e[0].confidence, Confidence::Resolved);
        assert_eq!(e[0].kind, EdgeKind::Bridge);
    }
}
