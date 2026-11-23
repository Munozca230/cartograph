//! Bridges (`tests/fixtures/workspace-bridges`, and the single-repo `electron-app`): every
//! endpoint and matched bridge, compared with `tests/golden/bridges-*.json`; regenerate with
//! `UPDATE_GOLDEN=1 cargo test -p cartograph-index --test bridges` and review the diff.
//!
//! The traps: a channel written as a constant of another file (resolved) next to one built
//! at run time (counted, never matched); a handler registered through a wrapper method of a
//! base class; a renderer call of a function the preload does not expose; a class name two
//! members declare, imported from a member that declares only one of them; an import of a
//! member's class without declaring its package; a dependency at another version than its
//! member builds.

use cartograph_core::{BridgeKind, Confidence, EdgeKind, Graph, Symbols};
use cartograph_index::{IndexOptions, index};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(name: &str) -> PathBuf {
    repo_root().join("tests/fixtures").join(name)
}

fn golden_view(s: &Symbols) -> Value {
    let endpoints: Vec<String> = s
        .endpoints
        .iter()
        .map(|e| {
            format!(
                "{} {} {:?} {}:{} [{}] {}{}{}",
                e.kind,
                e.role,
                e.key,
                e.file,
                e.line,
                e.confidence,
                e.label,
                e.symbol
                    .as_deref()
                    .map_or(String::new(), |x| format!(" sym {x}")),
                e.detail
                    .as_deref()
                    .map_or(String::new(), |d| format!(" ({d})"))
            )
        })
        .collect();
    let bridges: Vec<String> = s
        .bridges
        .iter()
        .map(|b| {
            format!(
                "{} {} {:?} {}:{} {} -> {}:{} {} [{}]",
                b.kind,
                b.subtype,
                b.key,
                b.src_file,
                b.src_line,
                b.src.as_deref().unwrap_or("-"),
                b.dst_file,
                b.dst_line,
                b.dst.as_deref().unwrap_or("-"),
                b.confidence
            )
        })
        .collect();
    json!({ "endpoints": endpoints, "bridges": bridges })
}

fn check_golden(name: &str, got: &Value) {
    let path = repo_root()
        .join("tests/golden")
        .join(format!("{name}.json"));
    let text = format!("{}\n", serde_json::to_string_pretty(got).unwrap());
    if std::env::var_os("UPDATE_GOLDEN").is_some_and(|v| v == "1") {
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let want: Value = serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e} (run with UPDATE_GOLDEN=1)", path.display())),
    )
    .unwrap();
    if &want != got {
        let out = std::env::temp_dir().join(format!("cartograph-{name}.actual.json"));
        std::fs::write(&out, &text).unwrap();
        panic!(
            "{name}: differs from {} (actual in {}; UPDATE_GOLDEN=1 regenerates)",
            path.display(),
            out.display()
        );
    }
}

fn import(g: &Graph, src: &str, dst: &str) -> Option<Confidence> {
    g.edges
        .iter()
        .find(|e| e.kind == EdgeKind::Import && e.src == src && e.dst == dst)
        .map(|e| e.confidence)
}

#[test]
fn workspace_bridges_match_the_golden_list() {
    let out = index(&fixture("workspace-bridges"), &IndexOptions::default()).unwrap();
    check_golden("bridges-workspace", &golden_view(&out.symbols));
    let g = &out.graph;
    // Same class name in two members: the declared one wins, as a resolved import.
    assert_eq!(
        import(
            g,
            "file:api/src/main/java/com/acme/api/Billing.java",
            "file:lib/src/main/java/com/acme/lib/Money.java"
        ),
        Some(Confidence::Resolved)
    );
    assert_eq!(
        import(
            g,
            "file:api/src/main/java/com/acme/api/Billing.java",
            "file:legacy/src/main/java/com/acme/lib/Money.java"
        ),
        None
    );
    // A member it does not declare: still an import, as a guess.
    assert_eq!(
        import(
            g,
            "file:api/src/main/java/com/acme/api/Reports.java",
            "file:legacy/src/main/java/com/acme/legacy/OldPrices.java"
        ),
        Some(Confidence::Heuristic)
    );
    // A package another member builds, declared in package.json: its source.
    assert_eq!(
        import(
            g,
            "file:desktop/src/renderer/App.tsx",
            "file:ui/src/index.ts"
        ),
        Some(Confidence::Resolved)
    );
    // IPC bridges are derived edges; package bridges are not.
    let bridge_edges: Vec<(&str, &str)> = g
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Bridge)
        .map(|e| (e.src.as_str(), e.dst.as_str()))
        .collect();
    assert!(bridge_edges.contains(&(
        "file:desktop/src/preload/index.ts",
        "file:desktop/src/main/docs.ts"
    )));
    assert!(!bridge_edges.iter().any(|(s, _)| s.ends_with("pom.xml")));
}

#[test]
fn a_single_electron_repo_has_ipc_bridges() {
    let out = index(&fixture("electron-app"), &IndexOptions::default()).unwrap();
    check_golden("bridges-electron-app", &golden_view(&out.symbols));
}

#[test]
fn bridges_turned_off_leave_no_rows_and_no_edges() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    copy_dir(&fixture("electron-app"), &root);
    std::fs::write(root.join("cartograph.toml"), "[bridges]\nipc = false\n").unwrap();
    let out = index(&root, &IndexOptions::default()).unwrap();
    assert!(out.symbols.bridges.is_empty() && out.symbols.endpoints.is_empty());
    assert!(!out.graph.edges.iter().any(|e| e.kind == EdgeKind::Bridge));
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dst = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dst);
        } else {
            std::fs::copy(entry.path(), &dst).unwrap();
        }
    }
}

/// A subpath of a member's package with no source file (built output, a stylesheet left
/// out of the walk) stays the external package it was, not an unresolved import.
#[test]
fn a_member_package_subpath_without_source_stays_external() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    copy_dir(&fixture("workspace-bridges"), &root);
    std::fs::write(
        root.join("desktop/src/renderer/theme.ts"),
        "import '@acme/ui-kit/dist/theme.css';\nexport const theme = 1;\n",
    )
    .unwrap();
    let out = index(&root, &IndexOptions::default()).unwrap();
    let g = &out.graph;
    assert!(
        !g.unresolved
            .iter()
            .any(|u| u.file == "desktop/src/renderer/theme.ts"),
        "{:?}",
        g.unresolved
    );
    assert!(
        g.edges
            .iter()
            .any(|e| e.src == "file:desktop/src/renderer/theme.ts"
                && e.dst == "external:npm:@acme/ui-kit")
    );
}

/// `src:line -> dst symbol [confidence]` of the HTTP bridges.
fn http_bridges(s: &Symbols) -> Vec<String> {
    s.bridges
        .iter()
        .filter(|b| b.kind == BridgeKind::Http)
        .map(|b| {
            format!(
                "{}:{} {} -> {} [{}]",
                b.src_file,
                b.src_line,
                b.key,
                b.dst.as_deref().unwrap_or("-"),
                b.confidence
            )
        })
        .collect()
}

/// `key file:line` of the HTTP endpoints with `detail`.
fn http_left(s: &Symbols, detail: &str) -> Vec<String> {
    s.endpoints
        .iter()
        .filter(|e| e.kind == BridgeKind::Http && e.detail.as_deref() == Some(detail))
        .map(|e| format!("{} {}:{}", e.key, e.file, e.line))
        .collect()
}

/// The HTTP traps of the workspace: a Spring prefix written as a constant of another
/// member, a wrapper three deep whose base is only known at run time, an axios instance
/// imported from another file and a wrapper over it, `/users/me` next to `/users/{id}`, a
/// router mounted twice, the wrong method, a route no member serves, a foreign host and a
/// URL built at run time.
#[test]
fn http_bridges_cross_members_through_wrappers_instances_and_mounts() {
    let out = index(&fixture("workspace-bridges"), &IndexOptions::default()).unwrap();
    let s = &out.symbols;
    let ctl = "sym:api/src/main/java/com/acme/api/UserController.java#UserController";
    assert_eq!(
        http_bridges(s),
        vec![
            format!("web/src/users.ts:4 GET /api/users/{{}} -> {ctl}.get [resolved]"),
            "web/src/users.ts:22 POST /v1/jobs/{} -> sym:gateway/src/jobs.ts#runJob [resolved]"
                .to_string(),
            format!(
                "desktop/src/main/http/UsersApi.ts:11 GET /api/users/{{}} -> {ctl}.get [resolved]"
            ),
            format!(
                "desktop/src/main/http/UsersApi.ts:15 GET /api/users/me -> {ctl}.me [resolved]"
            ),
            format!(
                "desktop/src/main/http/UsersApi.ts:19 POST /api/users -> {ctl}.create [resolved]"
            ),
            // Through an untyped object, with the path in a constant: tied by name.
            format!(
                "desktop/src/main/http/UsersApi.ts:37 GET /api/users/me -> {ctl}.me [heuristic]"
            ),
            format!("web/src/users.ts:13 GET /api/users/search -> {ctl}.search [resolved]"),
        ]
    );
    assert_eq!(
        http_left(s, "unmatched"),
        vec!["POST /api/reports/generate desktop/src/main/http/UsersApi.ts:24"]
    );
    assert_eq!(
        http_left(s, "method_mismatch"),
        vec!["PUT /api/users/me web/src/users.ts:18"]
    );
    assert_eq!(
        http_left(s, "external"),
        vec!["POST audit.example.com/events api/src/main/java/com/acme/api/AuditClient.java:10"]
    );
    assert_eq!(
        http_left(s, "dynamic"),
        vec![" desktop/src/main/http/UsersApi.ts:30"]
    );
    let unused = http_left(s, "unused");
    assert!(unused.contains(
        &"DELETE /api/users/{} api/src/main/java/com/acme/api/UserController.java:31".to_string()
    ));
    // The router is mounted under /v1 and /v2; only /v1 is called.
    assert!(unused.contains(&"POST /v2/jobs/{} gateway/src/jobs.ts:5".to_string()));
    assert!(!unused.iter().any(|u| u.starts_with("POST /v1/jobs")));
    // HTTP bridges are derived edges too.
    assert!(out.graph.edges.iter().any(|e| e.kind == EdgeKind::Bridge
        && e.src == "file:web/src/users.ts"
        && e.dst == "file:gateway/src/jobs.ts"));
}

/// In a single repo HTTP is opt-in (`[bridges] http = true`); a client and its server in
/// the same repo match then.
#[test]
fn a_single_repo_matches_http_when_asked() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("package.json"),
        r#"{ "name": "app", "dependencies": { "express": "4" } }"#,
    )
    .unwrap();
    std::fs::write(
        root.join("src/server.ts"),
        "import express from 'express';\nconst app = express();\napp.get('/api/items/:id', (_q, r) => r.end());\napp.listen(3000);\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/client.ts"),
        "export function item(id: string) {\n  return fetch(`/api/items/${id}`);\n}\n",
    )
    .unwrap();
    let out = index(&root, &IndexOptions::default()).unwrap();
    assert!(http_bridges(&out.symbols).is_empty(), "off by default");
    std::fs::write(root.join("cartograph.toml"), "[bridges]\nhttp = true\n").unwrap();
    let out = index(&root, &IndexOptions::default()).unwrap();
    // The handler is written inline at module level: no symbol runs it.
    assert_eq!(
        http_bridges(&out.symbols),
        vec!["src/client.ts:2 GET /api/items/{} -> - [exact]"]
    );
}

/// A single repo with `[bridges] http = true`, an Express server and `files`.
fn http_repo(files: &[(&str, &str)]) -> Symbols {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("cartograph.toml"), "[bridges]\nhttp = true\n").unwrap();
    std::fs::write(
        root.join("package.json"),
        r#"{ "name": "app", "dependencies": { "express": "4" } }"#,
    )
    .unwrap();
    for (path, text) in files {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    index(&root, &IndexOptions::default()).unwrap().symbols
}

const SERVER: &str = "import express from 'express';
const app = express();
function h() {}
app.get('/api/users', h);
app.get('/api/jobs', h);
app.post('/api/items', h);
app.put('/api/items', h);
app.listen(3000);
";

/// `key file:line` of the HTTP bridges, sorted.
fn http_keys(s: &Symbols) -> Vec<String> {
    let mut v: Vec<String> = s
        .bridges
        .iter()
        .filter(|b| b.kind == BridgeKind::Http)
        .map(|b| format!("{} {}:{}", b.key, b.src_file, b.src_line))
        .collect();
    v.sort();
    v
}

/// Two calls of one wrapper on one line are two sites.
#[test]
fn two_wrapper_calls_on_one_line_are_two_sites() {
    let s = http_repo(&[
        ("src/server.ts", SERVER),
        (
            "src/api.ts",
            "export class Api {
  constructor(private readonly base: string) {}
  get(path: string) {
    return fetch(`${this.base}${path}`);
  }
}
export class Svc {
  constructor(private readonly api: Api) {}
  all() {
    return Promise.all([this.api.get('/api/users'), this.api.get('/api/jobs')]);
  }
}
",
        ),
    ]);
    assert_eq!(
        http_keys(&s),
        vec![
            "GET /api/jobs src/api.ts:10",
            "GET /api/users src/api.ts:10"
        ]
    );
}

/// The options a wrapper hands on may be a local object (`const init = { method }`) or
/// something not read (`makeInit("PUT")`): the method is its `method`, or not known; never
/// the `GET` of a call that passes no options.
#[test]
fn options_passed_on_but_not_written_inline_are_no_get() {
    let s = http_repo(&[
        ("src/server.ts", SERVER),
        (
            "src/api.ts",
            "function makeInit(m: string): RequestInit { return { method: m }; }
export class Api {
  constructor(private readonly base: string) {}
  private send(url: string, init: RequestInit) {
    return fetch(url, { ...init });
  }
  request(method: string, path: string) {
    const init = { method };
    return this.send(`${this.base}${path}`, init);
  }
  put(path: string) {
    return this.send(`${this.base}${path}`, makeInit('PUT'));
  }
  post(path: string) {
    return this.request('POST', path);
  }
}
export class Svc {
  constructor(private readonly api: Api) {}
  make() { return this.api.post('/api/items'); }
  users() { return this.api.request('GET', '/api/users'); }
  update() { return this.api.put('/api/items'); }
}
",
        ),
    ]);
    let left: Vec<String> = s
        .endpoints
        .iter()
        .filter(|e| e.kind == BridgeKind::Http && e.detail.as_deref() == Some("method_mismatch"))
        .map(|e| format!("{} {}", e.key, e.line))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    let keys = http_keys(&s);
    assert!(
        keys.contains(&"POST /api/items src/api.ts:20".to_string()),
        "{keys:?}"
    );
    assert!(
        keys.contains(&"GET /api/users src/api.ts:21".to_string()),
        "{keys:?}"
    );
    // A method not known matches the path with either method.
    assert!(
        keys.contains(&"PUT /api/items src/api.ts:22".to_string()),
        "{keys:?}"
    );
}

/// In one repo of several Spring Boot modules, a module's context path prefixes its own
/// routes only.
#[test]
fn a_context_path_belongs_to_its_module() {
    let controller = |pkg: &str, path: &str| {
        format!(
            "package {pkg};\n@RestController\npublic class C {{\n    @GetMapping(\"{path}\")\n    public String get() {{ return \"\"; }}\n}}\n"
        )
    };
    let a = controller("a", "/x");
    let b = controller("b", "/y");
    let s = http_repo(&[
        (
            "orders/src/main/resources/application.properties",
            "server.servlet.context-path=/orders\n",
        ),
        ("orders/src/main/java/a/C.java", a.as_str()),
        ("users/src/main/java/b/C.java", b.as_str()),
        (
            "src/client.ts",
            "export function x() { return fetch('/orders/x'); }\nexport function y() { return fetch('/y'); }\n",
        ),
    ]);
    assert_eq!(
        http_keys(&s),
        vec!["GET /orders/x src/client.ts:1", "GET /y src/client.ts:2"]
    );
}
