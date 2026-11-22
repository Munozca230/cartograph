//! `cartograph eval` itself: a passing case, each kind of failure (missing fact, forbidden
//! fact, order, budget, error code), the exit code, a git repo built from the cases file,
//! and nothing written into the fixture. Also: repos outside the cases file's folder (by
//! absolute and relative path), cases with several calls, `needs` tags and `--data-dir`.
//!
//! The real suite (`evals/cases.toml`) is not run here: it measures the tools, and a case
//! that fails because a tool is wrong must stay visible instead of breaking `cargo test`.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

const CASES: &str = r#"
[repos.app]
fixture = "fixtures/query-app"

[repos.git]
[[repos.git.commits]]
message = "one"
[repos.git.commits.files]
"src/a.ts" = 'import { b } from "./b";'
"src/b.ts" = 'export const b = 1;'
[repos.git.worktree]
"src/c.ts" = 'import { a } from "./a";'

[[case]]
id = "pass"
repo = "app"
question = "tests for model?"
tool = "impact"
args = { nodes = ["src/core/model.ts"] }
contains = ["src/core/service.test.ts", "tests/e2e.test.ts"]
not_contains = ["node_modules"]

[[case]]
id = "missing-and-forbidden"
repo = "app"
question = "q"
tool = "deps"
args = { node = "src/ui/app.ts" }
contains = ["no-such-fact"]
not_contains = ["src/infra/db.ts"]

[[case]]
id = "order"
repo = "app"
question = "q"
tool = "deps"
args = { node = "src/ui/app.ts" }
contains = ["src/infra/db.ts", "src/core/service.ts"]
in_order = true

[[case]]
id = "budget"
repo = "app"
question = "q"
tool = "overview"
budget = 5

[[case]]
id = "error-code"
repo = "app"
question = "q"
tool = "explain"
args = { node = "src/nope.ts" }
error = "unknown_node"

[[case]]
id = "unexpected-error"
repo = "app"
question = "q"
tool = "explain"
args = { node = "src/nope.ts" }

[[case]]
id = "git-diff"
repo = "git"
question = "q"
tool = "diff"
args = { base = "main" }
contains = ["added src/c.ts"]
"#;

#[test]
fn reports_each_failure_kind_and_exits_non_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = tmp.path().join("fixtures/query-app");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/query-app"),
        &fixture,
    );
    let cases = tmp.path().join("cases.toml");
    std::fs::write(&cases, CASES).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cartograph"))
        .arg("eval")
        .arg(&cases)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    let by_id = |id: &str| -> Value {
        report["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("no case {id}: {report}"))
    };
    let failures = |id: &str| -> String { by_id(id)["failures"].to_string() };
    assert_eq!(by_id("pass")["pass"], true, "{}", by_id("pass"));
    assert_eq!(by_id("error-code")["pass"], true, "{}", by_id("error-code"));
    assert_eq!(by_id("git-diff")["pass"], true, "{}", by_id("git-diff"));
    let f = failures("missing-and-forbidden");
    assert!(
        f.contains("missing") && f.contains("must not contain"),
        "{f}"
    );
    assert!(
        failures("order").contains("out of order"),
        "{}",
        failures("order")
    );
    assert!(
        failures("budget").contains("over budget"),
        "{}",
        failures("budget")
    );
    assert!(
        failures("unexpected-error").contains("unknown_node"),
        "{}",
        failures("unexpected-error")
    );
    assert_eq!(report["passed"], 3, "{report}");
    assert_eq!(report["failed"], 4, "{report}");
    // The fixture copy stays clean: the index went to a temp data dir.
    assert!(!fixture.join(".cartograph").exists());

    // Only the passing case: exit 0, text report.
    let out = Command::new(env!("CARGO_BIN_EXE_cartograph"))
        .arg("eval")
        .arg(&cases)
        .args(["--filter", "pass"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("PASS") && text.contains("1 passed, 0 failed"),
        "{text}"
    );
}

/// The repos of a suite live anywhere: one by absolute path, one relative to the cases file.
/// A case may make several calls, on several repos; `needs` tags are counted.
#[test]
fn serves_external_repos_read_only_with_several_calls() {
    let outside = tempfile::tempdir().unwrap();
    let ext = outside.path().join("ext");
    write(
        &ext.join("src/a.ts"),
        "import { b } from \"./b\";\nexport function a() { return b(); }\n",
    );
    write(&ext.join("src/b.ts"), "export function b() { return 1; }\n");
    let home = tempfile::tempdir().unwrap();
    let rel = home.path().join("rel");
    write(&rel.join("lib/c.ts"), "export function c() { return 2; }\n");
    let before = (listing(&ext), listing(&rel));
    let suite = home.path().join("suites");
    let cases = suite.join("cases.toml");
    let ext_abs = ext.to_str().unwrap();
    write(
        &cases,
        &format!(
            r#"
[repos.ext]
path = '{ext_abs}'

[repos.rel]
path = "../rel"

[[case]]
id = "one-call"
repo = "ext"
question = "who imports b?"
tool = "deps"
args = {{ node = "src/b.ts", dir = "in" }}
contains = ["src/a.ts"]

[[case]]
id = "two-repos"
repo = "ext"
question = "where are b and c?"
needs = ["workspace"]
contains = ["src/b.ts", "lib/c.ts"]
in_order = true

[[case.calls]]
tool = "explain"
args = {{ node = "src/b.ts" }}

[[case.calls]]
tool = "explain"
args = {{ node = "lib/c.ts" }}
repo = "rel"

[[case]]
id = "future-tool"
repo = "rel"
question = "a tool that does not exist yet"
needs = ["workspace", "bridge"]
tool = "no_such_tool"
"#
        ),
    );
    let data = home.path().join("data");
    let out = Command::new(env!("CARGO_BIN_EXE_cartograph"))
        .arg("eval")
        .arg(&cases)
        .arg("--json")
        .arg("--data-dir")
        .arg(&data)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    let case = |id: &str| -> Value {
        report["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("no case {id}: {report}"))
    };
    assert_eq!(case("one-call")["pass"], true, "{}", case("one-call"));
    assert_eq!(case("one-call")["calls"], 1);
    let two = case("two-repos");
    assert_eq!(two["pass"], true, "{two}");
    assert_eq!(two["calls"], 2);
    assert_eq!(two["tool"], "explain,explain");
    assert!(two["tokens"].as_u64().unwrap() > 0, "{two}");
    let future = case("future-tool");
    assert_eq!(future["pass"], false);
    assert!(
        future["failures"].to_string().contains("protocol error"),
        "{future}"
    );
    let need = |n: &str| -> (u64, u64) {
        let e = report["by_need"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["need"] == n)
            .unwrap_or_else(|| panic!("no need {n}: {report}"));
        (e["passed"].as_u64().unwrap(), e["total"].as_u64().unwrap())
    };
    assert_eq!(need("-"), (1, 1));
    assert_eq!(need("workspace"), (1, 2));
    assert_eq!(need("bridge"), (0, 1));
    // Read-only: no file appeared or changed in either repo, and the temp indexes under
    // --data-dir are gone.
    assert_eq!((listing(&ext), listing(&rel)), before);
    assert_eq!(std::fs::read_dir(&data).unwrap().count(), 0);

    // The text report shows the calls and the counts per tag.
    let out = Command::new(env!("CARGO_BIN_EXE_cartograph"))
        .arg("eval")
        .arg(&cases)
        .args(["--filter", "two-repos", "--data-dir"])
        .arg(&data)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("calls") && text.contains("workspace"),
        "{text}"
    );
}

fn write(p: &Path, content: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

/// Every file under `root` with its content.
fn listing(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut v = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            } else {
                v.push((
                    e.path().display().to_string(),
                    std::fs::read(e.path()).unwrap(),
                ));
            }
        }
    }
    v.sort();
    v
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
