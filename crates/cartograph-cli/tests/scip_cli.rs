//! N3 through the CLI: `index` picks up `index.scip`, `query` shows what is precise, and
//! `doctor` warns (without failing) when the index is older than files it covers.

#[path = "../../cartograph-index/tests/scip_fixture/mod.rs"]
mod scip_fixture;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dst = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dst);
        } else {
            fs::copy(entry.path(), &dst).unwrap();
            scip_fixture::touch(&dst, -600);
        }
    }
}

fn cartograph(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cartograph"))
        .args(args)
        .env_remove("RUST_LOG")
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

struct Repo {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
}

impl Repo {
    fn new() -> Repo {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/scip-ts"),
            &root,
        );
        scip_fixture::write(&root.join("index.scip"), None);
        scip_fixture::touch(&root.join("index.scip"), -300);
        let data = tmp.path().join("data");
        Repo {
            _tmp: tmp,
            root,
            data,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut v: Vec<&str> = args.to_vec();
        let (root, data) = (self.root.to_str().unwrap(), self.data.to_str().unwrap());
        v.extend(["--data-dir", data]);
        if args[0] == "query" {
            v.extend(["-C", root]);
        } else {
            v.push(root);
        }
        cartograph(&v)
    }
}

#[test]
fn index_reports_the_import_and_queries_show_it() {
    let r = Repo::new();
    let o = r.run(&["index", "--no-git"]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        out.contains("scip index.scip: 7 documents, 6 match files under web/, 1 match none (e.g. src/generated.ts)"),
        "{out}"
    );
    assert!(out.contains("scip: 6 files precise"), "{out}");
    // Warm: from the cache.
    let out = text(&r.run(&["index", "--no-git"]));
    assert!(out.contains("(cached)"), "{out}");

    let o = r.run(&["query", "symbols", "web/src/store.ts"]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(out.contains("· precise (scip)"), "{out}");
    assert!(out.contains("function fetch L16-18 ←1 (scip)"), "{out}");

    let out = text(&r.run(&["query", "callers", "Store.save"]));
    assert!(out.contains("(exact 2, scip 2)"), "{out}");
    assert!(out.contains(":7 run() [exact, scip]"), "{out}");

    let out = text(&r.run(&["query", "explain", "web/src/store.ts"]));
    assert!(out.contains("(scip)"), "{out}");
    assert!(out.contains("· precise (scip)"), "{out}");

    let out = text(&r.run(&["query", "overview"]));
    assert!(
        out.contains("precise calls (scip): 6 files · symbols 12 matched, 1 added"),
        "{out}"
    );
    // util.ts → app.ts is a dependency only SCIP shows; findings see it like an import.
    assert!(out.contains("cycle 1"), "{out}");
    let out = text(&r.run(&["query", "path", "web/src/util.ts", "web/src/app.ts"]));
    assert!(out.contains("shortest path 1 step"), "{out}");

    // JSON carries the markers too.
    let out = text(&r.run(&["query", "--json", "callers", "Store.save"]));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["scip"], 2);

    // Without SCIP, none of it shows.
    let out = text(&r.run(&["index", "--no-git", "--no-scip"]));
    assert!(!out.contains("scip"), "{out}");
    let out = text(&r.run(&["query", "overview"]));
    assert!(!out.contains("scip"), "{out}");
    assert!(!out.contains("cycle"), "{out}");
    let out = text(&r.run(&["query", "--json", "symbols", "web/src/store.ts"]));
    assert!(!out.contains("scip"), "{out}");
}

#[test]
fn doctor_warns_on_a_stale_index_without_failing() {
    let r = Repo::new();
    let o = r.run(&["doctor"]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        out.lines()
            .any(|l| l.starts_with("PASS") && l.contains("scip index") && l.contains("up to date")),
        "{out}"
    );
    scip_fixture::touch(&r.root.join("web/src/legacy.ts"), 0);
    let o = r.run(&["doctor"]);
    let out = text(&o);
    assert!(o.status.success(), "a stale index is a warning: {out}");
    assert!(
        out.lines().any(|l| l.starts_with("WARN")
            && l.contains("index.scip is stale")
            && l.contains("web/src/legacy.ts")),
        "{out}"
    );
    assert!(out.contains("all checks passed (1 warning)"), "{out}");
    // Unreadable: a warning as well.
    fs::write(r.root.join("index.scip"), b"not protobuf").unwrap();
    let o = r.run(&["doctor"]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(out.contains("index.scip is unreadable"), "{out}");
}

#[test]
fn a_missing_scip_flag_file_is_an_error() {
    let r = Repo::new();
    let o = r.run(&["index", "--no-git", "--scip", "nope.scip"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("no such file"));
}
