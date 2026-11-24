//! String keys traced by `find` (`tests/fixtures/workspace-keys`): definitions, what runs
//! a key (with the code it runs), catalogs, where it is missing (and never where a
//! container is too small, is a test, or does not hold its siblings), and other uses.

mod common;

use cartograph_query::Repo;
use cartograph_query::find::{FindKind, find};

fn repo() -> Repo {
    common::fixture("workspace-keys")
}

fn section<'t>(text: &'t str, title: &str) -> Vec<&'t str> {
    text.lines()
        .skip_while(|l| !l.starts_with(title))
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .map(str::trim)
        .collect()
}

#[test]
fn a_key_missing_from_the_registry_that_holds_its_siblings() {
    let r = repo();
    let a = find(&r, "ZIP-DIR", None).unwrap();
    let t = &a.text;
    assert!(
        t.starts_with("key \"ZIP-DIR\" in 2 members (core, ui):\n"),
        "{t}"
    );
    assert_eq!(
        section(t, "defined"),
        ["core/src/main/java/com/acme/core/Action.java:15 enum Action.ZIP_DIR"],
        "{t}"
    );
    assert_eq!(
        section(t, "lists it"),
        [
            "ui/src/actions/catalog.ts:25 ACTION_CATEGORIES[].actions[].name",
            "ui/src/actions/icons.ts:13 ACTION_ICONS",
        ],
        "{t}"
    );
    assert_eq!(
        section(t, "missing from"),
        ["FileActionBindings: new ActionBinding \
             (server/app/src/main/java/com/acme/server/actions/FileActionBindings.java:8): \
             holds 11 of the 11 other keys of Action \
             (core/src/main/java/com/acme/core/Action.java)"],
        "{t}"
    );
    let tr = a.data.trace.as_ref().unwrap();
    assert_eq!(tr.missing.len(), 1);
    assert!(a.data.hits.is_empty());
}

/// A comment that names the key: the trace lists its file, and a container of that file is
/// not said to miss the key (it may be left out there on purpose, and said so).
#[test]
fn a_comment_naming_the_key_is_listed_and_no_claim_in_its_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("workspace-keys");
    common::copy_dir(
        &common::repo_root().join("tests/fixtures/workspace-keys"),
        &root,
    );
    let rel = "server/app/src/main/java/com/acme/server/actions/FileActionBindings.java";
    let p = root.join(rel);
    let text = std::fs::read_to_string(&p).unwrap().replace(
        "(zipping a folder is not supported yet)",
        "(ZIP-DIR is not supported yet)",
    );
    std::fs::write(&p, text).unwrap();
    let out = cartograph_index::index(&root, &cartograph_index::IndexOptions::default()).unwrap();
    assert!(
        out.symbols
            .mentions
            .iter()
            .any(|m| m.key == "ZIP-DIR" && m.file == rel),
        "{:?}",
        out.symbols.mentions
    );
    assert!(out.symbols.keys.iter().all(|k| k.form != "comment"));
    let config = cartograph_query::Config::load(&root).unwrap();
    let r = Repo::with_symbols(&out.graph, &out.symbols, config);
    let a = find(&r, "ZIP-DIR", None).unwrap();
    let t = &a.text;
    assert!(!t.contains("missing from"), "{t}");
    assert!(
        t.contains(&format!("named in comments of 1 file: {rel}\n")),
        "{t}"
    );
    assert_eq!(a.data.trace.as_ref().unwrap().comments, [rel]);
}

#[test]
fn what_runs_a_key_names_the_code_it_runs() {
    let r = repo();
    // By the constant's name: the key it defines.
    let a = find(&r, "READ_FILE", None).unwrap();
    let t = &a.text;
    assert!(t.starts_with("key \"READ-FILE\""), "{t}");
    assert_eq!(
        section(t, "runs it"),
        [
            "server/app/src/main/java/com/acme/server/actions/FileActionBindings.java:12 \
             registered in FileActionBindings: new ActionBinding → \
             server/app/src/main/java/com/acme/server/actions/FileOps.java (FileOps.read)"
        ],
        "{t}"
    );
    // In every container: missing from none. The test's own list never counts.
    assert!(!t.contains("missing from"), "{t}");
    assert!(
        t.contains("ui/src/actions/catalog.test.ts:9 keys[] (test)\n"),
        "{t}"
    );
    // Uses: code first, tests last.
    let uses = section(t, "used");
    assert_eq!(
        uses.first(),
        Some(
            &"server/app/src/main/java/com/acme/server/files/FileServiceImpl.java:10 in FileServiceImpl.read"
        ),
        "{t}"
    );
    assert!(uses.last().unwrap().ends_with("(test)"), "{t}");

    // A switch case over string constants: a stacked case shares the next one's body.
    let a = find(&r, "EXPORT", None).unwrap();
    let t = &a.text;
    assert_eq!(
        section(t, "runs it"),
        [
            "core/src/main/java/com/acme/core/Runner.java:20 case in Runner.run → \
             core/src/main/java/com/acme/core/Documents.java (Documents.share)"
        ],
        "{t}"
    );
    assert_eq!(
        section(t, "missing from"),
        [
            "STEP_TO_KEY (agent/src/steps.ts:3): holds 10 of the 10 other keys of \
             switch in Runner.run (core/src/main/java/com/acme/core/Runner.java)"
        ],
        "{t}"
    );
}

#[test]
fn a_catalog_missing_a_key_and_no_claim_from_small_or_unrelated_containers() {
    let r = repo();
    let t = find(&r, "MOVE-FILE", None).unwrap().text;
    assert_eq!(
        section(&t, "missing from"),
        [
            "ACTION_CATEGORIES[].actions[].name (ui/src/actions/catalog.ts:7): holds 10 of \
             the 10 other keys of FileActionBindings: new ActionBinding \
             (server/app/src/main/java/com/acme/server/actions/FileActionBindings.java)"
        ],
        "{t}"
    );
    // ACCENTS holds two keys: too few to compare. The test's list lacks MOVE-FILE too, and
    // never counts.
    assert!(
        !t.contains("ACCENTS") && !t.contains("catalog.test.ts"),
        "{t}"
    );
    // A constant of two siblings (STATE_*) has no peer.
    let t = find(&r, "failed", Some(FindKind::Key)).unwrap().text;
    assert!(t.starts_with("key \"failed\""), "{t}");
    assert!(!t.contains("missing"), "{t}");
}

#[test]
fn variants_are_lower_confidence_and_paths_still_win() {
    let r = repo();
    let t = find(&r, "move_file", None).unwrap().text;
    assert!(
        t.starts_with(
            "no key is the query as written; the closest differs in case or separators \
             (lower confidence): \"MOVE-FILE\"\n"
        ),
        "{t}"
    );
    // A query some path holds lists the paths, and points at the key.
    let a = find(&r, "catalog", None).unwrap();
    assert!(a.data.trace.is_none());
    assert!(a.text.contains("ui/src/actions/catalog.ts"), "{}", a.text);
    // Not a key anywhere: the usual answer, no hint.
    let a = find(&r, "FileOps", None).unwrap();
    assert!(
        a.data.trace.is_none() && a.data.key_hint.is_none(),
        "{}",
        a.text
    );
    let t = find(&r, "NOPE-NOPE", Some(FindKind::Key)).unwrap().text;
    assert!(t.starts_with("no string key \"NOPE-NOPE\""), "{t}");
}

/// `kind: key` on an index whose symbol layer predates string keys (or that has none) says
/// to re-index, rather than that the key does not exist.
#[test]
fn an_index_without_key_sites_says_to_reindex() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    let p = root.join("src/a.ts");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "export const A = [\"x-1\", \"x-2\", \"x-3\"];\n").unwrap();
    let mut out =
        cartograph_index::index(&root, &cartograph_index::IndexOptions::default()).unwrap();
    assert!(out.symbols.keys_indexed);
    let fresh = Repo::with_symbols(
        &out.graph,
        &out.symbols,
        cartograph_query::Config::default(),
    );
    let t = find(&fresh, "NOPE-1", Some(FindKind::Key)).unwrap().text;
    assert!(t.starts_with("no string key"), "{t}");
    // Written before key sites: the layer is there, its keys are not.
    out.symbols.keys.clear();
    out.symbols.keys_indexed = false;
    let old = Repo::with_symbols(
        &out.graph,
        &out.symbols,
        cartograph_query::Config::default(),
    );
    let e = find(&old, "x-1", Some(FindKind::Key))
        .unwrap_err()
        .to_string();
    assert!(e.contains("re-run `cartograph index`"), "{e}");
    // No symbol layer at all.
    let none = Repo::new(&out.graph, cartograph_query::Config::default());
    let e = find(&none, "x-1", Some(FindKind::Key))
        .unwrap_err()
        .to_string();
    assert!(e.contains("re-run `cartograph index`"), "{e}");
}

/// A registration alone in its container is a use, and reads as one.
#[test]
fn a_lone_registration_is_a_use() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    let p = root.join("src/a.ts");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(
        &p,
        "export const ONE = \"one-key\";\nexport function f() {\n  register(\"one-key\", () => 1);\n}\n",
    )
    .unwrap();
    let out = cartograph_index::index(&root, &cartograph_index::IndexOptions::default()).unwrap();
    let r = Repo::with_symbols(
        &out.graph,
        &out.symbols,
        cartograph_query::Config::default(),
    );
    let a = find(&r, "one-key", None).unwrap();
    let t = &a.text;
    assert_eq!(section(t, "used"), ["src/a.ts:3 in f"], "{t}");
    let tr = a.data.trace.as_ref().unwrap();
    assert!(tr.runs.is_empty(), "{t}");
    assert_eq!(tr.uses[0].form, "use", "{t}");
}

/// A word or a variant that symbols hold gives the symbols, and names the key after them.
#[test]
fn symbols_win_over_a_word_or_a_variant() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    let files = [
        (
            "src/fields.ts",
            "export const FIELDS = [\"width1\", \"width2\", \"width3\"];\nexport const ACTIONS = [\"PRINT-PAGE\", \"SAVE-PAGE\", \"OPEN-PAGE\"];\n",
        ),
        (
            "src/view.ts",
            "export function labelWidth1() {}\nexport function printPage() {}\n",
        ),
    ];
    for (rel, text) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let out = cartograph_index::index(&root, &cartograph_index::IndexOptions::default()).unwrap();
    let r = Repo::with_symbols(
        &out.graph,
        &out.symbols,
        cartograph_query::Config::default(),
    );
    // A plain word: the symbols, then the key.
    let a = find(&r, "width1", None).unwrap();
    assert!(a.data.trace.is_none(), "{}", a.text);
    assert!(a.text.contains("labelWidth1"), "{}", a.text);
    assert!(
        a.text.contains("it is also a string key: \"width1\""),
        "{}",
        a.text
    );
    // A variant of a key: the symbols, then the key it is a variant of.
    let a = find(&r, "printPage", None).unwrap();
    assert!(a.data.trace.is_none(), "{}", a.text);
    assert!(
        a.text
            .contains("a string key differs from it only in case or separators: \"PRINT-PAGE\""),
        "{}",
        a.text
    );
    // Written like a key: the trace.
    let a = find(&r, "PRINT-PAGE", None).unwrap();
    assert!(a.data.trace.is_some(), "{}", a.text);
    // Asked for: the trace of a word too.
    let a = find(&r, "width1", Some(FindKind::Key)).unwrap();
    assert!(a.text.starts_with("key \"width1\""), "{}", a.text);
}

/// A single repo (Python): an enum defines the keys, a dispatch table holds every one but
/// one, and a list offers them all.
#[test]
fn a_single_python_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    // Eleven actions; the handlers run all but one.
    let names = [
        "open", "save", "print", "share", "move", "sign", "scan", "send", "lock", "unlock", "copy",
    ];
    let mut actions = String::from("from enum import Enum\n\nclass Action(Enum):\n");
    for n in names {
        actions.push_str(&format!("    {}_DOC = \"{n}-doc\"\n", n.to_uppercase()));
    }
    let mut handlers = String::from("def run(a):\n    pass\n\nHANDLERS = {\n");
    for n in names.iter().filter(|n| **n != "copy") {
        handlers.push_str(&format!("    \"{n}-doc\": run,\n"));
    }
    handlers.push_str("}\n");
    let menu = format!(
        "MENU = [{}]\n",
        names
            .iter()
            .map(|n| format!("\"{n}-doc\""))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let files = [
        ("app/actions.py", actions),
        ("app/handlers.py", handlers),
        ("app/menu.py", menu),
    ];
    for (rel, text) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let out = cartograph_index::index(&root, &cartograph_index::IndexOptions::default()).unwrap();
    let r = Repo::with_symbols(
        &out.graph,
        &out.symbols,
        cartograph_query::Config::default(),
    );
    let t = find(&r, "copy-doc", None).unwrap().text;
    assert!(t.starts_with("key \"copy-doc\":\n"), "{t}");
    assert!(
        t.contains("app/actions.py:14 enum Action.COPY_DOC\n"),
        "{t}"
    );
    assert!(t.contains("app/menu.py:1 MENU[]\n"), "{t}");
    assert_eq!(
        section(&t, "missing from"),
        ["HANDLERS (app/handlers.py:4): holds 10 of the 10 other keys of Action (app/actions.py)"],
        "{t}"
    );
}
