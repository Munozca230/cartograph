//! A workspace end to end (`tests/fixtures/workspace-basic`, members made git repositories
//! at test time): `index`, `query` with aliases and member-relative paths, `check` with the
//! workspace's and the members' rules, and one MCP server over the whole workspace, also
//! started from inside a member with `--workspace FILE --member NAME`.

#[path = "../../cartograph-index/tests/support/git_fixture.rs"]
mod git_fixture;
mod mcp_client;
#[path = "../../cartograph-index/tests/support/workspace_fixture.rs"]
mod workspace_fixture;

use mcp_client::{Client, text};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use workspace_fixture::workspace_basic;

const T: Duration = Duration::from_secs(60);

fn cartograph(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_cartograph"))
        .args(args)
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn s(p: &Path) -> String {
    p.display().to_string()
}

#[test]
fn index_query_and_check_a_workspace() {
    let ws = workspace_basic();
    let data = tempfile::tempdir().unwrap();
    let (root, d) = (s(&ws.root), s(data.path()));
    let (code, out, err) = cartograph(&["index", &root, "--data-dir", &d]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("git: members pass"), "{out}");

    let q = |args: &[&str]| {
        let mut all = vec!["query", "-C", &root, "--data-dir", &d];
        all.extend_from_slice(args);
        cartograph(&all)
    };
    // The alias and the canonical path name the same file; outputs are canonical.
    let (code, out, err) = q(&["explain", "api:src/main/java/com/acme/api/UserService.java"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("file api/src/main/java/com/acme/api/UserService.java"),
        "{out}"
    );
    assert!(out.contains("lib/src/main/java/com/acme/lib/"), "{out}");
    // A path of one member only resolves; one of several is ambiguous, unless --member.
    let (code, out, _) = q(&["deps", "src/tools.ts"]);
    assert_eq!(code, 0);
    assert!(out.contains("desktop/src/shared/channels.ts"), "{out}");
    let (code, _, err) = q(&["explain", "pom.xml"]);
    assert_ne!(code, 0);
    assert!(err.contains("api:pom.xml, lib:pom.xml"), "{err}");
    let (code, out, _) = q(&["explain", "pom.xml", "--member", "lib"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("file lib/pom.xml"), "{out}");
    let (code, _, err) = q(&["explain", "pom.xml", "--member", "nope"]);
    assert_ne!(code, 0);
    assert!(err.contains("not a member"), "{err}");
    // Impact crosses members: the library's DTO reaches the API and its test.
    let (_, out, _) = q(&["impact", "lib:src/main/java/com/acme/lib/UserDto.java"]);
    assert!(
        out.contains("api/src/main/java/com/acme/api/UserController.java"),
        "{out}"
    );
    assert!(
        out.contains("api/src/test/java/com/acme/api/UserServiceTest.java"),
        "{out}"
    );

    // Symbol ids are canonical too: the listing's id prefix starts with the member.
    let (code, out, err) = q(&[
        "--json",
        "symbols",
        "api:src/main/java/com/acme/api/UserController.java",
    ]);
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        v["groups"][0]["id_prefix"], "sym:api/src/main/java/com/acme/api/UserController.java#",
        "{v}"
    );

    // infer proposes layers in the workspace's grammar and never rewrites its file.
    let (code, out, err) = cartograph(&["infer", &root, "--data-dir", &d]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("[[layers]]") && out.contains("\"api/**\""),
        "{out}"
    );
    assert!(err.contains("cartograph-workspace.toml"), "{err}");
    let (code, _, err) = cartograph(&["infer", &root, "--data-dir", &d, "--write"]);
    assert_ne!(code, 0);
    assert!(err.contains("does not rewrite"), "{err}");

    // Rules: the workspace's layers, and the API's own (rebased) layers.
    let (code, out, _) = cartograph(&["check", &root, "--data-dir", &d]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("rules from cartograph-workspace.toml"),
        "{out}"
    );
    assert!(
        out.contains("layer_violation:api.service->api.client"),
        "{out}"
    );
}

/// Call until the answer is not "indexing" (the first index may still run).
fn call_ready(c: &mut Client, tool: &str, args: Value) -> Value {
    let deadline = Instant::now() + T;
    loop {
        let r = c.call(tool, args.clone(), T);
        if !text(&r).contains("is indexing") || Instant::now() > deadline {
            return r;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn one_mcp_server_for_the_workspace() {
    let ws = workspace_basic();
    let data = tempfile::tempdir().unwrap();
    let d = s(data.path());
    let mut c = Client::spawn(&ws.root, &["--data-dir", &d, "--no-watch", "--no-stats"]);
    c.handshake();
    let r = call_ready(&mut c, "overview", json!({}));
    let t = text(&r);
    for m in ["api/", "lib/", "desktop/", "mcp/"] {
        assert!(t.contains(m), "{t}");
    }
    let r = c.call("deps", json!({"node": "mcp:src/tools.ts"}), T);
    assert!(
        text(&r).contains("desktop/src/shared/channels.ts"),
        "{}",
        text(&r)
    );
    // An absolute path inside a member's folder (named otherwise) is that member's.
    let abs = ws
        .root
        .join("backend-api/src/main/java/com/acme/api/UserService.java");
    let r = c.call("explain", json!({"node": s(&abs)}), T);
    assert!(
        text(&r).contains("api/src/main/java/com/acme/api/UserService.java"),
        "{}",
        text(&r)
    );
    let r = c.call(
        "callers",
        json!({"symbol": "api:src/main/java/com/acme/api/UserService.java#UserService.find"}),
        T,
    );
    assert!(text(&r).contains("UserController.get"), "{}", text(&r));
    let r = c.call("explain", json!({"node": "package.json"}), T);
    assert_eq!(r["isError"], true, "{r}");
    assert!(text(&r).contains("desktop:package.json"), "{}", text(&r));
    // Freshness without a watcher: a request naming a member file edited on disk (in its
    // folder, named otherwise) re-indexes it first.
    let client = ws
        .root
        .join("backend-api/src/main/java/com/acme/api/ReportClient.java");
    let source = std::fs::read_to_string(&client).unwrap().replace(
        "import org.springframework",
        "import com.acme.lib.Money;\nimport org.springframework",
    );
    std::fs::write(&client, source).unwrap();
    let r = c.call(
        "explain",
        json!({"node": "api:src/main/java/com/acme/api/ReportClient.java"}),
        T,
    );
    assert!(
        text(&r).contains("lib/src/main/java/com/acme/lib/"),
        "{}",
        text(&r)
    );
    // open_map takes the alias and focuses the canonical id.
    let r = c.call(
        "open_map",
        json!({"focus": "api:src/main/java/com/acme/api/UserController.java", "open_browser": false}),
        T,
    );
    assert!(
        text(&r).contains("focus=file:api/src/main/java/com/acme/api/UserController.java"),
        "{}",
        text(&r)
    );
    c.shutdown(T);
}

#[test]
fn an_mcp_server_from_inside_a_member() {
    let ws = workspace_basic();
    let data = tempfile::tempdir().unwrap();
    let file = ws.root.join("cartograph-workspace.toml");
    let (f, d) = (s(&file), s(data.path()));
    let member = ws.root.join("backend-api");
    let mut c = Client::spawn(
        &member,
        &[
            "--workspace",
            &f,
            "--member",
            "api",
            "--data-dir",
            &d,
            "--no-watch",
            "--no-stats",
        ],
    );
    c.handshake();
    // A plain path is the member's first: the API's pom, not the library's.
    let r = call_ready(&mut c, "explain", json!({"node": "pom.xml"}));
    assert!(text(&r).starts_with("file api/pom.xml"), "{}", text(&r));
    let r = c.call("explain", json!({"node": "lib:pom.xml"}), T);
    assert!(text(&r).starts_with("file lib/pom.xml"), "{}", text(&r));
    c.shutdown(T);

    let (code, _, err) = cartograph(&["mcp", "--workspace", &s(&member.join("pom.xml"))]);
    assert_ne!(code, 0);
    assert!(err.contains("must be named"), "{err}");
}

#[test]
fn diff_and_check_against_each_members_base() {
    let ws = workspace_basic();
    std::fs::remove_dir_all(ws.root.join("mcp/.git")).unwrap();
    let data = tempfile::tempdir().unwrap();
    let (root, d) = (s(&ws.root), s(data.path()));
    // The library's model starts using the API's client: a new dependency across members.
    let money = ws
        .root
        .join("shared-lib/src/main/java/com/acme/lib/Money.java");
    let text = std::fs::read_to_string(&money).unwrap().replace(
        "package com.acme.lib;\n",
        "package com.acme.lib;\n\nimport com.acme.api.ReportClient;\n",
    );
    std::fs::write(&money, text).unwrap();

    let (code, out, err) = cartograph(&["diff", "-C", &root, "--data-dir", &d]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("lib: merge-base with main"), "{out}");
    assert!(
        out.contains("mcp: working tree (no git repository)"),
        "{out}"
    );
    assert!(
        out.contains("api/src/main/java/com/acme/api/ReportClient.java"),
        "{out}"
    );

    // The workspace's rules: lib may depend on nothing; the new edge is introduced.
    let (code, out, err) = cartograph(&["check", &root, "--data-dir", &d, "--base", "main"]);
    assert_eq!(code, 1, "{out}{err}");
    assert!(out.contains("layer_violation:lib->api"), "{out}");
    // A revision for all that no member has is a mistake, not a comparison with itself.
    let (code, _, err) = cartograph(&["check", &root, "--data-dir", &d, "--base", "mian"]);
    assert_eq!(code, 2, "{err}");
    assert!(
        err.contains("no member's repository has revision \"mian\""),
        "{err}"
    );
    // A revision named for one member must exist there; one for all may be missing.
    let (code, _, err) = cartograph(&["check", &root, "--data-dir", &d, "--base", "api=nope"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("member api: nope"), "{err}");
    let (code, out, err) = cartograph(&[
        "diff",
        "main,lib=HEAD",
        "-C",
        &root,
        "--data-dir",
        &d,
        "--json",
    ]);
    assert_eq!(code, 0, "{out}{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    let base = v["base"].as_str().unwrap_or_default();
    assert!(base.starts_with("lib: HEAD ("), "{v}");
    assert!(base.contains(", api: main ("), "{v}");
}
