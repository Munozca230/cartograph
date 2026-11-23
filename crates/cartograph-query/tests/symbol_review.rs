//! Regression tests for the code-review findings on the symbol layer (N2): symbol ids,
//! the symbol resolver, flow entries in `overview` and `find` by kind.

mod common;

use cartograph_index::{IndexOptions, index};
use cartograph_query::find::{self, FindKind};
use cartograph_query::{Config, QueryError, Repo, overview, symbols};
use std::fs;
use std::path::Path;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// Index `files` from a temp dir, symbols included.
fn repo(files: &[(&str, &str)]) -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    for (rel, text) in files {
        write(&root, rel, text);
    }
    let out = index(&root, &IndexOptions::default()).unwrap();
    Repo::with_symbols(&out.graph, &out.symbols, Config::default())
}

/// A repo indexed with the symbol layer but without symbol-bearing files is indexed: it
/// has no symbols, not an index to re-run. Both in memory and through the store.
#[test]
fn a_repo_without_symbols_is_still_indexed() {
    let files = [("README.md", "# r\n"), ("data/config.json", "{\"a\": 1}\n")];
    let in_memory = repo(&files);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    for (rel, text) in files {
        write(&root, rel, text);
    }
    let opts = IndexOptions {
        data_dir: Some(tmp.path().join("db")),
        ..IndexOptions::default()
    };
    cartograph_index::index_to_store(&root, &opts).unwrap();
    let store = cartograph_core::Store::open(tmp.path().join("db/graph.db")).unwrap();
    let (g, s) = store.load_graph_and_symbols().unwrap();
    let stored = Repo::with_symbols(&g, &s, Config::default());
    for r in [&in_memory, &stored] {
        assert!(r.symbols.symbols.is_empty());
        assert!(r.symbols.indexed);
        assert!(matches!(
            symbols::callers(r, "x"),
            Err(QueryError::UnknownNode { .. })
        ));
        assert_eq!(find::find_symbols(r, "x").unwrap().data.hits.len(), 0);
    }
}

/// JS/TS private names hold a `#`: their ids resolve whole, as `path#name` and as
/// `Class.#name`, and `callers` finds their call sites.
#[test]
fn private_names_resolve_and_have_callers() {
    let r = repo(&[(
        "src/a.ts",
        "export class Model {\n  #p() { return 1; }\n  run() { return this.#p(); }\n}\n",
    )]);
    let t = &r.symbols;
    let id = "sym:src/a.ts#Model.#p";
    assert!(
        t.symbols.iter().any(|s| s.id() == id),
        "{:?}",
        t.symbols.iter().map(|s| s.id()).collect::<Vec<_>>()
    );
    for input in [id, "src/a.ts#Model.#p", "src/a.ts##p", "Model.#p", "#p"] {
        assert_eq!(t.resolve(input).unwrap(), id, "{input}");
    }
    let c = symbols::callers(&r, "Model.#p").unwrap();
    assert_eq!(c.data.symbol, id);
    assert_eq!(c.data.total, 1, "{}", c.text);
    assert!(c.text.contains("Model.#p"), "{}", c.text);
    // An unknown private name is suggested from its whole name.
    match t.resolve("Model.#q") {
        Err(QueryError::UnknownNode { suggestions, .. }) => {
            assert!(suggestions.iter().any(|s| s == id), "{suggestions:?}");
        }
        other => panic!("{other:?}"),
    }
}

/// A `#` in a folder name is part of the path.
#[test]
fn a_hash_in_the_path_is_not_the_separator() {
    let r = repo(&[("src/c#/a.ts", "export function run() { return 1; }\n")]);
    let t = &r.symbols;
    assert_eq!(t.resolve("src/c#/a.ts#run").unwrap(), "sym:src/c#/a.ts#run");
    assert_eq!(t.resolve("run").unwrap(), "sym:src/c#/a.ts#run");
    match t.resolve("src/c#/a.ts#nope") {
        Err(QueryError::UnknownNode { .. }) => {}
        other => panic!("{other:?}"),
    }
}

/// The repo overview's flow entries leave out tests, fixtures and examples; from inside an
/// example, that example's entries count.
#[test]
fn overview_flows_skip_test_fixture_and_example_trees() {
    let main = "def run():\n    return 1\n\nif __name__ == \"__main__\":\n    run()\n";
    let r = repo(&[
        ("app/cli.py", main),
        ("examples/demo/cli.py", main),
        ("tests/fixtures/f/cli.py", main),
        ("tests/test_cli.py", main),
    ]);
    let all: Vec<&str> = r.symbols.entries.iter().map(|e| e.file.as_str()).collect();
    assert_eq!(all.len(), 4, "{all:?}");
    let o = overview::overview(&r, None, 2).unwrap();
    let f = o.data.flows.as_ref().unwrap();
    assert_eq!(f.total, 1, "{}", o.text);
    assert!(f.first[0].at.starts_with("app/cli.py:"), "{}", o.text);
    let o = overview::overview(&r, Some("examples/demo"), 2).unwrap();
    let f = o.data.flows.as_ref().unwrap();
    assert_eq!(f.total, 1, "{}", o.text);
    assert!(
        f.first[0].at.starts_with("examples/demo/cli.py:"),
        "{}",
        o.text
    );
}

/// `path#name` that fits several symbols of the file lists them.
#[test]
fn path_and_name_matching_several_symbols_is_ambiguous() {
    let r = repo(&[(
        "src/a.ts",
        "export class A { run() { return 1; } }\nexport class B { run() { return 2; } }\n",
    )]);
    match r.symbols.resolve("src/a.ts#run") {
        Err(QueryError::Ambiguous {
            count, candidates, ..
        }) => {
            assert_eq!(count, 2);
            assert_eq!(candidates, vec!["sym:src/a.ts#A.run", "sym:src/a.ts#B.run"]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        r.symbols.resolve("src/a.ts#B.run").unwrap(),
        "sym:src/a.ts#B.run"
    );
}

/// `find` with kind `sym` searches symbols, as `find_symbols` does; without a kind it
/// searches paths and packages, and symbols only when no path has the query.
#[test]
fn find_with_kind_sym_searches_symbols() {
    let r = repo(&[(
        "src/a.ts",
        "export function compactSegments() { return 1; }\n",
    )]);
    let hits = |kind| -> Vec<String> {
        find::find(&r, "compactSegments", kind)
            .unwrap()
            .data
            .hits
            .into_iter()
            .map(|h| h.id)
            .collect()
    };
    assert_eq!(
        hits(Some(FindKind::Sym)),
        vec!["sym:src/a.ts#compactSegments"]
    );
    // No path holds it: the symbols that do, said so.
    let none = find::find(&r, "compactSegments", None).unwrap();
    assert_eq!(
        none.data
            .hits
            .iter()
            .map(|h| h.id.as_str())
            .collect::<Vec<_>>(),
        vec!["sym:src/a.ts#compactSegments"]
    );
    assert!(
        none.text.starts_with("no file, folder or library has it"),
        "{}",
        none.text
    );
    // A path that holds it: paths only, no note.
    let path = find::find(&r, "src/a", None).unwrap();
    assert!(path.data.hits.iter().all(|h| !h.id.starts_with("sym:")));
    assert!(path.data.note.is_none());
    let direct = find::find_symbols(&r, "compactSegments").unwrap();
    let via = find::find(&r, "compactSegments", Some(FindKind::Sym)).unwrap();
    assert_eq!(via.text, direct.text);
    // Without a symbol layer, the same error as `find_symbols`.
    let bare = Repo::new(&cartograph_core::Graph::default(), Config::default());
    assert!(matches!(
        find::find(&bare, "x", Some(FindKind::Sym)),
        Err(QueryError::BadArgument(_))
    ));
}

/// `callers` lists production callers first, even when tests call the symbol more, and
/// sums up the test callers without their sites. A symbol called only from tests keeps
/// its test call sites.
#[test]
fn callers_put_production_before_tests() {
    let r = repo(&[
        ("package.json", "{\"name\": \"r\"}\n"),
        (
            "src/core.ts",
            "export function work() { return 1; }\nexport function onlyTested() { return 2; }\n",
        ),
        ("src/app.ts", "import { work } from './core';\nwork();\n"),
        (
            "tests/core.test.ts",
            "import { work, onlyTested } from '../src/core';\nwork(); work(); work();\nonlyTested();\n",
        ),
        (
            "src/core.spec.ts",
            "import { work } from './core';\nwork(); work();\n",
        ),
    ]);
    let c = symbols::callers(&r, "work").unwrap();
    let files: Vec<(&str, bool)> = c
        .data
        .groups
        .iter()
        .map(|g| (g.file.as_str(), g.test))
        .collect();
    assert_eq!(
        files,
        vec![
            ("file:src/app.ts", false),
            ("file:tests/core.test.ts", true),
            ("file:src/core.spec.ts", true),
        ],
        "{}",
        c.text
    );
    assert_eq!((c.data.test_total, c.data.test_files), (5, 2));
    assert!(c.data.groups[1].sites.is_empty(), "{}", c.text);
    assert!(
        c.text
            .contains("in tests: 5 call sites in 2 files\n  tests/core.test.ts (3)\n"),
        "{}",
        c.text
    );

    let only = symbols::callers(&r, "onlyTested").unwrap();
    assert_eq!(only.data.groups.len(), 1, "{}", only.text);
    assert_eq!(only.data.groups[0].sites.len(), 1, "{}", only.text);
    assert!(only.text.contains("  :3 "), "{}", only.text);
}

/// `(file, line, confidence)` of the call sites of `sym`.
fn sites_of(r: &Repo, sym: &str) -> Vec<(String, u32, String)> {
    let c = symbols::callers(r, sym).unwrap();
    c.data
        .groups
        .iter()
        .flat_map(|g| {
            g.sites
                .iter()
                .map(|s| (g.file.clone(), s.line, s.confidence.to_string()))
        })
        .collect()
}

/// `self.field.m()` resolves through the field's declared type: `resolved` when the type
/// comes from an import or the file itself. A field whose type is not one named type
/// (`Option<T>`), an undeclared field, or another object's field stay unresolved, even
/// though another type of the repo has a method of that name.
#[test]
fn calls_through_a_typed_field_resolve() {
    let rust = repo(&[
        (
            "Cargo.toml",
            "[package]\nname = \"r\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", "mod guard;\nmod other;\nmod tools;\n"),
        (
            "src/guard.rs",
            "pub struct Guard;\nimpl Guard {\n    pub fn run(&self) {}\n}\n",
        ),
        (
            "src/other.rs",
            "pub struct Other;\nimpl Other {\n    pub fn run(&self) {}\n}\n",
        ),
        (
            "src/tools.rs",
            "use crate::guard::Guard;\nuse std::sync::Arc;\n\
             pub struct Tools {\n    guard: Arc<Guard>,\n    opt: Option<Guard>,\n}\n\
             impl Tools {\n    fn go(&self, x: &Tools) {\n        self.guard.run();\n        \
             self.opt.run();\n        self.missing.run();\n        x.guard.run();\n    }\n}\n",
        ),
    ]);
    assert_eq!(
        sites_of(&rust, "Guard.run"),
        vec![("file:src/tools.rs".to_string(), 9, "resolved".to_string())]
    );
    assert!(sites_of(&rust, "Other.run").is_empty());

    let ts = repo(&[
        (
            "src/guard.ts",
            "export class Guard {\n  run() { return 1; }\n}\n",
        ),
        (
            "src/other.ts",
            "export class Other {\n  run() { return 2; }\n}\n",
        ),
        (
            "src/tools.ts",
            "import { Guard } from './guard';\nexport class Tools {\n  private guard: Guard;\n  \
             constructor(private readonly backup: Guard) { this.guard = backup; }\n  \
             go() {\n    this.guard.run();\n    this.backup.run();\n  }\n}\n",
        ),
    ]);
    assert_eq!(
        sites_of(&ts, "Guard.run"),
        vec![
            ("file:src/tools.ts".to_string(), 6, "resolved".to_string()),
            ("file:src/tools.ts".to_string(), 7, "resolved".to_string()),
        ]
    );
    assert!(sites_of(&ts, "Other.run").is_empty());
}

/// `entries` leaves out what `overview` leaves out: entries in test, fixture or example
/// trees below the queried folder. Asked within such a tree, or with `include_tests`, it
/// lists them.
#[test]
fn entries_skip_test_fixture_and_example_trees() {
    let r = repo(&[
        ("src/main.rs", "fn main() {}\n"),
        ("tests/fixtures/app/src/main.rs", "fn main() {}\n"),
        ("examples/demo/main.rs", "fn main() {}\n"),
    ]);
    let files = |a: &cartograph_query::budget::Answer<symbols::EntriesResult>| -> Vec<String> {
        a.data
            .items
            .iter()
            .map(|e| {
                e.at.rsplit_once(':')
                    .map_or(e.at.clone(), |(p, _)| p.to_string())
            })
            .collect()
    };
    let main = Some(cartograph_core::EntryKind::Main);
    let all = symbols::entries(&r, main, None, false).unwrap();
    assert_eq!(files(&all), vec!["src/main.rs"], "{}", all.text);
    assert_eq!(all.data.in_tests, 2);
    assert!(
        all.text
            .contains("2 more in test, fixture or example trees (include_tests"),
        "{}",
        all.text
    );
    let inside = symbols::entries(&r, main, Some("tests/fixtures/app"), false).unwrap();
    assert_eq!(files(&inside), vec!["tests/fixtures/app/src/main.rs"]);
    assert_eq!(inside.data.in_tests, 0);
    let with = symbols::entries(&r, main, None, true).unwrap();
    assert_eq!(with.data.total, 3, "{}", with.text);
}

/// `callers` of a name no symbol has: the one symbol whose name starts with it (or, failing
/// that, contains it) is answered for, saying so; several are ambiguous; a short or fuzzier
/// name stays unknown, with suggestions.
#[test]
fn callers_complete_a_partial_name() {
    let r = repo(&[
        (
            "src/fmt.ts",
            "export function formatActionCommand(a: string) { return a; }\n\
             export function renderRow(a: string) { return a; }\n\
             export function renderRowCompact(a: string) { return a; }\n",
        ),
        (
            "src/app.ts",
            "import { formatActionCommand, renderRow } from './fmt';\n\
             export function run() { formatActionCommand('x'); renderRow('y'); }\n",
        ),
    ]);
    let c = symbols::callers(&r, "formatAction").unwrap();
    assert_eq!(c.data.symbol, "sym:src/fmt.ts#formatActionCommand");
    assert_eq!(c.data.instead_of.as_deref(), Some("formatAction"));
    assert!(
        c.text
            .starts_with("no `formatAction`; showing `formatActionCommand`"),
        "{}",
        c.text
    );
    assert!(c.text.contains("src/app.ts"), "{}", c.text);
    // Inside the name, in one file.
    let c = symbols::callers(&r, "src/fmt.ts#ActionCommand").unwrap();
    assert_eq!(c.data.symbol, "sym:src/fmt.ts#formatActionCommand");
    // An exact name answers as before, without the note.
    let c = symbols::callers(&r, "renderRow").unwrap();
    assert!(c.data.instead_of.is_none());
    // Several symbols start with it: ambiguous, with the candidates.
    match symbols::callers(&r, "render") {
        Err(QueryError::Ambiguous { candidates, .. }) => {
            assert!(candidates.contains(&"sym:src/fmt.ts#renderRow".to_string()));
            assert!(candidates.contains(&"sym:src/fmt.ts#renderRowCompact".to_string()));
        }
        other => panic!("{other:?}"),
    }
    // Too short, or only a typo away: unknown.
    assert!(matches!(
        symbols::callers(&r, "fo"),
        Err(QueryError::UnknownNode { .. })
    ));
    assert!(matches!(
        symbols::callers(&r, "formatActonCommand"),
        Err(QueryError::UnknownNode { .. })
    ));
}

/// `find` of a name no path holds and no symbol has: the letter-by-letter matches come with
/// a note saying they are not substring matches. Symbols match with case and separators
/// ignored (`READ-FILE` finds `ActionRunner.runReadFile`). A string key gets its trace
/// instead (`tests/keys.rs`).
#[test]
fn find_says_when_hits_are_only_approximate() {
    let r = repo(&[
        (
            "src/actions.ts",
            "export class ActionRunner {\n  runReadFile() {}\n}\n",
        ),
        ("scripts/migrate-legacy-data.js", "const a = 1;\n"),
    ]);
    let a = find::find(&r, "READ-FILE", None).unwrap();
    assert!(
        a.text.starts_with("no file, folder or library has it"),
        "{}",
        a.text
    );
    assert!(a.text.contains("ActionRunner.runReadFile"), "{}", a.text);
    let a = find::find(&r, "gtlgcy", None).unwrap();
    assert!(
        a.text.starts_with("no exact or substring match; closest"),
        "{}",
        a.text
    );
    assert!(a.text.contains("migrate-legacy-data.js"), "{}", a.text);
    let a = find::find(&r, "migrate", None).unwrap();
    assert!(a.data.note.is_none(), "{}", a.text);
}

/// `hierarchy` lists as many subtypes as its text budget holds: 21 subclasses with long
/// paths all fit (the JSON, with every id and path in full, no longer decides).
#[test]
fn hierarchy_lists_what_the_budget_holds() {
    let dir = "src/main/platform/execution/steps/implementations";
    let mut files = vec![(
        format!("{dir}/StepBase.ts"),
        "export abstract class StepBase {\n    run(): void {}\n}\n".to_string(),
    )];
    for i in 0..21 {
        files.push((
            format!("{dir}/ConcreteStepNumber{i:02}.ts"),
            format!(
                "import {{ StepBase }} from './StepBase';\nexport class ConcreteStepNumber{i:02} extends StepBase {{}}\n"
            ),
        ));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let r = repo(&refs);
    let h = cartograph_query::hierarchy::hierarchy(&r, "StepBase").unwrap();
    assert_eq!(h.data.subtypes, 21);
    assert_eq!(h.data.subs_more, 0, "{}", h.text);
    assert!(
        cartograph_query::budget::tokens(&h.text) <= cartograph_query::budget::limits::HIERARCHY
    );
}
