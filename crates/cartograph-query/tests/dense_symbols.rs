//! `symbols` of files with many definitions (the `dense-symbols` fixture): an Electron
//! preload that exposes one object with 150 functions, a Java class with 60 methods, a
//! Rust module with a struct and many functions and a module of 120 constants. Every name fits the budget, or the answer
//! says how many were left out.
//! Regenerate the goldens with `UPDATE_GOLDEN=1 cargo test -p cartograph-query --test
//! dense_symbols` and review the diff.

mod common;

use cartograph_index::{IndexOptions, index};
use cartograph_query::budget::{JSON_BUDGET_FACTOR, json_tokens, limits, tokens};
use cartograph_query::{Answer, Config, Repo, symbols};
use common::{fixture, golden, repo_root};
use serde::Serialize;
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

/// `+N more` counts in a text.
fn cut(text: &str) -> usize {
    text.split('+')
        .skip(1)
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
    assert!(
        missing.is_empty(),
        "missing {missing:?}:
{}",
        a.text
    );
    assert_eq!(cut(&a.text), 0, "{}", a.text);
    golden("query-dense/symbols-codes.txt", &a.text);
}

#[test]
fn an_object_too_big_for_the_budget_says_what_was_left_out() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("src");
    std::fs::create_dir_all(&dir).unwrap();
    let total = 800;
    let mut src = String::from("import { contextBridge } from \"electron\";\n");
    src.push_str("contextBridge.exposeInMainWorld(\"api\", {\n");
    for i in 0..total {
        src.push_str(&format!("  member{i:03}: () => {i},\n"));
    }
    src.push_str("});\n");
    std::fs::write(dir.join("preload.ts"), src).unwrap();
    let out = index(tmp.path(), &IndexOptions::default()).unwrap();
    let r = Repo::with_symbols(&out.graph, &out.symbols, Config::default());
    let a = symbols::symbols(&r, "src/preload.ts").unwrap();
    fits("800 members", &a);
    let shown = (0..total)
        .filter(|i| a.text.contains(&format!("member{i:03}")))
        .count();
    // The budget holds well over the 150 names of a preload, and the rest is counted.
    assert!(shown >= 150, "{shown} shown:\n{}", a.text);
    assert_eq!(shown + cut(&a.text), total, "{}", a.text);
}
