//! `symbols` of files with many definitions (the `dense-symbols` fixture): an Electron
//! preload that exposes one object with 150 functions, a Java class with 60 methods, a
//! Rust module with a struct and many functions and a module of 120 constants; generated
//! files with 800 members, 500 small classes and a class whose called methods keep their
//! line. Every name fits the budget, or the answer says how many were left out; the JSON
//! rebuilds each id from `id_prefix`.
//! Regenerate the goldens with `UPDATE_GOLDEN=1 cargo test -p cartograph-query --test
//! dense_symbols` and review the diff.

mod common;

use cartograph_index::{IndexOptions, index};
use cartograph_query::budget::{JSON_BUDGET_FACTOR, json_tokens, limits, tokens};
use cartograph_query::{Answer, Config, Repo, symbols};
use common::{fixture, golden, repo_root};
use serde::Serialize;
use serde_json::Value;
use std::sync::OnceLock;

fn dense() -> &'static Repo {
    static DENSE: OnceLock<Repo> = OnceLock::new();
    DENSE.get_or_init(|| fixture("dense-symbols"))
}

fn fits<T: Serialize>(what: &str, a: &Answer<T>) {
    let (t, j) = (tokens(&a.text), json_tokens(&a.data));
    assert!(t <= limits::SYMBOLS, "{what}: text {t} tokens:\n{}", a.text);
    assert!(
        j <= limits::SYMBOLS * JSON_BUDGET_FACTOR,
        "{what}: JSON {j} tokens"
    );
    assert!(
        !a.text.contains("(truncated)"),
        "{what}: cut line by line:\n{}",
        a.text
    );
}

/// Index files written to a temp dir (`(path, source)`).
fn temp_repo(files: &[(&str, String)]) -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    for (path, src) in files {
        let p = tmp.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, src).unwrap();
    }
    let out = index(tmp.path(), &IndexOptions::default()).unwrap();
    Repo::with_symbols(&out.graph, &out.symbols, Config::default())
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn num(v: &Value) -> usize {
    v.as_u64().unwrap_or(0) as usize
}

/// The ids a file of a `symbols` answer names (on a line, in a member group or among the
/// others), rebuilt from its `id_prefix`, and how many it counts as left out.
fn named_ids(file: &Value) -> (Vec<String>, usize) {
    let prefix = file["id_prefix"].as_str().expect("id_prefix").to_string();
    let mut ids: Vec<String> = file["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| format!("{prefix}{}", x["name"].as_str().unwrap()))
        .collect();
    let mut left = num(&file["more"]) + num(&file["members_more"]);
    for g in file["members"].as_array().into_iter().flatten() {
        let c = g["container"].as_str().unwrap();
        ids.extend(strs(&g["names"]).iter().map(|n| format!("{prefix}{c}.{n}")));
        left += num(&g["more"]);
    }
    ids.extend(strs(&file["others"]).iter().map(|n| format!("{prefix}{n}")));
    (ids, left)
}

/// Every id the answer names is a symbol of the repo, and named + left out = total.
fn accounted<T: Serialize>(r: &Repo, a: &Answer<T>) -> usize {
    let v = serde_json::to_value(&a.data).unwrap();
    let file = &v["groups"][0];
    let (ids, left) = named_ids(file);
    for id in &ids {
        assert!(r.symbols.symbol(id).is_some(), "not an id: {id}");
    }
    assert_eq!(ids.len() + left, num(&file["total"]), "{}", a.text);
    left
}

/// The names of the functions the fixture's preload exposes, in file order.
fn exposed() -> Vec<String> {
    let src = std::fs::read_to_string(
        repo_root().join("tests/fixtures/dense-symbols/src/electron/preload.ts"),
    )
    .unwrap();
    src.lines()
        .filter(|l| l.contains("=> send("))
        .filter_map(|l| l.trim().split(':').next())
        .map(str::to_string)
        .collect()
}

/// `+N more` counts in a text (not the `+N more lines` of a text cut line by line).
fn cut(text: &str) -> usize {
    text.split('+')
        .skip(1)
        .filter(|x| !x.contains(" more lines"))
        .filter_map(|x| x.split(" more").next()?.parse::<usize>().ok())
        .sum()
}

#[test]
fn every_function_of_a_big_exposed_object_is_named() {
    let names = exposed();
    assert_eq!(names.len(), 150);
    let a = symbols::symbols(dense(), "src/electron/preload.ts").unwrap();
    fits("preload", &a);
    let missing: Vec<&String> = names.iter().filter(|n| !a.text.contains(*n)).collect();
    assert!(missing.is_empty(), "missing {missing:?}:\n{}", a.text);
    assert!(a.text.contains("appAPI (150, pub): "), "{}", a.text);
    assert_eq!(cut(&a.text), 0, "nothing is left out:\n{}", a.text);
    // The helper they all call keeps its detailed row.
    assert!(a.text.contains("function send L4-6 ←150"), "{}", a.text);
    // The group is not under `send`, which is no container.
    assert!(a.text.contains("\nappAPI (150, pub): "), "{}", a.text);
    assert_eq!(accounted(dense(), &a), 0);
    golden("query-dense/symbols-preload.txt", &a.text);
}

#[test]
fn a_class_with_sixty_methods_keeps_a_row_per_method() {
    let a = symbols::symbols(dense(), "src/main/java/com/example/BigService.java").unwrap();
    fits("java class", &a);
    assert!(a.text.contains("class BigService L2-183 pub"), "{}", a.text);
    let rows = a
        .text
        .lines()
        .filter(|l| l.contains("method BigService."))
        .count();
    assert_eq!(rows, 60, "{}", a.text);
    assert_eq!(cut(&a.text), 0, "{}", a.text);
    assert_eq!(accounted(dense(), &a), 0);
    golden("query-dense/symbols-java.txt", &a.text);
}

#[test]
fn a_rust_module_keeps_a_row_per_definition() {
    let a = symbols::symbols(dense(), "src/rs/lib.rs").unwrap();
    fits("rust module", &a);
    assert!(a.text.contains("method Store.op_number_25 L"), "{}", a.text);
    assert!(
        a.text.contains("function helper_function_35 L"),
        "{}",
        a.text
    );
    assert_eq!(cut(&a.text), 0, "{}", a.text);
    golden("query-dense/symbols-rust.txt", &a.text);
}

#[test]
fn every_constant_of_a_dense_module_is_named() {
    let a = symbols::symbols(dense(), "src/codes/codes.ts").unwrap();
    fits("constants", &a);
    let missing: Vec<String> = (1..=120)
        .map(|i| format!("CODE_{i:03}"))
        .filter(|n| !a.text.contains(n.as_str()))
        .collect();
    assert!(missing.is_empty(), "missing {missing:?}:\n{}", a.text);
    assert_eq!(cut(&a.text), 0, "{}", a.text);
    assert_eq!(accounted(dense(), &a), 0);
    golden("query-dense/symbols-codes.txt", &a.text);
}

#[test]
fn an_object_too_big_for_the_budget_says_what_was_left_out() {
    let total = 800;
    let mut src = String::from("import { contextBridge } from \"electron\";\n");
    src.push_str("contextBridge.exposeInMainWorld(\"api\", {\n");
    for i in 0..total {
        src.push_str(&format!("  member{i:03}: () => {i},\n"));
    }
    src.push_str("});\n");
    let r = temp_repo(&[("src/preload.ts", src)]);
    let a = symbols::symbols(&r, "src/preload.ts").unwrap();
    fits("800 members", &a);
    let shown = (0..total)
        .filter(|i| a.text.contains(&format!("member{i:03}")))
        .count();
    // The budget holds well over the 150 names of a preload, and the rest is counted.
    assert!(shown >= 150, "{shown} shown:\n{}", a.text);
    assert_eq!(shown + cut(&a.text), total, "{}", a.text);
    assert_eq!(accounted(&r, &a), total - shown);
}

#[test]
fn five_hundred_small_classes_stay_within_the_budget() {
    let mut src = String::new();
    for i in 0..500 {
        src.push_str(&format!(
            "export class C{i:03} {{\n  start() {{ return 1; }}\n  stop() {{ return 2; }}\n}}\n"
        ));
    }
    let r = temp_repo(&[("src/many.ts", src)]);
    let a = symbols::symbols(&r, "src/many.ts").unwrap();
    assert_eq!(a.data.total, 1500);
    fits("500 classes", &a);
    let left = accounted(&r, &a);
    assert!(left > 0, "{}", a.text);
    assert_eq!(cut(&a.text), left, "{}", a.text);
}

#[test]
fn called_members_keep_their_line_when_the_rest_go_by_name() {
    let mut src =
        String::from("export class Big {\n  m000() {\n    this.m007();\n    this.m142();\n  }\n");
    for i in 1..200 {
        src.push_str(&format!("  m{i:03}() {{\n    return {i};\n  }}\n"));
    }
    src.push_str("}\n");
    let r = temp_repo(&[("src/big.ts", src)]);
    let a = symbols::symbols(&r, "src/big.ts").unwrap();
    fits("class with called methods", &a);
    for m in ["m007", "m142"] {
        let row = format!("  method Big.{m} L");
        let line = a.text.lines().find(|l| l.starts_with(&row));
        assert!(line.is_some_and(|l| l.ends_with("←1")), "{m}:\n{}", a.text);
    }
    // The class's own line, then its other methods by name, under it.
    assert!(a.text.contains("\nclass Big L1-"), "{}", a.text);
    assert!(
        a.text.contains("\n  Big (198, pub): m000, m001, "),
        "{}",
        a.text
    );
    assert_eq!(accounted(&r, &a), 0);
}
