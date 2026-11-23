//! What Spring says is annotated, never subtracted: every caller and every flow callee the
//! Java fixtures had before Spring was read (`tests/golden/java-baseline/*.json`, written
//! by that version and not to be regenerated with a later one) is still there. Only a
//! callee without a body may give its place to what the beans run.

mod common;

use cartograph_query::dispatch::Dispatcher;
use cartograph_query::flow::{CallIndex, symbol_card};
use cartograph_query::symbols;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

type Rows = BTreeMap<String, BTreeSet<String>>;

fn rows(v: &Value) -> Rows {
    serde_json::from_value(v.clone()).unwrap()
}

#[test]
fn no_caller_and_no_callee_is_lost() {
    let mut lost = Vec::new();
    let mut compared = 0;
    for name in [
        "inherit-java",
        "java-receivers",
        "spring-java",
        "workspace-bridges",
        "workspace-keys",
        "workspace-slice",
    ] {
        let path = common::repo_root().join(format!("tests/golden/java-baseline/{name}.json"));
        let was: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let r = common::fixture(name);
        let ix = CallIndex::build(&r);
        let dispatch = Dispatcher::new(&r.symbols);
        for (id, sites) in rows(&was["callers"]) {
            let d = symbols::callers(&r, &id).unwrap().data;
            for site in sites {
                compared += 1;
                // `file:path:line`, or `file:path (n)` for tests listed by file.
                let there = d.groups.iter().any(|g| {
                    site == format!("{} ({})", g.file, g.count)
                        || g.sites
                            .iter()
                            .any(|c| site == format!("{}:{}", g.file, c.line))
                });
                if !there {
                    lost.push(format!("{name}: caller of {id}: {site}"));
                }
            }
        }
        for (id, callees) in rows(&was["callees"]) {
            let card = symbol_card(&r, &ix, &id).unwrap();
            let now: BTreeSet<&str> = card.callees.iter().map(|c| c.id.as_str()).collect();
            for to in callees {
                compared += 1;
                let beans = dispatch.declared(&to);
                let replaced = beans.iter().any(|(m, _)| now.contains(m.as_str()));
                if !now.contains(to.as_str()) && !replaced {
                    lost.push(format!("{name}: callee of {id}: {to}"));
                }
            }
        }
    }
    assert!(compared > 100, "{compared}");
    assert!(lost.is_empty(), "{}", lost.join("\n"));
}
