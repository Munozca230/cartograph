//! `/api/findings` and the findings counts of `/api/meta`, with the repo's cartograph.toml.

mod common;

use cartograph_index::IndexOptions;
use cartograph_query::{Config, ConfigError};
use cartograph_server::api;
use cartograph_server::snapshot::Snapshot;
use cartograph_server::state::AppState;
use common::*;
use std::collections::BTreeMap;
use std::sync::Arc;

fn state_with(name: &str, config: Result<Config, ConfigError>) -> Arc<AppState> {
    let root = fixture(name);
    let out = cartograph_index::index(&root, &IndexOptions::default()).unwrap();
    let mut meta = BTreeMap::new();
    meta.insert("repo_name".to_string(), name.to_string());
    let state = Arc::new(AppState::new(
        name.to_string(),
        root.clone(),
        root.join(".x"),
    ));
    state.set_port(PORT);
    state.set_snapshot(Arc::new(Snapshot::build_with(out.graph, &meta, 1, config)));
    state
}

#[tokio::test]
async fn findings_follow_the_rules_of_cartograph_toml() {
    let config = Config::load(&fixture("query-app"));
    let app = api::router(state_with("query-app", config));
    let (status, f) = get(&app, "/api/findings").await;
    assert_eq!(status, 200);
    assert_eq!(f["generation"], 1);
    assert_eq!(f["config"]["origin"], "cartograph.toml");
    assert_eq!(f["config"]["error"], serde_json::Value::Null);
    let list = f["findings"].as_array().unwrap();
    assert_eq!(list.len() as u64, f["total"].as_u64().unwrap());
    assert_eq!(f["truncated"], false);
    // Violations first, each with severity, title, location ids and evidence edges.
    let first = &list[0];
    assert_eq!(first["violation"], true);
    assert_eq!(first["id"], "layer_violation:core->ui");
    assert_eq!(first["severity"], "error");
    assert_eq!(first["nodes"][0], "file:src/core/util.ts");
    assert_eq!(first["edges"][0]["src"], "file:src/core/util.ts");
    assert_eq!(first["edges"][0]["dst"], "file:src/ui/widgets.ts");
    assert_eq!(first["edges"][0]["line"], 1);
    assert_eq!(f["summary"]["violations"], 5);

    // meta counts the problems (error and warning) apart from the informational ones.
    let (_, m) = get(&app, "/api/meta").await;
    let problems = list.iter().filter(|x| x["severity"] != "info").count() as u64;
    assert_eq!(m["counts"]["findings"].as_u64(), Some(problems));
    assert_eq!(
        m["counts"]["findings_info"].as_u64(),
        Some(list.len() as u64 - problems)
    );

    let (_, f) = get(&app, "/api/findings?limit=2").await;
    assert_eq!(f["findings"].as_array().unwrap().len(), 2);
    assert_eq!(f["truncated"], true);
    let (status, _) = get(&app, "/api/findings?limit=x").await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn a_broken_cartograph_toml_is_reported_not_fatal() {
    let bad = Config::parse("[rules]\nmax_fan_out = 0\n", "cartograph.toml");
    let app = api::router(state_with("query-app", bad));
    let (status, f) = get(&app, "/api/findings").await;
    assert_eq!(status, 200);
    let err = f["config"]["error"].as_str().unwrap();
    assert!(
        err.starts_with("cartograph.toml:2: rules.max_fan_out"),
        "{err}"
    );
    // The defaults still find cycles and orphans; nothing is a rule violation.
    assert!(f["total"].as_u64().unwrap() > 0);
    assert_eq!(f["summary"]["violations"], 0);
}
