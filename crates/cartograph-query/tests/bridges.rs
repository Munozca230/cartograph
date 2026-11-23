//! Bridges in the queries (`tests/fixtures/workspace-bridges`): callers over IPC with the
//! renderer behind them, deps and explain blocks, impact over bridges, path through them
//! (and without them), findings, the `bridges` answer, flows, and layer rules that count
//! them only when asked to.

mod common;

use cartograph_core::BridgeKind;
use cartograph_query::bridges::{BridgeQuery, bridges};
use cartograph_query::deps::{Direction, Level, deps};
use cartograph_query::{FindingKind, Repo, explain, impact, path, symbols};

fn repo() -> Repo {
    common::fixture("workspace-bridges")
}

#[test]
fn callers_over_ipc_reach_back_to_the_renderer() {
    let r = repo();
    let a = symbols::callers(&r, "DocsModule.onSave").unwrap();
    assert!(
        a.text.contains(
            "over IPC \"docs:save\" (invoke) from desktop/src/preload/index.ts:13 api.saveDoc()"
        ),
        "{}",
        a.text
    );
    assert!(
        a.text.contains("reached from window.api.saveDoc by:"),
        "{}",
        a.text
    );
    assert!(
        a.text.contains("desktop/src/renderer/App.tsx:4 App()"),
        "{}",
        a.text
    );
    // The exposed function's own callers are the renderer's, over the exposure.
    let a = symbols::callers(&r, "api.saveDoc").unwrap();
    assert!(
        a.text.contains(
            "over IPC \"window.api.saveDoc\" (expose) from desktop/src/renderer/App.tsx:4"
        ),
        "{}",
        a.text
    );
}

#[test]
fn deps_and_explain_list_bridges_apart_from_imports() {
    let r = repo();
    let d = deps(
        &r,
        "desktop:src/preload/index.ts",
        Direction::Both,
        1,
        Level::File,
    )
    .unwrap();
    let (uses, _) = d.text.split_once("over bridges").unwrap();
    assert!(
        !uses.contains("desktop/src/main/docs.ts"),
        "a bridge is not an import: {}",
        d.text
    );
    assert!(
        d.text
            .contains("IPC invoke \"docs:save\" → desktop/src/main/docs.ts:6 DocsModule.onSave"),
        "{}",
        d.text
    );
    assert!(
        d.text
            .contains("IPC expose \"window.api.saveDoc\" ← desktop/src/renderer/App.tsx:4 App"),
        "{}",
        d.text
    );
    let e = explain::explain(&r, "lib:pom.xml").unwrap();
    assert!(
        e.text
            .contains("package maven \"maven:com.acme:lib\" ← api/pom.xml:22"),
        "{}",
        e.text
    );
}

#[test]
fn impact_crosses_up_to_two_bridges() {
    let r = repo();
    let i = impact::impact(&r, &["desktop:src/main/docs.ts".to_string()]).unwrap();
    assert!(i.text.contains("reached over bridges"), "{}", i.text);
    assert!(
        i.text
            .contains("desktop/src/preload/index.ts via ipc invoke docs:save"),
        "{}",
        i.text
    );
    assert!(
        i.text
            .contains("desktop/src/renderer/App.tsx via ipc expose window.api.saveDoc"),
        "{}",
        i.text
    );
    assert_eq!(i.data.dependents, 1, "imports only: main.ts");
}

#[test]
fn path_follows_bridges_unless_told_not_to() {
    let r = repo();
    let p = path::path(
        &r,
        "desktop:src/renderer/App.tsx",
        "desktop:src/main/docs.ts",
    )
    .unwrap();
    assert!(
        p.text.contains("desktop/src/renderer/App.tsx:4 (ipc expose window.api.saveDoc) → desktop/src/preload/index.ts:13 (ipc invoke docs:save) → desktop/src/main/docs.ts"),
        "{}",
        p.text
    );
    let p = path::path_with(
        &r,
        "desktop:src/renderer/App.tsx",
        "desktop:src/main/docs.ts",
        false,
    )
    .unwrap();
    assert!(p.data.paths.is_empty(), "{}", p.text);
}

#[test]
fn findings_about_bridges() {
    let r = repo();
    let ids: Vec<(&str, FindingKind)> = r
        .findings()
        .iter()
        .map(|f| (f.id.as_str(), f.kind))
        .collect();
    for (id, kind) in [
        ("ipc_no_handler:desktop:nope", FindingKind::IpcNoHandler),
        (
            "ipc_unexposed:desktop:window.api.notExposed",
            FindingKind::IpcUnexposed,
        ),
        (
            "ipc_unused_handler:desktop:docs:unused",
            FindingKind::IpcUnusedHandler,
        ),
        (
            "undeclared_member_dependency:api->legacy",
            FindingKind::UndeclaredMemberDependency,
        ),
        (
            "version_skew:api/pom.xml:maven:com.acme:lib",
            FindingKind::VersionSkew,
        ),
        (
            "version_skew:desktop/package.json:npm:@acme/ui-kit",
            FindingKind::VersionSkew,
        ),
    ] {
        assert!(ids.contains(&(id, kind)), "{id}: {ids:?}");
    }
    // A channel known only at run time, and a declared member, are never reported.
    assert!(
        !ids.iter()
            .any(|(id, _)| id.contains("plugin") || id.contains("api->lib")),
        "{ids:?}"
    );
    // No bridge is a dependency: no cycle, no new fan-in.
    assert!(
        !ids.iter().any(|(_, k)| *k == FindingKind::Cycle),
        "{ids:?}"
    );
}

#[test]
fn the_bridges_answer_and_its_filters() {
    let r = repo();
    let a = bridges(&r, &BridgeQuery::default()).unwrap();
    assert!(a.text.starts_with("bridges (whole repo): 15 · http axios 1, http fetch 1, http wrapper 5, ipc expose 3, ipc invoke 2, ipc push 1, package maven 1, package npm 1"), "{}", a.text);
    let a = bridges(
        &r,
        &BridgeQuery {
            kind: Some(BridgeKind::Package),
            member: Some("api".into()),
            ..BridgeQuery::default()
        },
    )
    .unwrap();
    assert_eq!(a.data.rows.len(), 1, "{}", a.text);
    let a = bridges(
        &r,
        &BridgeQuery {
            unmatched: true,
            node: Some("desktop:src/preload".into()),
            ..BridgeQuery::default()
        },
    )
    .unwrap();
    assert!(
        a.text.contains("\"nope\"") && a.text.contains("dynamic"),
        "{}",
        a.text
    );
    assert!(
        !a.text.contains("notExposed"),
        "the renderer is outside the node: {}",
        a.text
    );
    let bad = bridges(
        &r,
        &BridgeQuery {
            member: Some("nobody".into()),
            ..BridgeQuery::default()
        },
    );
    assert!(bad.is_err());
}

#[test]
fn flows_continue_over_bridges_with_their_label() {
    let r = repo();
    let ix = cartograph_query::flow::CallIndex::build(&r);
    let f = cartograph_query::flow::flow(
        &r,
        &ix,
        "sym:desktop/src/renderer/App.tsx#App",
        &cartograph_query::flow::FlowOptions::default(),
    )
    .unwrap();
    let via: Vec<(&str, &str, Option<&str>)> = f
        .edges
        .iter()
        .map(|e| (e.src.as_str(), e.dst.as_str(), e.via.as_deref()))
        .collect();
    assert!(
        via.contains(&(
            "sym:desktop/src/preload/index.ts#api.saveDoc",
            "sym:desktop/src/main/docs.ts#DocsModule.onSave",
            Some("ipc invoke docs:save")
        )),
        "{via:?}"
    );
}

/// Ignored files are left out of the findings about packages, at either end.
#[test]
fn ignored_manifests_and_members_leave_package_findings() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    common::copy_dir(
        &common::repo_root().join("tests/fixtures/workspace-bridges"),
        &root,
    );
    let file = root.join("cartograph-workspace.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(
        &file,
        format!("ignore = [\"api/pom.xml\", \"legacy/**\"]\n{text}"),
    )
    .unwrap();
    let out = cartograph_index::index(&root, &cartograph_index::IndexOptions::default()).unwrap();
    let r = Repo::with_symbols(
        &out.graph,
        &out.symbols,
        cartograph_query::Config::load(&root).unwrap(),
    );
    let ids: Vec<&str> = r.findings().iter().map(|f| f.id.as_str()).collect();
    assert!(
        !ids.contains(&"version_skew:api/pom.xml:maven:com.acme:lib"),
        "{ids:?}"
    );
    assert!(
        !ids.contains(&"undeclared_member_dependency:api->legacy"),
        "{ids:?}"
    );
    assert!(
        ids.contains(&"version_skew:desktop/package.json:npm:@acme/ui-kit"),
        "{ids:?}"
    );
}

/// `callers` of a controller method lists the client sites that reach it over HTTP, the
/// one awaited with type arguments and the one through an axios instance included.
#[test]
fn callers_of_a_route_handler_come_over_http() {
    let r = repo();
    let a = symbols::callers(&r, "UserController.get").unwrap();
    for want in [
        "over HTTP \"GET /api/users/{}\" (wrapper) from desktop/src/main/http/UsersApi.ts:11 UsersApi.getUser()",
        "over HTTP \"GET /api/users/{}\" (axios) from web/src/users.ts:4 loadUser()",
    ] {
        assert!(a.text.contains(want), "{want}\n{}", a.text);
    }
    // `/users/me` is its own route, not `/users/{id}`.
    assert!(!a.text.contains("UsersApi.me()"), "{}", a.text);
}

#[test]
fn http_findings_report_what_is_broken_not_what_is_unknown() {
    let r = repo();
    let ids: Vec<(&str, FindingKind)> = r
        .findings()
        .iter()
        .map(|f| (f.id.as_str(), f.kind))
        .collect();
    for (id, kind) in [
        (
            "http_unmatched_call:desktop:POST /api/reports/generate",
            FindingKind::HttpUnmatchedCall,
        ),
        (
            "http_method_mismatch:web:PUT /api/users/me",
            FindingKind::HttpMethodMismatch,
        ),
        (
            "http_unused_route:api:DELETE /api/users/{}",
            FindingKind::HttpUnusedRoute,
        ),
        (
            "http_unused_route:gateway:POST /v2/jobs/{}",
            FindingKind::HttpUnusedRoute,
        ),
    ] {
        assert!(ids.contains(&(id, kind)), "{id}: {ids:?}");
    }
    // A foreign host and a URL built at run time are counted, never reported.
    assert!(
        !ids.iter()
            .any(|(id, _)| id.contains("audit.example.com") || id.ends_with(':')),
        "{ids:?}"
    );
}

#[test]
fn explain_lists_the_calls_no_route_serves() {
    let r = repo();
    let e = explain::explain(&r, "desktop:src/main/http/UsersApi.ts").unwrap();
    assert!(
        e.text.contains("left without the other side:")
            && e.text.contains(
                "HTTP \"POST /api/reports/generate\" at desktop/src/main/http/UsersApi.ts:24"
            ),
        "{}",
        e.text
    );
    assert!(
        e.text.contains("HTTP wrapper \"GET /api/users/me\""),
        "{}",
        e.text
    );
}

#[test]
fn entries_count_the_callers_of_a_route_over_http() {
    let r = repo();
    let a = symbols::entries(&r, None, Some("api:src"), false).unwrap();
    assert!(a.text.contains(" · 2 over HTTP"), "{}", a.text);
    assert!(a.text.contains("no caller over HTTP"), "{}", a.text);
}
