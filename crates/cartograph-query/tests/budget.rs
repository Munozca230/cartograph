//! Every answer stays within its token budget (chars / 4), text and JSON, on a big
//! synthetic graph with long Java-like paths: thousands of files, dense imports, a file
//! that everything uses, many libraries and many findings.

use cartograph_core::{
    CallStats, Confidence, Edge, EdgeKind, EntryKind, EntryPoint, Graph, ImportSite, Node,
    NodeKind, SuperType, Symbol, SymbolCall, SymbolKind, Symbols, node_id, symbol_id,
};
use cartograph_query::budget::{json_tokens, limits, tokens};
use cartograph_query::deps::{Direction, Level};
use cartograph_query::list::Filter;
use cartograph_query::{
    Answer, Config, QueryError, Repo, check, deps, explain, find, impact, list, overview, path,
    symbols,
};
use serde::Serialize;
use serde_json::Value;

fn node(kind: NodeKind, path: &str, lang: Option<&str>, loc: u64) -> Node {
    Node {
        id: node_id(kind, path),
        kind,
        path: path.to_string(),
        name: path.rsplit('/').next().unwrap_or(path).to_string(),
        lang: lang.map(str::to_string),
        loc,
        bytes: loc * 30,
        hash: None,
        meta: Value::Null,
    }
}

/// 60 packages × 50 files under a long Java-like prefix; every file imports 6 others
/// (some across packages, making cycles), the first file is imported by everything.
fn big_graph() -> Graph {
    let mut g = Graph::default();
    let prefix = "src/main/java/com/example/enterprise/platform/very/long/package/name";
    g.nodes.push(node(NodeKind::Repo, "", None, 0));
    let mut files = Vec::new();
    for p in 0..60 {
        let dir = format!("{prefix}/module{p:02}/implementation/details");
        for f in 0..50 {
            let path = format!("{dir}/SomethingQuiteDescriptiveService{f:02}Impl.java");
            g.nodes.push(node(
                NodeKind::File,
                &path,
                Some("java"),
                50 + (f * 37 % 400) as u64,
            ));
            files.push(path);
        }
    }
    let mut test = Vec::new();
    for t in 0..200 {
        let path = format!(
            "src/test/java/com/example/enterprise/platform/SomethingQuiteDescriptiveServiceTest{t:03}.java"
        );
        g.nodes.push(node(NodeKind::File, &path, Some("java"), 80));
        test.push(path);
    }
    for x in 0..120 {
        g.nodes.push(node(
            NodeKind::External,
            &format!("maven:org.library{x:03}"),
            None,
            0,
        ));
    }
    let mut seed: u64 = 42;
    let mut rnd = |n: usize| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % n
    };
    let add = |src: &str, dst: &str, line: u32, g: &mut Graph| {
        g.edges.push(Edge::new(
            node_id(NodeKind::File, src),
            node_id(NodeKind::File, dst),
            EdgeKind::Import,
            Confidence::Resolved,
        ));
        g.imports.push(ImportSite {
            file: src.to_string(),
            line,
            specifier: dst.replace('/', "."),
            target: node_id(NodeKind::File, dst),
        });
    };
    for (i, src) in files.iter().enumerate() {
        if i > 0 {
            add(src, &files[0], 1, &mut g);
        }
        for k in 0..6 {
            let j = rnd(files.len());
            if j != i {
                add(src, &files[j], 2 + k, &mut g);
            }
        }
        for k in 0..3 {
            let x = rnd(120);
            g.edges.push(Edge::new(
                node_id(NodeKind::File, src),
                format!("external:maven:org.library{x:03}"),
                EdgeKind::ImportExternal,
                Confidence::Resolved,
            ));
            let _ = k;
        }
    }
    for (i, t) in test.iter().enumerate() {
        add(t, &files[i * 7 % files.len()], 1, &mut g);
        add(t, &files[0], 2, &mut g);
    }
    g.normalize();
    g
}

fn assert_fits<T: Serialize>(what: &str, a: &Answer<T>, budget: usize) {
    let t = tokens(&a.text);
    let j = json_tokens(&a.data);
    assert!(
        t <= budget,
        "{what}: text {t} > {budget} tokens:\n{}",
        a.text
    );
    assert!(j <= budget, "{what}: JSON {j} > {budget} tokens");
    assert!(!a.text.is_empty(), "{what}: empty");
}

#[test]
fn every_answer_fits_its_budget_on_a_big_repo() {
    let g = big_graph();
    let r = Repo::new(&g, Config::default());
    let hub = "src/main/java/com/example/enterprise/platform/very/long/package/name/module00/implementation/details/SomethingQuiteDescriptiveService00Impl.java";
    let other = "src/main/java/com/example/enterprise/platform/very/long/package/name/module31/implementation/details/SomethingQuiteDescriptiveService17Impl.java";
    let module = "src/main/java/com/example/enterprise/platform/very/long/package/name/module05";
    for depth in 1..=4 {
        assert_fits(
            &format!("overview {depth}"),
            &overview::overview(&r, None, depth).unwrap(),
            limits::OVERVIEW,
        );
    }
    assert_fits(
        "overview module",
        &overview::overview(&r, Some(module), 4).unwrap(),
        limits::OVERVIEW,
    );
    for n in [hub, other, module, "maven:org.library001", "."] {
        assert_fits(
            &format!("explain {n}"),
            &explain::explain(&r, n).unwrap(),
            limits::EXPLAIN,
        );
    }
    for n in [hub, other, module] {
        for dir in [Direction::Out, Direction::In, Direction::Both] {
            for level in [Level::File, Level::Folder] {
                for depth in 1..=3 {
                    assert_fits(
                        &format!("deps {n} {dir:?} {level:?} {depth}"),
                        &deps::deps(&r, n, dir, depth, level).unwrap(),
                        limits::DEPS,
                    );
                }
            }
        }
    }
    assert_fits(
        "impact hub",
        &impact::impact(&r, &[hub.to_string()]).unwrap(),
        limits::IMPACT,
    );
    assert_fits(
        "impact many",
        &impact::impact(
            &r,
            &[
                other.to_string(),
                module.to_string(),
                "maven:org.library002".to_string(),
            ],
        )
        .unwrap(),
        limits::IMPACT,
    );
    for q in ["s", "service impl", "module05", "library"] {
        assert_fits(
            &format!("find {q}"),
            &find::find(&r, q, None).unwrap(),
            limits::FIND,
        );
    }
    assert_fits(
        "path files",
        &path::path(&r, other, hub).unwrap(),
        limits::PATH,
    );
    assert_fits(
        "path dirs",
        &path::path(
            &r,
            module,
            "src/main/java/com/example/enterprise/platform/very/long/package/name/module00",
        )
        .unwrap(),
        limits::PATH,
    );
    assert_fits(
        "findings",
        &list::findings(&r, &Filter::default()).unwrap(),
        limits::FINDINGS,
    );
    assert_fits(
        "findings within",
        &list::findings(
            &r,
            &Filter {
                within: Some(module.to_string()),
                ..Filter::default()
            },
        )
        .unwrap(),
        limits::FINDINGS,
    );
    assert_fits("check", &check::check(&r), limits::CHECK);
    // Truncation is explicit.
    let f = list::findings(&r, &Filter::default()).unwrap();
    if f.data.more > 0 {
        assert!(
            f.text.contains(&format!("+{} more", f.data.more)),
            "{}",
            f.text
        );
    }
}

/// `diff` and `check --base` against an empty base (everything new) and the reverse
/// (everything removed), with a layer per module so that dozens of layer pairs, violations
/// and unresolved imports change.
#[test]
fn diff_and_check_base_fit_their_budget_on_a_big_repo() {
    let mut g = big_graph();
    for i in 0..300u32 {
        g.unresolved.push(cartograph_core::UnresolvedImport {
            file: g.nodes[1 + (i as usize % 3000)].path.clone(),
            line: i + 1,
            specifier: format!("com.example.missing.VeryLongMissingClassName{i:03}"),
            reason: cartograph_core::UnresolvedReason::NotFound,
            detail: None,
        });
    }
    let prefix = "src/main/java/com/example/enterprise/platform/very/long/package/name";
    let mut toml = String::new();
    for p in 0..12 {
        toml.push_str(&format!(
            "[[layers]]\nname = \"module{p:02}\"\npaths = [\"{prefix}/module{p:02}/**\"]\nallow = []\n\n"
        ));
    }
    toml.push_str("[rules]\nno_cycles = true\nmax_fan_out = 5\n");
    let cfg = Config::parse(&toml, "cartograph.toml").unwrap();
    let mut empty = Graph::default();
    empty.nodes.push(node(NodeKind::Repo, "", None, 0));
    let head = Repo::new(&g, cfg.clone());
    let base = Repo::new(&empty, cfg);
    for (what, a, b) in [("new", &base, &head), ("removed", &head, &base)] {
        let d = cartograph_query::diff::diff(a, b, "main (0123abcd)", "working tree");
        assert_fits(&format!("diff {what}"), &d, limits::DIFF);
        assert!(d.text.contains(" more"), "diff {what}: {}", d.text);
        let c = check::check_base(b, a, "main (0123abcd)");
        assert_fits(&format!("check_base {what}"), &c, limits::CHECK);
    }
    let d = cartograph_query::diff::diff(&base, &head, "main", "working tree");
    assert!(d.data.violations_more > 0 && d.data.unresolved_new_more > 0);
    assert!(
        d.text
            .contains(&format!("+{} more", d.data.unresolved_new_more)),
        "{}",
        d.text
    );
    assert!(!check::check_base(&head, &base, "main").data.ok);
    // The same graph on both sides: nothing changed, nothing fails.
    let same = cartograph_query::diff::diff(&head, &head, "main", "working tree");
    assert!(same.data.is_empty(), "{}", same.text);
    assert!(check::check_base(&head, &head, "main").data.ok);
}

/// A symbol layer for [`big_graph`]: 12 methods per file with long names, one method that
/// every file calls many times, entry points by the hundred.
fn big_symbols(g: &Graph) -> Symbols {
    let mut s = Symbols::default();
    let files: Vec<&str> = g
        .nodes_of(NodeKind::File)
        .map(|n| n.path.as_str())
        .collect();
    let hub = files[0];
    for f in &files {
        for k in 0..12u32 {
            s.symbols.push(Symbol {
                file: f.to_string(),
                name: format!("handleTheIncomingRequestForResourceNumber{k:02}"),
                container: Some("SomethingQuiteDescriptiveServiceImplementation".into()),
                kind: SymbolKind::Method,
                line: 10 + k * 20,
                end_line: 25 + k * 20,
                exported: k % 2 == 0,
            });
        }
    }
    let target = symbol_id(
        hub,
        "SomethingQuiteDescriptiveServiceImplementation.handleTheIncomingRequestForResourceNumber00",
    );
    for f in &files {
        for line in 0..4u32 {
            s.calls.push(SymbolCall {
                file: f.to_string(),
                line: 100 + line,
                caller: Some(symbol_id(
                    f,
                    "SomethingQuiteDescriptiveServiceImplementation.handleTheIncomingRequestForResourceNumber03",
                )),
                target: target.clone(),
                confidence: if line % 2 == 0 {
                    Confidence::Resolved
                } else {
                    Confidence::Heuristic
                },
            });
        }
        s.stats.push(CallStats {
            file: f.to_string(),
            resolved: 2,
            heuristic: 2,
            unresolved: 40,
            ..CallStats::default()
        });
    }
    for (i, f) in files.iter().enumerate().take(400) {
        s.entries.push(EntryPoint {
            file: f.to_string(),
            line: 3,
            kind: if i % 3 == 0 { EntryKind::Cli } else { EntryKind::Http },
            name: format!("GET /api/v1/some/very/long/resource/path/segment/{i:04}/details"),
            target: Some(symbol_id(
                f,
                "SomethingQuiteDescriptiveServiceImplementation.handleTheIncomingRequestForResourceNumber01",
            )),
        });
    }
    s.normalize();
    s
}

/// A class per file of [`big_graph`], each extending the hub file's class, whose chain of
/// supertypes is deeper than the walk (each link with an interface outside the repo).
fn big_hierarchy(g: &Graph) -> Symbols {
    let mut s = big_symbols(g);
    let files: Vec<String> = g.nodes_of(NodeKind::File).map(|n| n.path.clone()).collect();
    let class = "SomethingQuiteDescriptiveServiceImplementation";
    for f in &files {
        s.symbols.push(Symbol {
            file: f.clone(),
            name: class.into(),
            container: None,
            kind: SymbolKind::Class,
            line: 5,
            end_line: 400,
            exported: true,
        });
    }
    let hub = symbol_id(&files[0], class);
    for f in &files[1..] {
        s.supers.push(SuperType {
            sub: symbol_id(f, class),
            name: class.into(),
            target: Some(hub.clone()),
            confidence: Some(Confidence::Resolved),
            file: f.clone(),
            line: 5,
        });
    }
    // The hub's chain: a base in file 1, its base in file 2, … eight deep.
    let mut sub = hub.clone();
    for (k, f) in files.iter().enumerate().skip(1).take(8) {
        let name = format!("{class}Base{k:02}");
        let base = symbol_id(f, &name);
        s.symbols.push(Symbol {
            file: f.clone(),
            name: name.clone(),
            container: None,
            kind: SymbolKind::Class,
            line: 500,
            end_line: 600,
            exported: true,
        });
        s.supers.push(SuperType {
            sub: sub.clone(),
            name,
            target: Some(base.clone()),
            confidence: Some(Confidence::Resolved),
            file: files[0].clone(),
            line: 5,
        });
        s.supers.push(SuperType {
            sub: sub.clone(),
            name: format!("org.example.framework.SomeVeryLongInterfaceName{k:02}"),
            target: None,
            confidence: None,
            file: files[0].clone(),
            line: 5,
        });
        sub = base;
    }
    s.normalize();
    s
}

#[test]
fn hierarchy_fits_its_budget_on_a_big_repo() {
    let g = big_graph();
    let r = Repo::with_symbols(&g, &big_hierarchy(&g), Config::default());
    let hub = "src/main/java/com/example/enterprise/platform/very/long/package/name/module00/implementation/details/SomethingQuiteDescriptiveService00Impl.java";
    let class = "SomethingQuiteDescriptiveServiceImplementation";
    for input in [
        format!("{hub}#{class}"),
        format!("{hub}#{class}.handleTheIncomingRequestForResourceNumber00"),
    ] {
        let h = cartograph_query::hierarchy::hierarchy(&r, &input).unwrap();
        assert_fits(&format!("hierarchy {input}"), &h, limits::HIERARCHY);
        assert_eq!(h.data.subtypes, 3199);
        assert!(h.data.subs_more > 0, "{}", h.text);
        // The chain stops after five hops: five bases and their five interfaces.
        assert_eq!(h.data.supers.len() + h.data.supers_more, 10, "{}", h.text);
        assert!(h.data.supers.iter().all(|x| x.depth <= 5), "{}", h.text);
    }
    assert_fits(
        "explain of the hub with its hierarchy",
        &explain::explain(&r, hub).unwrap(),
        limits::EXPLAIN,
    );
}

#[test]
fn symbol_answers_fit_their_budget_on_a_big_repo() {
    let g = big_graph();
    let r = Repo::with_symbols(&g, &big_symbols(&g), Config::default());
    let hub = "src/main/java/com/example/enterprise/platform/very/long/package/name/module00/implementation/details/SomethingQuiteDescriptiveService00Impl.java";
    let module = "src/main/java/com/example/enterprise/platform/very/long/package/name/module05";
    for n in [hub, module, "."] {
        assert_fits(
            &format!("symbols {n}"),
            &symbols::symbols(&r, n).unwrap(),
            limits::SYMBOLS,
        );
        assert_fits(
            &format!("explain {n}"),
            &explain::explain(&r, n).unwrap(),
            limits::EXPLAIN,
        );
        assert_fits(
            &format!("entries within {n}"),
            &symbols::entries(&r, None, Some(n), false).unwrap(),
            limits::ENTRIES,
        );
    }
    let callers = symbols::callers(
        &r,
        &format!(
            "{hub}#SomethingQuiteDescriptiveServiceImplementation.handleTheIncomingRequestForResourceNumber00"
        ),
    )
    .unwrap();
    assert_fits("callers of the hub", &callers, limits::CALLERS);
    assert_eq!(callers.data.total, 4 * 3200);
    assert!(callers.data.groups_more > 0);
    assert!(
        callers
            .text
            .contains(&format!("+{} more files", callers.data.groups_more)),
        "{}",
        callers.text
    );
    assert_fits(
        "entries",
        &symbols::entries(&r, None, None, false).unwrap(),
        limits::ENTRIES,
    );
    assert_fits(
        "find sym",
        &find::find_symbols(&r, "handle request").unwrap(),
        limits::FIND,
    );
    for depth in [1, 4] {
        assert_fits(
            &format!("overview {depth} with flows"),
            &overview::overview(&r, None, depth).unwrap(),
            limits::OVERVIEW,
        );
    }
    // A bare name shared by thousands of symbols is an error listing a few, not a dump.
    match symbols::callers(&r, "handleTheIncomingRequestForResourceNumber07") {
        Err(QueryError::Ambiguous {
            count, candidates, ..
        }) => {
            assert_eq!(count, 3200);
            assert_eq!(candidates.len(), 5);
        }
        other => panic!("{:?}", other.map(|a| a.text)),
    }
}

#[test]
fn findings_on_a_big_repo_are_quick() {
    let g = big_graph();
    let start = std::time::Instant::now();
    let r = Repo::new(&g, Config::default());
    let n = r.findings().len();
    let ms = start.elapsed().as_millis();
    assert!(n > 0);
    // Debug build on CI: generous.
    assert!(ms < 30_000, "{ms} ms");
}

#[test]
fn overview_counts_the_top_level_folders_it_leaves_out() {
    let mut g = Graph::default();
    g.nodes.push(node(NodeKind::Repo, "", None, 0));
    for i in 0..60u64 {
        let path = format!("folder{i:02}/a.ts");
        g.nodes
            .push(node(NodeKind::File, &path, Some("typescript"), 10 + i));
    }
    g.normalize();
    let r = Repo::new(&g, Config::default());
    let o = overview::overview(&r, None, 1).unwrap();
    assert!(o.data.tree_more > 0);
    assert_eq!(o.data.tree.len() + o.data.tree_more, 60);
    let more = format!("+{} more folders in ./", o.data.tree_more);
    assert!(o.text.contains(&more), "{}", o.text);
}
