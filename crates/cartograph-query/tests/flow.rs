//! Flows for the web view: entries, the bounded call flow from a function, a
//! function's card and a file's flows, on the `flow-rust` fixture.

mod common;

use cartograph_core::{Confidence, EntryKind};
use cartograph_query::flow::{
    self, CallIndex, FlowGraph, FlowOptions, PER_NODE, entries, file_flows, flow, search_functions,
    symbol_card,
};
use cartograph_query::{QueryError, Repo, Severity};

fn repo() -> Repo {
    common::fixture("flow-rust")
}

fn names(g: &FlowGraph) -> Vec<&str> {
    g.nodes.iter().map(|n| n.name.as_str()).collect()
}

fn node<'g>(g: &'g FlowGraph, name: &str) -> &'g flow::FlowNode {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("{name} not in {:?}", names(g)))
}

#[test]
fn entries_leave_out_examples_and_start_commands_at_their_handler() {
    let r = repo();
    let ix = CallIndex::build(&r);
    let list = entries(&r, &ix);
    assert!(list.indexed);
    assert_eq!(list.hidden, 1, "the main of examples/ is not a repo entry");
    let rows: Vec<(EntryKind, &str, &str)> = list
        .entries
        .iter()
        .map(|e| (e.kind, e.name.as_str(), e.root.as_str()))
        .collect();
    assert_eq!(
        rows,
        vec![
            (EntryKind::Cli, "index", "sym:src/main.rs#cmd_index"),
            (EntryKind::Cli, "serve", "sym:src/main.rs#cmd_serve"),
            (EntryKind::Http, "GET /health", "sym:src/routes.rs#health"),
            (EntryKind::Main, "main", "sym:src/main.rs#main"),
        ]
    );
}

#[test]
fn a_flow_is_bounded_by_depth_per_function_and_node_budget() {
    let r = repo();
    let ix = CallIndex::build(&r);
    let g = flow(&r, &ix, "cmd_index", &FlowOptions::default()).unwrap();
    assert_eq!(g.root, "sym:src/main.rs#cmd_index");
    assert_eq!(g.nodes[0].depth, 0);
    // Source order: run_all, save, countdown.
    assert_eq!(
        &names(&g)[..4],
        &["cmd_index", "run_all", "save", "countdown"]
    );
    // run_all calls 11 functions: PER_NODE are shown, the rest counted.
    let run_all = node(&g, "run_all");
    assert_eq!(run_all.callees, 11);
    assert_eq!(run_all.more, 11 - PER_NODE);
    // Depth 4 stops under chain3 (cmd_index → run_all → chain1 → chain2 → chain3).
    let chain3 = node(&g, "chain3");
    assert_eq!(chain3.depth, 4);
    assert_eq!(chain3.more, 1);
    assert!(!names(&g).contains(&"chain4"));
    assert!(g.truncated);
    // a1 is called from run_all and from save: one node, two edges.
    let a1 = "sym:src/steps.rs#a1";
    assert_eq!(g.nodes.iter().filter(|n| n.id == a1).count(), 1);
    assert_eq!(g.edges.iter().filter(|e| e.dst == a1).count(), 2);
    // Recursion is not an edge to itself.
    assert!(!g.edges.iter().any(|e| e.src == e.dst));
    // Every edge joins two nodes of the flow, with its confidence and line.
    for e in &g.edges {
        assert!(g.nodes.iter().any(|n| n.id == e.src) && g.nodes.iter().any(|n| n.id == e.dst));
        assert!(e.line > 0);
    }
    let first = g
        .edges
        .iter()
        .find(|e| e.dst == "sym:src/steps.rs#run_all")
        .unwrap();
    assert_eq!((first.line, first.confidence), (27, Confidence::Resolved));
    // Files in the steps ↔ store cycle carry the finding's mark.
    assert_eq!(node(&g, "save").finding, Some(Severity::Warning));
    assert_eq!(node(&g, "cmd_index").finding, None);
}

#[test]
fn expanding_a_function_shows_what_was_cut() {
    let r = repo();
    let ix = CallIndex::build(&r);
    let opts = FlowOptions {
        expand: vec![
            "sym:src/steps.rs#run_all".into(),
            "sym:src/steps.rs#chain3".into(),
        ],
        ..FlowOptions::default()
    };
    let g = flow(&r, &ix, "sym:src/main.rs#cmd_index", &opts).unwrap();
    assert_eq!(node(&g, "run_all").more, 0);
    assert!(names(&g).contains(&"a10"));
    // Past the depth: chain4 joins, as a leaf with its own cut counted.
    assert_eq!(node(&g, "chain4").depth, 5);
    assert_eq!(node(&g, "chain4").more, 1);
}

fn ids(g: &FlowGraph) -> Vec<&str> {
    g.nodes.iter().map(|n| n.id.as_str()).collect()
}

fn edge_ids(g: &FlowGraph) -> Vec<(&str, &str)> {
    g.edges
        .iter()
        .map(|e| (e.src.as_str(), e.dst.as_str()))
        .collect()
}

#[test]
fn opening_a_card_never_removes_cards_already_shown() {
    let r = repo();
    let ix = CallIndex::build(&r);
    let with = |max_nodes: usize, expand: &[&str]| {
        let opts = FlowOptions {
            max_nodes,
            expand: expand.iter().map(|s| s.to_string()).collect(),
            ..FlowOptions::default()
        };
        flow(&r, &ix, "cmd_index", &opts).unwrap()
    };
    for budget in 10..=25 {
        let base = with(budget, &[]);
        for cut in base.nodes.iter().filter(|n| n.more > 0) {
            let open = with(budget, &[&cut.id]);
            for id in ids(&base) {
                assert!(
                    ids(&open).contains(&id),
                    "budget {budget}: opening {} removed {id}",
                    cut.id
                );
            }
            for e in edge_ids(&base) {
                assert!(
                    edge_ids(&open).contains(&e),
                    "budget {budget}: opening {} removed the edge {e:?}",
                    cut.id
                );
            }
            assert_eq!(node(&open, &cut.name).more, 0, "{} is open", cut.id);
            // A second card opened after it keeps both.
            for next in open.nodes.iter().filter(|n| n.more > 0 && n.id != cut.id) {
                let both = with(budget, &[&cut.id, &next.id]);
                for id in ids(&open) {
                    assert!(
                        ids(&both).contains(&id),
                        "budget {budget}: opening {} after {} removed {id}",
                        next.id,
                        cut.id
                    );
                }
            }
        }
    }
}

#[test]
fn the_node_budget_and_depth_are_clamped() {
    let r = repo();
    let ix = CallIndex::build(&r);
    let shallow = FlowOptions {
        depth: 1,
        ..FlowOptions::default()
    };
    let g = flow(&r, &ix, "cmd_index", &shallow).unwrap();
    assert_eq!(names(&g), vec!["cmd_index", "run_all", "save", "countdown"]);
    assert_eq!(node(&g, "run_all").more, 11);
    let tiny = FlowOptions {
        max_nodes: 0,
        depth: 99,
        ..FlowOptions::default()
    };
    let g = flow(&r, &ix, "cmd_index", &tiny).unwrap();
    assert_eq!(g.max_nodes, 10);
    assert_eq!(g.depth, flow::MAX_DEPTH);
    assert_eq!(g.nodes.len(), 10);
    // The budget goes to run_all (11 callees) before save (2): save's flush is left out.
    assert_eq!(node(&g, "run_all").more, 11 - 6);
    assert_eq!(node(&g, "save").more, 1);
}

#[test]
fn lanes_follow_top_folders_under_a_source_root() {
    let r = repo();
    let ix = CallIndex::build(&r);
    assert_eq!(ix.lane_of("src/main.rs"), "src");
    assert_eq!(ix.lane_of("src/util/probe.rs"), "src/util");
    assert_eq!(ix.lane_of("Cargo.toml"), "");
    let g = flow(&r, &ix, "health", &FlowOptions::default()).unwrap();
    assert_eq!(g.lanes[0].id, "src");
}

#[test]
fn a_symbol_card_puts_production_callers_first_and_counts_tests() {
    let r = repo();
    let ix = CallIndex::build(&r);
    let c = symbol_card(&r, &ix, "a1").unwrap();
    assert_eq!(c.symbol.id, "sym:src/steps.rs#a1");
    let prod: Vec<(&str, Option<&str>)> = c
        .callers
        .production
        .iter()
        .map(|s| (s.file.as_str(), s.caller_name.as_deref()))
        .collect();
    assert_eq!(
        prod,
        vec![
            ("src/steps.rs", Some("run_all")),
            ("src/store.rs", Some("save"))
        ]
    );
    assert_eq!((c.callers.test_sites, c.callers.test_files), (1, 1));
    assert_eq!(c.callees_total, 1);
    assert_eq!(c.callees[0].name, "flush");
    assert_eq!(c.finding, Some(Severity::Warning));
    assert!(c.git.is_none(), "no git layer in a temp copy");
    let entry = symbol_card(&r, &ix, "cmd_index").unwrap();
    assert_eq!(entry.entries.len(), 1);
    assert_eq!(entry.entries[0].name, "index");
}

#[test]
fn a_file_rooted_flow_has_a_card_too() {
    let r = repo();
    let ix = CallIndex::build(&r);
    let c = symbol_card(&r, &ix, "file:src/store.rs").unwrap();
    assert_eq!(c.symbol.id, "file:src/store.rs");
    assert_eq!(
        (c.symbol.name.as_str(), c.symbol.kind.as_str()),
        ("store.rs", "file")
    );
    assert_eq!(c.symbol.file, "src/store.rs");
    assert_eq!(c.finding, Some(Severity::Warning));
    assert!(matches!(
        symbol_card(&r, &ix, "file:nope.rs"),
        Err(QueryError::UnknownNode { .. })
    ));
    // A Godot scene starts its own flow: its card says which entry runs it.
    let g = common::fixture("godot-basic");
    let gx = CallIndex::build(&g);
    let list = entries(&g, &gx);
    let scene = list
        .entries
        .iter()
        .find(|e| e.kind == EntryKind::GodotMainScene)
        .expect("a main scene");
    assert!(scene.root.starts_with("file:"), "{}", scene.root);
    let card = symbol_card(&g, &gx, &scene.root).unwrap();
    assert_eq!(card.symbol.kind, "file");
    assert!(
        card.entries
            .iter()
            .any(|e| e.kind == EntryKind::GodotMainScene)
    );
}

#[test]
fn file_flows_list_its_functions_and_the_entries_that_reach_it() {
    let r = repo();
    let ix = CallIndex::build(&r);
    let f = file_flows(&r, &ix, "file:src/store.rs").unwrap();
    let fns: Vec<&str> = f.functions.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(fns, vec!["flush", "save"], "most called first");
    let reach: Vec<&str> = f.entries.iter().map(|e| e.name.as_str()).collect();
    // `index` runs cmd_index → save; `main` runs cmd_index too. `serve` never reaches it.
    assert!(
        reach.contains(&"index") && reach.contains(&"main"),
        "{reach:?}"
    );
    assert!(!reach.contains(&"serve"));
    assert!(!f.partial);
    assert!(matches!(
        file_flows(&r, &ix, "file:nope.rs"),
        Err(QueryError::UnknownNode { .. })
    ));
}

#[test]
fn function_search_finds_methods_and_functions_by_name() {
    let r = repo();
    let hits = search_functions(&r, "chain", 10);
    let got: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(got.len(), 5);
    assert_eq!(got[0], "chain1", "{got:?}");
    let exact = search_functions(&r, "steps.rs#a1", 5);
    assert_eq!(exact[0].id, "sym:src/steps.rs#a1");
    assert!(search_functions(&r, "", 5).is_empty());
}

#[test]
fn an_index_without_symbols_says_so() {
    let r = Repo::new(&cartograph_core::Graph::default(), Default::default());
    let ix = CallIndex::build(&r);
    assert!(!entries(&r, &ix).indexed);
    assert!(matches!(
        flow(&r, &ix, "main", &FlowOptions::default()),
        Err(QueryError::BadArgument(_))
    ));
}
