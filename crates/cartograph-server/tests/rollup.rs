//! The rollup invariant: the sum of edge weights in `/api/graph` equals the number of
//! file → file import edges whose ends fall in different frontier nodes. The expected
//! value is computed by brute force from the raw store edges and the frontier paths the
//! API returned, independently of the server's own lifting code.

mod common;

use cartograph_core::EdgeKind;
use cartograph_server::api;
use common::*;
use serde_json::Value;

/// Frontier node of `file` (repo-relative path) among `nodes` (`dir:` / `file:` ids).
fn owner<'a>(file: &str, nodes: &'a [String]) -> &'a str {
    let hits: Vec<&String> = nodes
        .iter()
        .filter(|id| {
            if let Some(f) = id.strip_prefix("file:") {
                f == file
            } else if let Some(d) = id.strip_prefix("dir:") {
                file.starts_with(&format!("{d}/"))
            } else {
                false
            }
        })
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "{file} must be in exactly one frontier node: {hits:?}"
    );
    hits[0]
}

fn check(app_graph: &Value, file_edges: &[(String, String)]) -> u64 {
    let nodes: Vec<String> = app_graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap().to_string())
        .collect();
    let expected = file_edges
        .iter()
        .filter(|(a, b)| owner(a, &nodes) != owner(b, &nodes))
        .count() as u64;
    let edges = app_graph["edges"].as_array().unwrap();
    let sum: u64 = edges.iter().map(|e| e["weight"].as_u64().unwrap()).sum();
    assert_eq!(sum, expected, "sum of weights vs crossing file edges");
    // Each edge's confidence split adds up to its weight, and no edge is a self-loop.
    for e in edges {
        let c = &e["confidence"];
        let parts = ["exact", "resolved", "heuristic"]
            .iter()
            .map(|k| c[k].as_u64().unwrap())
            .sum::<u64>();
        assert_eq!(parts, e["weight"].as_u64().unwrap());
        assert_ne!(e["src"], e["dst"]);
        assert!(nodes.iter().any(|n| *n == e["src"]) && nodes.iter().any(|n| *n == e["dst"]));
    }
    // Weighted degrees add up to the same total.
    let fan_in: u64 = app_graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["fan_in"].as_u64().unwrap())
        .sum();
    let fan_out: u64 = app_graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["fan_out"].as_u64().unwrap())
        .sum();
    assert_eq!((fan_in, fan_out), (sum, sum));
    sum
}

#[tokio::test]
async fn edge_weights_sum_to_the_crossing_file_edges_for_every_expansion() {
    let snap = snapshot_of("imports-ts", 1);
    let file_edges: Vec<(String, String)> = snap
        .graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import)
        .filter_map(|e| {
            Some((
                e.src.strip_prefix("file:")?.to_string(),
                e.dst.strip_prefix("file:")?.to_string(),
            ))
        })
        .collect();
    assert!(file_edges.len() >= 15, "fixture has a real file graph");
    let all_dirs: Vec<String> = snap
        .dirs
        .iter()
        .skip(1)
        .map(|d| format!("dir:{}", d.path))
        .collect();
    let app = api::router(ready_state("imports-ts"));

    let sets: Vec<Vec<&str>> = vec![
        vec![],
        vec!["dir:apps"],
        vec!["dir:apps", "dir:apps/web"],
        vec!["dir:apps/web/src", "dir:apps/web", "dir:apps"], // any order
        vec![
            "dir:apps",
            "dir:apps/web",
            "dir:apps/web/src",
            "dir:packages",
        ],
        vec![
            "dir:packages",
            "dir:packages/ui",
            "dir:packages/ui/src",
            "dir:packages/shared",
        ],
        vec!["dir:apps/web"], // parent not expanded: ignored
        vec!["dir:apps", "dir:apps", "repo:", "dir:"], // duplicates and the root
    ];
    let mut sums = Vec::new();
    for set in &sets {
        let q = set.join(",");
        let (status, g) = get(&app, &format!("/api/graph?expanded={q}")).await;
        assert_eq!(status, 200, "{q}");
        sums.push(check(&g, &file_edges));
    }
    // Everything expanded: every non-self-loop file edge is visible.
    let q = all_dirs.join(",");
    let (_, g) = get(&app, &format!("/api/graph?expanded={q}")).await;
    assert_eq!(g["ignored"], serde_json::json!([]));
    let total = check(&g, &file_edges);
    let non_loops = file_edges.iter().filter(|(a, b)| a != b).count() as u64;
    assert_eq!(total, non_loops);
    // Expanding never hides edges: deeper views show at least as much weight.
    assert!(sums[0] <= sums[1] && sums[1] <= sums[2] && sums[2] <= total);
    // The ignored child case equals the collapsed view.
    assert_eq!(sums[6], sums[0]);
}

#[tokio::test]
async fn python_fixture_holds_the_invariant_including_namespace_packages() {
    let snap = snapshot_of("imports-py", 1);
    let app = api::router(ready_state("imports-py"));
    let file_edges: Vec<(String, String)> = snap
        .graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import)
        .filter_map(|e| {
            Some((
                e.src.strip_prefix("file:")?.to_string(),
                e.dst.strip_prefix("file:")?.to_string(),
            ))
        })
        .collect();
    let ns = snap.ns_edges.len() as u64;
    let all_dirs: Vec<String> = snap
        .dirs
        .iter()
        .skip(1)
        .map(|d| format!("dir:{}", d.path))
        .collect();
    let (_, g) = get(&app, &format!("/api/graph?expanded={}", all_dirs.join(","))).await;
    let nodes: Vec<String> = g["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap().to_string())
        .collect();
    let expected = file_edges
        .iter()
        .filter(|(a, b)| owner(a, &nodes) != owner(b, &nodes))
        .count() as u64;
    let sum: u64 = g["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["weight"].as_u64().unwrap())
        .sum();
    // With every dir open, namespace-package targets are expanded: none is placed.
    assert!(ns > 0, "the fixture imports a namespace package");
    assert_eq!(g["hidden_namespace_imports"].as_u64(), Some(ns));
    assert_eq!(sum, expected);

    // With src/fxpy open and its `ns` package collapsed, core.py → ns is a placed edge
    // that crosses, so it adds to the file-edge count.
    let (_, g) = get(&app, "/api/graph?expanded=dir:src,dir:src/fxpy").await;
    let nodes: Vec<String> = g["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap().to_string())
        .collect();
    let crossing = file_edges
        .iter()
        .filter(|(a, b)| owner(a, &nodes) != owner(b, &nodes))
        .count() as u64;
    let sum: u64 = g["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["weight"].as_u64().unwrap())
        .sum();
    assert_eq!(g["hidden_namespace_imports"], 0);
    assert_eq!(sum, crossing + ns);
    assert!(
        g["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["src"] == "file:src/fxpy/core.py" && e["dst"] == "dir:src/fxpy/ns")
    );
}

fn node_ids(g: &Value) -> Vec<String> {
    let mut ids: Vec<String> = g["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn depth_and_collapsed_open_the_same_frontier_as_the_explicit_list() {
    let snap = snapshot_of("imports-ts", 1);
    let app = api::router(ready_state("imports-ts"));
    let file_edges: Vec<(String, String)> = snap
        .graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import)
        .filter_map(|e| {
            Some((
                e.src.strip_prefix("file:")?.to_string(),
                e.dst.strip_prefix("file:")?.to_string(),
            ))
        })
        .collect();
    // Dirs above a level (top-level dirs are level 1).
    let dirs_above = |level: usize| -> Vec<String> {
        snap.dirs
            .iter()
            .skip(1)
            .filter(|d| d.path.split('/').count() < level)
            .map(|d| format!("dir:{}", d.path))
            .collect()
    };
    let explicit = |ids: &[String]| format!("/api/graph?expanded={}", ids.join(","));

    // depth=1 is the default view; 2 and 3 equal the explicit lists of dirs above them.
    let (_, plain) = get(&app, "/api/graph").await;
    let (status, d1) = get(&app, "/api/graph?depth=1").await;
    assert_eq!(status, 200);
    assert_eq!(node_ids(&d1), node_ids(&plain));
    for level in [2usize, 3] {
        let (status, by_depth) = get(&app, &format!("/api/graph?depth={level}")).await;
        assert_eq!(status, 200);
        let (_, by_list) = get(&app, &explicit(&dirs_above(level))).await;
        assert_eq!(node_ids(&by_depth), node_ids(&by_list), "depth={level}");
        assert_eq!(check(&by_depth, &file_edges), check(&by_list, &file_edges));
        assert_eq!(by_depth["ignored"], serde_json::json!([]));
    }

    // depth=all opens everything: every non-self-loop file edge is visible.
    let (_, all) = get(&app, "/api/graph?depth=all").await;
    let non_loops = file_edges.iter().filter(|(a, b)| a != b).count() as u64;
    assert_eq!(check(&all, &file_edges), non_loops);
    assert!(node_ids(&all).iter().all(|id| id.starts_with("file:")));

    // collapsed closes a dir (and so everything under it) even inside the level.
    let (_, g) = get(&app, "/api/graph?depth=all&collapsed=dir:apps").await;
    let ids = node_ids(&g);
    assert!(ids.contains(&"dir:apps".to_string()));
    assert!(!ids.iter().any(|id| id.contains("apps/")));
    check(&g, &file_edges);

    // expanded adds dirs below the level; a collapsed one wins and is reported.
    let (_, g) = get(
        &app,
        "/api/graph?depth=2&expanded=dir:apps/web&collapsed=dir:packages",
    )
    .await;
    let ids = node_ids(&g);
    assert!(ids.contains(&"dir:apps/web/src".to_string()));
    assert!(ids.contains(&"dir:packages".to_string()));
    assert!(!ids.iter().any(|id| id.starts_with("dir:packages/")));
    check(&g, &file_edges);
    let (_, g) = get(
        &app,
        "/api/graph?depth=2&expanded=dir:apps&collapsed=dir:apps&collapsed=dir:nope&collapsed=repo:",
    )
    .await;
    assert_eq!(
        g["ignored"],
        serde_json::json!(["dir:nope", "repo:", "dir:apps"])
    );
    assert!(node_ids(&g).contains(&"dir:apps".to_string()));

    // An id given twice is reported once (the mock API does the same).
    let (_, g) = get(
        &app,
        "/api/graph?expanded=dir:nope,dir:nope&collapsed=dir:nope&collapsed=repo:&collapsed=repo:",
    )
    .await;
    assert_eq!(g["ignored"], serde_json::json!(["dir:nope", "repo:"]));

    // Only plain digits are a level, as in the mock API; a huge one opens everything.
    let (status, huge) = get(&app, "/api/graph?depth=99999999999999999999999").await;
    assert_eq!(status, 200);
    assert_eq!(node_ids(&huge), node_ids(&all));

    // Bad depths are a 400 with the error envelope.
    for bad in [
        "0", "00", "-1", "+5", "%2B5", "%205", "5%20", "1.0", "abc", "",
    ] {
        let (status, body) = get(&app, &format!("/api/graph?depth={bad}")).await;
        assert_eq!(status, 400, "depth={bad}");
        assert_eq!(body["error"]["code"], "bad_request");
    }
}
