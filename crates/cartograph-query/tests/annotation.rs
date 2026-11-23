//! Notes: `explain_with` shows the note of the node (stale or not) and stays in budget with
//! the longest note the MCP `annotate` tool accepts.

mod common;

use cartograph_query::budget::{JSON_BUDGET_FACTOR, json_tokens, limits, tokens};
use cartograph_query::explain::{Annotation, explain_with};

#[test]
fn explain_shows_the_note_within_budget() {
    let repo = common::fixture("query-app");
    let mut seen = None;
    let a = explain_with(&repo, "src/core/model.ts", |id| {
        seen = Some(id.to_string());
        Some(Annotation {
            text: "Domain model; plain data.".into(),
            stale: false,
        })
    })
    .unwrap();
    assert_eq!(seen.as_deref(), Some("file:src/core/model.ts"));
    assert!(
        a.text.contains("note: Domain model; plain data.\n"),
        "{}",
        a.text
    );

    let a = explain_with(&repo, "src/core", |_| {
        Some(Annotation {
            text: "é".repeat(600),
            stale: true,
        })
    })
    .unwrap();
    assert!(a.text.contains("note (stale"), "{}", a.text);
    assert!(tokens(&a.text) <= limits::EXPLAIN, "{}", tokens(&a.text));
    assert!(json_tokens(&a.data) <= limits::EXPLAIN * JSON_BUDGET_FACTOR);

    let a = explain_with(&repo, "src/main.ts", |_| None).unwrap();
    assert!(a.text.ends_with("note: none\n"), "{}", a.text);
}
