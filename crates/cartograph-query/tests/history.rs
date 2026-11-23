//! Queries over the git layer of a synthetic repository (`cartograph-index`'s
//! `support/git_fixture.rs`): hidden coupling, `explain`, `hotspots` and `impact`, with
//! goldens (dates and people are fixed, so the answers are too) and budgets.
//!
//! Regenerate with `UPDATE_GOLDEN=1 cargo test -p cartograph-query --test history`.

mod common;
#[path = "../../cartograph-index/tests/support/git_fixture.rs"]
mod git_fixture;

use cartograph_core::Store;
use cartograph_index::{IndexOptions, git, index_to_store};
use cartograph_query::budget::{JSON_BUDGET_FACTOR, json_tokens, limits, tokens};
use cartograph_query::list::Filter;
use cartograph_query::{Answer, Config, FindingKind, Repo, explain, hotspots, impact, list};
use common::golden;
use serde::Serialize;

struct Indexed {
    _repo: git_fixture::GitRepo,
    _data: tempfile::TempDir,
    store: Store,
}

fn indexed() -> Indexed {
    let repo = git_fixture::history_repo();
    let data = tempfile::tempdir().unwrap();
    let opts = IndexOptions {
        data_dir: Some(data.path().to_path_buf()),
        ..IndexOptions::default()
    };
    index_to_store(&repo.root, &opts).unwrap();
    let mut store = Store::open(data.path().join("graph.db")).unwrap();
    git::update(&repo.root, &mut store).unwrap();
    Indexed {
        _repo: repo,
        _data: data,
        store,
    }
}

fn repo_with(ix: &Indexed, config: Config) -> Repo {
    let (graph, symbols) = ix.store.load_graph_and_symbols().unwrap();
    Repo::with_symbols(&graph, &symbols, config).with_history(&ix.store.load_history().unwrap())
}

fn check<T: Serialize>(name: &str, a: &Answer<T>, budget: usize) {
    assert!(tokens(&a.text) <= budget, "{name}: text over budget");
    assert!(
        json_tokens(&a.data) <= budget * JSON_BUDGET_FACTOR,
        "{name}: json over budget"
    );
    golden(&format!("query-history/{name}.txt"), &a.text);
    let json = serde_json::to_string_pretty(&a.data).unwrap() + "\n";
    golden(&format!("query-history/{name}.json"), &json);
}

#[test]
fn git_answers_golden() {
    let ix = indexed();
    let r = repo_with(&ix, Config::default());

    // One hidden coupling: client ↔ server. api ↔ db change as often, but api imports db.
    let hidden: Vec<&str> = r
        .findings()
        .iter()
        .filter(|f| f.kind == FindingKind::HiddenCoupling)
        .map(|f| f.id.as_str())
        .collect();
    assert_eq!(
        hidden,
        vec!["hidden_coupling:file:src/client.ts->file:src/server.ts"]
    );
    let f = r
        .findings()
        .iter()
        .find(|f| f.kind == FindingKind::HiddenCoupling)
        .unwrap();
    assert_eq!(f.metrics["together"], 10.0);
    assert!(!f.violation);
    let findings = list::findings(
        &r,
        &Filter {
            kinds: vec![FindingKind::HiddenCoupling],
            ..Filter::default()
        },
    )
    .unwrap();
    check("findings-hidden", &findings, limits::FINDINGS);

    check(
        "explain-server",
        &explain::explain(&r, "src/server.ts").unwrap(),
        limits::EXPLAIN,
    );
    // A renamed file: util.ts's history is helpers.ts's.
    check(
        "explain-helpers",
        &explain::explain(&r, "src/helpers.ts").unwrap(),
        limits::EXPLAIN,
    );
    check(
        "explain-src",
        &explain::explain(&r, "src").unwrap(),
        limits::EXPLAIN,
    );
    check(
        "hotspots",
        &hotspots::hotspots(&r, None).unwrap(),
        limits::HOTSPOTS,
    );
    check(
        "hotspots-gen",
        &hotspots::hotspots(&r, Some("gen")).unwrap(),
        limits::HOTSPOTS,
    );
    let i = impact::impact(&r, &["src/client.ts".to_string()]).unwrap();
    assert_eq!(i.data.also_changes[0].id, "file:src/server.ts");
    check("impact-client", &i, limits::IMPACT);
    // db's partner api already depends on it: nothing extra to name.
    let i = impact::impact(&r, &["src/db.ts".to_string()]).unwrap();
    assert!(i.data.also_changes.is_empty(), "{:?}", i.data.also_changes);
}

/// A git read that failed after a good one keeps the stored history (`git::update` records
/// the error and leaves the rows): `hotspots` and `explain` show it, with the error.
#[test]
fn history_kept_after_a_failed_read_is_shown_with_the_error() {
    let ix = indexed();
    let (graph, symbols) = ix.store.load_graph_and_symbols().unwrap();
    let mut history = ix.store.load_history().unwrap();
    let st = history.status.as_mut().unwrap();
    st.state = cartograph_core::GitState::Error;
    st.error = Some("object database locked".into());
    let r = Repo::with_symbols(&graph, &symbols, Config::default()).with_history(&history);

    let h = hotspots::hotspots(&r, None).unwrap();
    assert!(h.data.git.is_none(), "{:?}", h.data.git);
    assert!(!h.data.items.is_empty());
    assert!(h.text.contains("src/server.ts"), "{}", h.text);
    assert!(h.text.contains("object database locked"), "{}", h.text);

    let e = explain::explain(&r, "src/server.ts").unwrap();
    assert!(e.data.git.is_some());
    assert!(e.text.contains("commits (+"), "{}", e.text);
    assert!(e.text.contains("object database locked"), "{}", e.text);
}

/// With a failed read recorded, a file no commit touches says so (there is no kept
/// history of it to show), and a folder names the failure without claiming to show
/// history (its history is per file).
#[test]
fn a_failed_read_does_not_claim_history_it_does_not_show() {
    let repo = git_fixture::history_repo();
    repo.write("src/fresh.ts", "export const fresh = 1;\n");
    let data = tempfile::tempdir().unwrap();
    let opts = IndexOptions {
        data_dir: Some(data.path().to_path_buf()),
        ..IndexOptions::default()
    };
    index_to_store(&repo.root, &opts).unwrap();
    let mut store = Store::open(data.path().join("graph.db")).unwrap();
    git::update(&repo.root, &mut store).unwrap();
    let (graph, symbols) = store.load_graph_and_symbols().unwrap();
    let mut history = store.load_history().unwrap();
    let st = history.status.as_mut().unwrap();
    st.state = cartograph_core::GitState::Error;
    st.error = Some("object database locked".into());
    let r = Repo::with_symbols(&graph, &symbols, Config::default()).with_history(&history);

    let e = explain::explain(&r, "src/fresh.ts").unwrap();
    assert!(e.data.git.is_none());
    assert!(e.text.contains("no commit touches it"), "{}", e.text);
    assert!(!e.text.contains("showing the history"), "{}", e.text);

    let d = explain::explain(&r, "dir:src").unwrap();
    assert!(d.data.git.is_none());
    assert!(d.text.contains("hotspots --within dir:src"), "{}", d.text);
    assert!(d.text.contains("object database locked"), "{}", d.text);
    assert!(!d.text.contains("showing the history"), "{}", d.text);
}

#[test]
fn thresholds_and_switches_from_the_config() {
    let ix = indexed();
    let hidden = |toml: &str| {
        let r = repo_with(&ix, Config::parse(toml, "cartograph.toml").unwrap());
        r.findings()
            .iter()
            .filter(|f| f.kind == FindingKind::HiddenCoupling)
            .count()
    };
    assert_eq!(hidden(""), 1);
    assert_eq!(hidden("[thresholds]\ncoupling_min_shared = 11\n"), 0);
    assert_eq!(hidden("[thresholds]\ncoupling_min_degree = 1.0\n"), 0);
    assert_eq!(hidden("[thresholds]\nmax_coupling = 0\n"), 0);
    assert_eq!(hidden("[findings]\ndisable = [\"hidden_coupling\"]\n"), 0);
    assert_eq!(hidden("ignore = [\"src/server.ts\"]\n"), 0);
    let e =
        Config::parse("[thresholds]\ncoupling_min_degree = 2\n", "cartograph.toml").unwrap_err();
    assert_eq!(e.key.as_deref(), Some("thresholds.coupling_min_degree"));
    // Without history there is no hidden coupling, and explain says why.
    let (graph, symbols) = ix.store.load_graph_and_symbols().unwrap();
    let bare = Repo::with_symbols(&graph, &symbols, Config::default());
    assert!(
        !bare
            .findings()
            .iter()
            .any(|f| f.kind == FindingKind::HiddenCoupling)
    );
    let e = explain::explain(&bare, "src/server.ts").unwrap();
    assert!(e.data.git.is_none());
    assert!(e.text.contains("git: not indexed"), "{}", e.text);
    let h = hotspots::hotspots(&bare, None).unwrap();
    assert!(h.text.contains("git history not indexed"), "{}", h.text);
}
