//! N3 golden tests: a SCIP index (built in `scip_fixture`, written as protobuf) imported
//! over `tests/fixtures/scip-ts`. Regenerate with
//! `UPDATE_GOLDEN=1 cargo test -p cartograph-index --test scip_golden` and review the diff.

mod scip_fixture;

use cartograph_core::{Confidence, EdgeKind, Graph, Store, Symbols};
use cartograph_index::{IndexOptions, IndexOutput, index, index_to_store, scip};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), &dst).unwrap();
            scip_fixture::touch(&dst, -600);
        }
    }
}

/// A temp copy of the fixture with `index.scip` at its root, written after the sources;
/// `legacy.ts` is then edited (newer than the index).
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    copy_dir(&repo_root().join("tests/fixtures/scip-ts"), &root);
    scip_fixture::write(&root.join("index.scip"), None);
    scip_fixture::touch(&root.join("index.scip"), -300);
    scip_fixture::touch(&root.join("web/src/legacy.ts"), 0);
    let data = tmp.path().join("data");
    Fixture {
        _tmp: tmp,
        root,
        data,
    }
}

impl Fixture {
    fn opts(&self) -> IndexOptions {
        IndexOptions {
            data_dir: Some(self.data.clone()),
            ..IndexOptions::default()
        }
    }

    fn index(&self) -> IndexOutput {
        index_to_store(&self.root, &self.opts()).unwrap()
    }

    fn without_scip(&self) -> IndexOutput {
        index(
            &self.root,
            &IndexOptions {
                no_scip: true,
                ..IndexOptions::default()
            },
        )
        .unwrap()
    }
}

/// Edges SCIP added: in `with`, not in `without`.
fn new_edges(with: &Graph, without: &Graph) -> Vec<String> {
    let before: BTreeSet<(String, String, EdgeKind)> = without
        .edges
        .iter()
        .map(|e| (e.src.clone(), e.dst.clone(), e.kind))
        .collect();
    with.edges
        .iter()
        .filter(|e| !before.contains(&(e.src.clone(), e.dst.clone(), e.kind)))
        .map(|e| format!("{} -> {} [{} {}]", e.src, e.dst, e.kind, e.confidence))
        .collect()
}

fn render(s: &Symbols, with: &Graph, without: &Graph) -> Value {
    let added: BTreeSet<(String, u32)> = s
        .scip
        .added
        .iter()
        .map(|x| (x.id.clone(), x.line))
        .collect();
    let symbols: Vec<String> = s
        .symbols
        .iter()
        .map(|x| {
            format!(
                "{} {} L{}-{}{}{}",
                x.kind.as_str(),
                x.id(),
                x.line,
                x.end_line,
                if x.exported { " exported" } else { "" },
                if added.contains(&(x.id(), x.line)) {
                    " (scip)"
                } else {
                    ""
                }
            )
        })
        .collect();
    let calls: Vec<String> = s
        .calls
        .iter()
        .map(|c| {
            format!(
                "{}:{} {} -> {} [{}]",
                c.file,
                c.line,
                c.caller.as_deref().unwrap_or("(top level)"),
                c.target,
                c.confidence
            )
        })
        .collect();
    let stats: Vec<String> = s
        .stats
        .iter()
        .map(|c| {
            format!(
                "{}: exact {}, resolved {}, heuristic {}, ambiguous {}, unresolved {}",
                c.file, c.exact, c.resolved, c.heuristic, c.ambiguous, c.unresolved
            )
        })
        .collect();
    let mut scip = serde_json::to_value(s.scip.stats.as_ref().unwrap()).unwrap();
    for i in scip["indexes"].as_array_mut().unwrap() {
        i.as_object_mut().unwrap().remove("cached");
    }
    json!({
        "covered": s.scip.files,
        "symbols": symbols,
        "calls": calls,
        "stats": stats,
        "edges_added": new_edges(with, without),
        "scip": scip,
    })
}

fn golden(name: &str, v: &Value) {
    let actual = serde_json::to_string_pretty(v).unwrap() + "\n";
    let path = repo_root()
        .join("tests/golden")
        .join(format!("{name}.json"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden {}", path.display()))
        .replace("\r\n", "\n");
    assert_eq!(actual, expected, "golden {name} differs");
}

fn call<'a>(s: &'a Symbols, at: &str, target: &str) -> Option<&'a cartograph_core::SymbolCall> {
    let (file, line) = at.rsplit_once(':').unwrap();
    let line: u32 = line.parse().unwrap();
    s.calls
        .iter()
        .find(|c| c.file == file && c.line == line && c.target == target)
}

#[test]
fn scip_ts_golden() {
    let f = fixture();
    let out = f.index();
    let without = f.without_scip();
    golden("scip-ts", &render(&out.symbols, &out.graph, &without.graph));
}

#[test]
fn definitions_match_n2_symbols_and_fill_its_gaps() {
    let f = fixture();
    let s = f.index().symbols;
    let st = s.scip.stats.clone().unwrap();
    // Store, save, loadAll, makeStore, api (store.ts); format, save (util.ts); run, helper,
    // read (app.ts); show (view.ts).
    assert_eq!(st.symbols_matched, 11, "{st:?}");
    // `api.fetch` (a method of an object literal passed to a call) is N2's blind spot.
    assert_eq!(st.symbols_added, 1);
    let fetch = s
        .symbols
        .iter()
        .find(|x| x.id() == "sym:web/src/store.ts#fetch")
        .expect("fetch added from SCIP");
    assert_eq!((fetch.line, fetch.end_line), (16, 18));
    assert_eq!(s.scip.added.len(), 1);
    // A field or a variable is never added.
    assert!(!s.symbols.iter().any(|x| x.name == "items"));
}

#[test]
fn references_upgrade_calls_to_exact_and_drop_unconfirmed_ones() {
    let f = fixture();
    let without = f.without_scip().symbols;
    let s = f.index().symbols;
    // Heuristic in N2 (a unique method name), confirmed by SCIP: exact, and its caller is
    // now the object literal's method SCIP knows.
    let before = call(
        &without,
        "web/src/store.ts:17",
        "sym:web/src/store.ts#Store.loadAll",
    );
    assert_eq!(before.unwrap().confidence, Confidence::Heuristic);
    let after = call(
        &s,
        "web/src/store.ts:17",
        "sym:web/src/store.ts#Store.loadAll",
    )
    .unwrap();
    assert_eq!(after.confidence, Confidence::Exact);
    assert_eq!(after.caller.as_deref(), Some("sym:web/src/store.ts#fetch"));
    // Resolved through an import in N2: exact now.
    assert_eq!(
        call(&without, "web/src/app.ts:7", "sym:web/src/util.ts#format")
            .unwrap()
            .confidence,
        Confidence::Resolved
    );
    assert_eq!(
        call(&s, "web/src/app.ts:7", "sym:web/src/util.ts#format")
            .unwrap()
            .confidence,
        Confidence::Exact
    );
    // A call N2 could not resolve (a method on a typed local).
    assert!(
        call(
            &without,
            "web/src/app.ts:7",
            "sym:web/src/store.ts#Store.save"
        )
        .is_none()
    );
    assert!(call(&s, "web/src/app.ts:7", "sym:web/src/store.ts#Store.save").is_some());
    // N2's guess for `cfg.loadAll()` (a parameter's own type) is wrong; SCIP drops it.
    assert!(
        call(
            &without,
            "web/src/view.ts:5",
            "sym:web/src/store.ts#Store.loadAll"
        )
        .is_some()
    );
    assert!(
        call(
            &s,
            "web/src/view.ts:5",
            "sym:web/src/store.ts#Store.loadAll"
        )
        .is_none()
    );
    let st = s.scip.stats.unwrap();
    assert!(st.calls_upgraded >= 3, "{st:?}");
    assert_eq!(st.calls_dropped, 1, "{st:?}");
    // Every call of a covered file is exact: here they all call symbols the index defines.
    for c in s.calls.iter().filter(|c| s.scip.files.contains(&c.file)) {
        assert_eq!(c.confidence, Confidence::Exact, "{c:?}");
    }
}

#[test]
fn a_reference_is_a_call_only_where_the_text_calls_it() {
    let f = fixture();
    let without = f.without_scip().symbols;
    let s = f.index().symbols;
    let save = "sym:web/src/store.ts#Store.save";
    // `s.save(format(input))`: N2 has no call (a method on a typed local); SCIP's reference
    // is followed by its arguments, so it is one.
    assert!(call(&without, "web/src/app.ts:7", save).is_none());
    let added = call(&s, "web/src/app.ts:7", save).unwrap();
    assert_eq!(added.confidence, Confidence::Exact);
    assert_eq!(added.caller.as_deref(), Some("sym:web/src/app.ts#run"));
    // `debounce(run, 10)`: `run` is passed as a value, not called.
    let run = "sym:web/src/app.ts#run";
    assert!(call(&without, "web/src/app.ts:9", run).is_none());
    assert!(call(&s, "web/src/app.ts:9", run).is_none());
    // util.ts names `run` without calling it either.
    assert!(call(&s, "web/src/util.ts:6", run).is_none());
    // A value reference still adds the file dependency.
    let out = f.index();
    assert!(
        new_edges(&out.graph, &f.without_scip().graph)
            .contains(&"file:web/src/util.ts -> file:web/src/app.ts [uses exact]".into())
    );
}

#[test]
fn calls_to_files_the_index_does_not_define_keep_their_n2_confidence() {
    // A monorepo: the index covers `web/` only, and a covered file calls into `shared/`.
    let f = fixture();
    let shared = f.root.join("shared/text.ts");
    std::fs::create_dir_all(shared.parent().unwrap()).unwrap();
    std::fs::write(
        &shared,
        "export function shout(s: string): string {\n  return s.toUpperCase();\n}\n",
    )
    .unwrap();
    // Appended, so the index's lines for util.ts still hold.
    let util = f.root.join("web/src/util.ts");
    let mut text = std::fs::read_to_string(&util).unwrap();
    text.push_str(
        "\nimport { shout } from \"../../shared/text\";\n\n\
         export function yell(x: string): string {\n  return shout(x);\n}\n",
    );
    std::fs::write(&util, text).unwrap();
    scip_fixture::touch(&shared, -600);
    scip_fixture::touch(&util, -600);
    let target = "sym:shared/text.ts#shout";
    let without = f.without_scip().symbols;
    let n2 = call(&without, "web/src/util.ts:12", target).expect("N2 resolves the call");
    let s = f.index().symbols;
    assert!(s.scip.files.contains(&"web/src/util.ts".to_string()));
    let kept = call(&s, "web/src/util.ts:12", target).expect("kept with SCIP");
    assert_eq!(kept.confidence, n2.confidence);
    assert_ne!(kept.confidence, Confidence::Exact);
    let st = s
        .stats
        .iter()
        .find(|c| c.file == "web/src/util.ts")
        .unwrap();
    assert_eq!(st.resolved + st.heuristic, 1, "{st:?}");
}

#[test]
fn uncovered_and_stale_files_keep_their_n2_data() {
    let f = fixture();
    let without = f.without_scip().symbols;
    let s = f.index().symbols;
    let of = |s: &Symbols, file: &str| -> Vec<cartograph_core::SymbolCall> {
        s.calls.iter().filter(|c| c.file == file).cloned().collect()
    };
    // Not in the index at all.
    assert!(!of(&without, "tools/job.py").is_empty());
    assert_eq!(of(&s, "tools/job.py"), of(&without, "tools/job.py"));
    // In the index, but edited after it was written: its lines may have moved.
    assert_eq!(
        of(&s, "web/src/legacy.ts"),
        of(&without, "web/src/legacy.ts")
    );
    assert!(!s.scip.files.contains(&"web/src/legacy.ts".to_string()));
    assert!(!s.scip.files.contains(&"tools/job.py".to_string()));
    let ix = &s.scip.stats.unwrap().indexes[0];
    assert_eq!(ix.stale, 1);
    // Its symbols are kept as N2 found them.
    assert!(
        s.symbols
            .iter()
            .any(|x| x.id() == "sym:web/src/legacy.ts#old")
    );
}

#[test]
fn a_project_root_elsewhere_maps_under_its_folder() {
    let f = fixture();
    let s = f.index().symbols;
    let ix = &s.scip.stats.as_ref().unwrap().indexes[0];
    assert_eq!(ix.prefix, "web");
    assert_eq!((ix.documents, ix.matched, ix.unmatched), (7, 6, 1));
    assert_eq!(ix.unmatched_sample, ["src/generated.ts"]);
    assert_eq!(ix.tool, "scip-typescript 0.3.14");
    // The same index given with `--scip` from inside `web/`, its project root a real local
    // folder: mapped through that folder.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    copy_dir(&repo_root().join("tests/fixtures/scip-ts"), &root);
    let web = std::fs::canonicalize(root.join("web")).unwrap();
    let uri = format!(
        "file:///{}",
        web.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .replace('\\', "/")
            .trim_start_matches('/')
    );
    let other = tmp.path().join("elsewhere.scip");
    scip_fixture::write(&other, Some(&uri));
    scip_fixture::touch(&other, -300);
    let out = index(
        &root,
        &IndexOptions {
            scip: vec![other],
            ..IndexOptions::default()
        },
    )
    .unwrap();
    let ix = &out.symbols.scip.stats.unwrap().indexes[0];
    assert_eq!((ix.prefix.as_str(), ix.matched), ("web", 6));
}

#[test]
fn cross_file_references_add_missing_dependencies_and_libraries() {
    let f = fixture();
    let out = f.index();
    let without = f.without_scip();
    let added = new_edges(&out.graph, &without.graph);
    // app.ts imports the barrel (index.ts); what it uses is defined in store.ts.
    assert!(added.contains(&"file:web/src/app.ts -> file:web/src/store.ts [uses exact]".into()));
    // util.ts uses app.ts with no import (a cycle only SCIP sees).
    assert!(added.contains(&"file:web/src/util.ts -> file:web/src/app.ts [uses exact]".into()));
    // A lodash symbol from a file that does not import lodash.
    assert!(
        added.contains(
            &"file:web/src/util.ts -> external:npm:lodash [import_external exact]".into()
        )
    );
    // Existing imports are not counted twice: app.ts imports util.ts and lodash.
    assert!(
        !added
            .iter()
            .any(|e| e.starts_with("file:web/src/app.ts -> file:web/src/util.ts"))
    );
    assert!(
        !added
            .iter()
            .any(|e| e.starts_with("file:web/src/app.ts -> external:"))
    );
    // Nor are files SCIP does not cover (legacy.ts is stale).
    assert!(
        !added
            .iter()
            .any(|e| e.starts_with("file:web/src/legacy.ts"))
    );
    let st = out.symbols.scip.stats.unwrap();
    assert_eq!((st.deps_added, st.externals_added), (2, 1));
    // @types/lodash → npm:lodash; typescript's lib has no library node.
    assert_eq!((st.packages_matched, st.packages_unmatched), (1, 1));
    // Derived folder edges count imports only: unchanged.
    let derived = |g: &Graph| -> Vec<String> {
        g.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::DependsOn)
            .map(|e| format!("{} {} {}", e.src, e.dst, e.weight))
            .collect()
    };
    assert_eq!(derived(&out.graph), derived(&without.graph));
}

#[test]
fn an_unchanged_index_is_read_from_the_cache() {
    let f = fixture();
    let first = f.index();
    let st = first.symbols.scip.stats.as_ref().unwrap();
    assert!(!st.indexes[0].cached);
    let second = f.index();
    assert!(second.symbols.scip.stats.as_ref().unwrap().indexes[0].cached);
    let strip = |s: &Symbols| {
        let mut s = s.clone();
        s.scip.stats = None;
        s
    };
    assert_eq!(strip(&first.symbols), strip(&second.symbols));
    assert_eq!(first.graph, second.graph);
    // Touched but unchanged: the hash says it is the same file.
    scip_fixture::touch(&f.root.join("index.scip"), -200);
    assert!(f.index().symbols.scip.stats.unwrap().indexes[0].cached);
    // Rewritten: decoded again.
    scip_fixture::write(&f.root.join("index.scip"), Some("file:///elsewhere/web"));
    scip_fixture::touch(&f.root.join("index.scip"), -100);
    let third = f.index();
    assert!(!third.symbols.scip.stats.as_ref().unwrap().indexes[0].cached);
    assert_eq!(strip(&first.symbols), strip(&third.symbols));
    // A damaged cache row (bad JSON, or indexes out of range) costs a decode, nothing else.
    for data in [
        "{not json",
        r#"{"s":[],"d":[{"p":"src/app.ts","r":[[7,1,2]]}]}"#,
    ] {
        Store::open(f.data.join("graph.db"))
            .unwrap()
            .connection()
            .execute("UPDATE scip_cache SET data = ?1", [data])
            .unwrap();
        let again = f.index();
        assert!(!again.symbols.scip.stats.as_ref().unwrap().indexes[0].cached);
        assert_eq!(strip(&first.symbols), strip(&again.symbols));
    }
    let third = f.index();
    assert!(third.symbols.scip.stats.as_ref().unwrap().indexes[0].cached);
    // What the store holds is what the run returned.
    let stored = Store::open(f.data.join("graph.db"))
        .unwrap()
        .load_symbols()
        .unwrap();
    assert_eq!(stored, third.symbols);
}

#[test]
fn explicit_indexes_are_remembered_until_no_scip() {
    let f = fixture();
    std::fs::remove_file(f.root.join("index.scip")).unwrap();
    let elsewhere = f.root.parent().unwrap().join("ci.scip");
    scip_fixture::write(&elsewhere, None);
    scip_fixture::touch(&elsewhere, -300);
    let out = index_to_store(
        &f.root,
        &IndexOptions {
            scip: vec![elsewhere.clone()],
            ..f.opts()
        },
    )
    .unwrap();
    assert!(!out.symbols.scip.files.is_empty());
    // Later re-indexes without the flag (`cartograph index`, `serve`, `mcp`) keep using it.
    assert!(!f.index().symbols.scip.files.is_empty());
    let off = index_to_store(
        &f.root,
        &IndexOptions {
            no_scip: true,
            ..f.opts()
        },
    )
    .unwrap();
    assert!(off.symbols.scip.is_empty());
    assert!(
        f.index().symbols.scip.is_empty(),
        "forgotten after --no-scip"
    );
}

#[test]
fn a_corrupt_or_foreign_file_is_an_error_message() {
    let f = fixture();
    let bad = f.root.parent().unwrap().join("bad.scip");
    for bytes in [
        b"this is not protobuf at all".to_vec(),
        vec![0x12, 0xff, 0xff, 0xff, 0xff, 0x0f, 0x01],
        Vec::new(),
    ] {
        std::fs::write(&bad, &bytes).unwrap();
        let out = index(
            &f.root,
            &IndexOptions {
                scip: vec![bad.clone()],
                no_scip: false,
                ..IndexOptions::default()
            },
        )
        .unwrap();
        let st = out.symbols.scip.stats.unwrap();
        let bad_ix = st
            .indexes
            .iter()
            .find(|i| i.path.ends_with("bad.scip"))
            .unwrap();
        assert!(bad_ix.error.is_some(), "{bytes:?}");
        // The good index at the root still applies.
        assert!(st.files_covered > 0);
    }
    // A missing file too.
    let out = index(
        &f.root,
        &IndexOptions {
            scip: vec![f.root.join("missing.scip")],
            ..IndexOptions::default()
        },
    )
    .unwrap();
    assert!(
        out.symbols.scip.stats.unwrap().indexes[0]
            .error
            .as_deref()
            .unwrap()
            .starts_with("cannot read")
    );
}

#[test]
fn health_reports_a_stale_index() {
    let f = fixture();
    let h = scip::health(&f.root, &f.data);
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].label, "index.scip");
    let ok = h[0].outcome.as_ref().unwrap();
    assert_eq!((ok.documents, ok.matched), (7, 6));
    assert_eq!(ok.stale, ["web/src/legacy.ts"]);
    // Regenerated after the edit: fresh.
    scip_fixture::touch(&f.root.join("index.scip"), 60);
    let h = scip::health(&f.root, &f.data);
    assert!(h[0].outcome.as_ref().unwrap().stale.is_empty());
    // None at all: nothing to report.
    std::fs::remove_file(f.root.join("index.scip")).unwrap();
    assert!(scip::health(&f.root, &f.data).is_empty());
}

#[test]
fn diff_inputs_never_see_scip() {
    // The working tree of `diff` / `check --base` is built like a base snapshot: no SCIP.
    let f = fixture();
    let (wt, _) = cartograph_index::index_working_tree(&f.root, &f.opts()).unwrap();
    assert!(!wt.graph.edges.iter().any(|e| e.kind == EdgeKind::Uses));
    assert!(wt.symbols.scip.is_empty());
}
