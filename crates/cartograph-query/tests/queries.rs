//! Golden outputs (text and JSON) of every query on the `query-app` fixture, plus the
//! contracts every answer keeps: budget, stable ids, errors with suggestions.
//! Regenerate with `UPDATE_GOLDEN=1 cargo test -p cartograph-query --test queries` and
//! review the diff.

mod common;

use cartograph_query::budget::{JSON_BUDGET_FACTOR, json_tokens, limits, tokens};
use cartograph_query::deps::{Direction, Level};
use cartograph_query::find::FindKind;
use cartograph_query::list::Filter;
use cartograph_query::{
    Answer, FindingKind, QueryError, Repo, check, deps, explain, find, impact, infer, list,
    overview, path, symbols,
};
use common::{fixture, golden};
use serde::Serialize;
use std::sync::OnceLock;

fn app() -> &'static Repo {
    static APP: OnceLock<Repo> = OnceLock::new();
    APP.get_or_init(|| fixture("query-app"))
}

/// Check the budget, then compare text and JSON with their goldens.
fn both<T: Serialize>(name: &str, a: &Answer<T>, budget: usize) {
    assert!(
        tokens(&a.text) <= budget,
        "{name}: text over budget:\n{}",
        a.text
    );
    assert!(
        json_tokens(&a.data) <= budget * JSON_BUDGET_FACTOR,
        "{name}: JSON over budget"
    );
    golden(&format!("query-app/{name}.txt"), &a.text);
    let json = serde_json::to_string_pretty(&a.data).unwrap() + "\n";
    golden(&format!("query-app/{name}.json"), &json);
}

#[test]
fn overview_golden() {
    both(
        "overview",
        &overview::overview(app(), None, 2).unwrap(),
        limits::OVERVIEW,
    );
    both(
        "overview-src",
        &overview::overview(app(), Some("src"), 1).unwrap(),
        limits::OVERVIEW,
    );
}

#[test]
fn explain_golden() {
    both(
        "explain-model",
        &explain::explain(app(), "src/core/model.ts").unwrap(),
        limits::EXPLAIN,
    );
    both(
        "explain-core",
        &explain::explain(app(), "dir:src/core").unwrap(),
        limits::EXPLAIN,
    );
    both(
        "explain-react",
        &explain::explain(app(), "npm:react").unwrap(),
        limits::EXPLAIN,
    );
    let e = explain::explain(app(), "src/core/model.ts").unwrap().data;
    // Slots later tasks fill are there and empty; symbols are filled.
    assert!(e.git.is_none() && e.annotation.is_none());
    let s = e.symbols.unwrap();
    assert_eq!((s.total, s.exported), (1, 1));
    assert_eq!(s.main[0].id, "sym:src/core/model.ts#Model");
    assert_eq!(e.layer.as_deref(), Some("core"));
    let lib = explain::explain(app(), "npm:react").unwrap().data;
    assert!(lib.symbols.is_none());
}

#[test]
fn symbols_callers_and_entries_golden() {
    both(
        "symbols-core",
        &symbols::symbols(app(), "src/core").unwrap(),
        limits::SYMBOLS,
    );
    both(
        "symbols-repo",
        &symbols::symbols(app(), "src/core/repo.ts").unwrap(),
        limits::SYMBOLS,
    );
    both(
        "callers-model",
        &symbols::callers(app(), "Model").unwrap(),
        limits::CALLERS,
    );
    both(
        "entries",
        &symbols::entries(app(), None, None, false).unwrap(),
        limits::ENTRIES,
    );
    both(
        "find-sym-run",
        &find::find_symbols(app(), "run").unwrap(),
        limits::FIND,
    );
    let c = symbols::callers(app(), "src/core/service.ts#run")
        .unwrap()
        .data;
    assert_eq!(c.symbol, "sym:src/core/service.ts#run");
    let app_ts = c
        .groups
        .iter()
        .find(|g| g.file == "file:src/ui/app.ts")
        .unwrap();
    assert_eq!(
        app_ts.sites[0].caller.as_deref(),
        Some("sym:src/ui/app.ts#start")
    );
}

/// Flow entries with content: a Rust crate with `main`, an axum route and clap commands.
#[test]
fn flow_entries_golden() {
    let r = fixture("symbols-rust");
    let check = |name: &str, text: &str, json: String, budget: usize| {
        assert!(tokens(text) <= budget, "{name}: {text}");
        golden(&format!("query-symbols-rust/{name}.txt"), text);
        golden(&format!("query-symbols-rust/{name}.json"), &json);
    };
    let pretty = |v: &dyn erased::Ser| v.pretty();
    let e = symbols::entries(&r, None, None, false).unwrap();
    check("entries", &e.text, pretty(&e.data), limits::ENTRIES);
    assert!(e.text.contains("http GET /health → health "), "{}", e.text);
    let o = overview::overview(&r, None, 1).unwrap();
    check("overview", &o.text, pretty(&o.data), limits::OVERVIEW);
    assert!(
        o.text.contains("flow entries: cli 2, http 1, main 1"),
        "{}",
        o.text
    );
    let s = symbols::symbols(&r, "src").unwrap();
    check("symbols-src", &s.text, pretty(&s.data), limits::SYMBOLS);
    let only = symbols::entries(
        &r,
        Some(cartograph_core::EntryKind::Cli),
        Some("src/main.rs"),
        false,
    )
    .unwrap()
    .data;
    assert_eq!(only.total, 2);
}

/// Pretty JSON of any answer's data, for goldens.
mod erased {
    pub trait Ser {
        fn pretty(&self) -> String;
    }
    impl<T: serde::Serialize> Ser for T {
        fn pretty(&self) -> String {
            serde_json::to_string_pretty(self).unwrap() + "\n"
        }
    }
}

#[test]
fn symbol_ids_resolve_with_suggestions() {
    let t = &app().symbols;
    for input in [
        "sym:src/core/model.ts#Model",
        "src/core/model.ts#Model",
        "src\\core\\model.ts#Model",
        "Model",
    ] {
        assert_eq!(
            t.resolve(input).unwrap(),
            "sym:src/core/model.ts#Model",
            "{input}"
        );
    }
    match t.resolve("Modle") {
        Err(QueryError::UnknownNode { suggestions, .. }) => {
            assert!(
                suggestions.contains(&"sym:src/core/model.ts#Model".to_string()),
                "{suggestions:?}"
            );
        }
        other => panic!("{other:?}"),
    }
    match t.resolve("src/core/model.ts#Nope") {
        Err(QueryError::UnknownNode { .. }) => {}
        other => panic!("{other:?}"),
    }
    // A repo without a symbol layer says so.
    let bare = cartograph_query::Repo::new(&cartograph_core::Graph::default(), Default::default());
    assert!(matches!(
        symbols::callers(&bare, "x"),
        Err(QueryError::BadArgument(_))
    ));
}

#[test]
fn deps_golden() {
    both(
        "deps-app",
        &deps::deps(app(), "src/ui/app.ts", Direction::Both, 2, Level::File).unwrap(),
        limits::DEPS,
    );
    both(
        "deps-src-folders",
        &deps::deps(app(), "src/ui", Direction::Both, 3, Level::Folder).unwrap(),
        limits::DEPS,
    );
}

#[test]
fn impact_golden() {
    let a = impact::impact(app(), &["src/core/model.ts".to_string()]).unwrap();
    both("impact-model", &a, limits::IMPACT);
    let tests: Vec<&str> = a.data.tests.iter().map(|t| t.id.as_str()).collect();
    // Tests that reach model.ts through service, app and main.
    assert!(
        tests.contains(&"file:src/core/service.test.ts"),
        "{tests:?}"
    );
    assert!(tests.contains(&"file:src/ui/app.spec.ts"), "{tests:?}");
    assert!(tests.contains(&"file:tests/e2e.test.ts"), "{tests:?}");
}

#[test]
fn find_golden() {
    both(
        "find-serv",
        &find::find(app(), "serv", None).unwrap(),
        limits::FIND,
    );
    let only_dirs = find::find(app(), "core", Some(FindKind::Dir)).unwrap();
    assert!(only_dirs.data.hits.iter().all(|h| h.kind == FindKind::Dir));
}

#[test]
fn path_golden() {
    both(
        "path-ui-infra",
        &path::path(app(), "src/ui", "src/infra").unwrap(),
        limits::PATH,
    );
    both(
        "path-main-repo",
        &path::path(app(), "src/main.ts", "src/core/repo.ts").unwrap(),
        limits::PATH,
    );
    // No path one way: says so and shows the other way.
    let a = path::path(app(), "src/core/repo.ts", "src/main.ts").unwrap();
    assert!(a.data.paths.is_empty());
    assert!(a.data.reverse.is_some());
    assert!(a.text.contains("does not depend on"), "{}", a.text);
}

#[test]
fn findings_and_check_golden() {
    both(
        "findings",
        &list::findings(app(), &Filter::default()).unwrap(),
        limits::FINDINGS,
    );
    let only = list::findings(
        app(),
        &Filter {
            kinds: vec![FindingKind::Orphan],
            ..Filter::default()
        },
    )
    .unwrap();
    both("findings-orphans", &only, limits::FINDINGS);
    let c = check::check(app());
    both("check", &c, limits::CHECK);
    assert!(!c.data.ok);
}

#[test]
fn infer_golden() {
    let a = infer::infer(app());
    golden("query-app/infer.toml", &a.text);
    // The proposal parses and passes check by construction.
    let cfg = cartograph_query::Config::parse(&a.data.toml, "infer").unwrap();
    assert!(!cfg.layers.is_empty());
}

#[test]
fn ids_round_trip_and_unknown_ids_suggest() {
    let r = app();
    for input in [
        "src/core/model.ts",
        "file:src/core/model.ts",
        "./src/core/model.ts",
        "src\\core\\model.ts",
    ] {
        let n = r.resolve(input).unwrap();
        assert_eq!(r.graph.id(n), "file:src/core/model.ts", "{input}");
    }
    for input in ["src/core", "src/core/", "dir:src/core"] {
        assert_eq!(
            r.graph.id(r.resolve(input).unwrap()),
            "dir:src/core",
            "{input}"
        );
    }
    for input in ["", ".", "./", "repo:"] {
        assert_eq!(r.graph.id(r.resolve(input).unwrap()), "repo:", "{input:?}");
    }
    for input in ["react", "npm:react", "external:npm:react", "ext:npm:react"] {
        assert_eq!(
            r.graph.id(r.resolve(input).unwrap()),
            "external:npm:react",
            "{input}"
        );
    }
    // Every id a query prints resolves back to the same node.
    let o = overview::overview(r, None, 3).unwrap().data;
    for row in &o.tree {
        assert_eq!(r.graph.id(r.resolve(&row.id).unwrap()), row.id);
    }
    match r.resolve("src/core/modle.ts") {
        Err(QueryError::UnknownNode { suggestions, .. }) => {
            assert!(
                suggestions.iter().any(|s| s == "src/core/model.ts"),
                "{suggestions:?}"
            );
        }
        other => panic!("{other:?}"),
    }
    let e = explain::explain(r, "nope/nothing.rs").unwrap_err();
    assert!(
        e.to_string()
            .starts_with("unknown node \"nope/nothing.rs\""),
        "{e}"
    );
}

#[test]
fn bad_arguments_are_errors_not_panics() {
    let r = app();
    assert!(matches!(
        deps::deps(r, "src/main.ts", Direction::Out, 0, Level::File),
        Err(QueryError::BadArgument(_))
    ));
    assert!(matches!(
        deps::deps(r, "src/main.ts", Direction::Out, 4, Level::File),
        Err(QueryError::BadArgument(_))
    ));
    assert!(matches!(
        deps::deps(r, "react", Direction::Out, 1, Level::File),
        Err(QueryError::BadArgument(_))
    ));
    assert!(deps::deps(r, "react", Direction::In, 2, Level::Folder).is_ok());
    assert!(matches!(
        overview::overview(r, Some("src/main.ts"), 1),
        Err(QueryError::BadArgument(_))
    ));
    assert!(matches!(
        path::path(r, "src", "src/core"),
        Err(QueryError::BadArgument(_))
    ));
    assert!(matches!(
        path::path(r, "src/main.ts", "react"),
        Err(QueryError::BadArgument(_))
    ));
    assert!(matches!(
        impact::impact(r, &[]),
        Err(QueryError::BadArgument(_))
    ));
    assert!(matches!(
        find::find(r, "  ", None),
        Err(QueryError::BadArgument(_))
    ));
    assert_eq!("symbol".parse::<FindKind>().unwrap(), FindKind::Sym);
    assert!("module".parse::<FindKind>().is_err());
    for weird in [
        "file:",
        "dir:",
        "external:",
        "ext:",
        "..",
        "/",
        "ñ/é",
        "a\0b",
        "*",
    ] {
        let _ = r.resolve(weird);
        let _ = explain::explain(r, weird);
        let _ = find::find(r, weird, None);
    }
    // Overview depth is clamped, not an error.
    assert!(overview::overview(r, None, 0).is_ok());
    assert!(overview::overview(r, None, 99).is_ok());
}
