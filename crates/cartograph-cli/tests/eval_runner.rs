//! `cartograph eval` itself: a passing case, each kind of failure (missing fact, forbidden
//! fact, order, budget, error code), the exit code, a git repo built from the cases file,
//! and nothing written into the fixture.
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
