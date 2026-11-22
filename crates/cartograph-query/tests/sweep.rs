//! Budget sweep over a real index: every folder, the 50 most depended-on files, the 50 most
//! changed files (git) and every location a finding names, through every query, text and
//! JSON; symbols of every folder and hot file, callers of the 60 most-called symbols, entries,
//! symbol search and hotspots of every folder. Ignored by default (it needs an index); run it
//! with
//! `CARTOGRAPH_SWEEP_DB=<data dir>/graph.db cargo test --release -p cartograph-query --test sweep -- --ignored`.

use cartograph_core::Store;
use cartograph_query::budget::{json_tokens, limits, tokens};
use cartograph_query::deps::{Direction, Level};
use cartograph_query::graph::NodeRef;
use cartograph_query::list::Filter;
use cartograph_query::{
    Answer, Config, QueryError, Repo, check, deps, explain, find, hotspots, impact, list, overview,
    path, symbols,
};
use serde::Serialize;

fn over<T: Serialize>(
    what: String,
    a: Result<Answer<T>, QueryError>,
    budget: usize,
    bad: &mut Vec<String>,
    n: &mut usize,
) {
    *n += 1;
    match a {
        Ok(a) => {
            let (t, j) = (tokens(&a.text), json_tokens(&a.data));
            if t > budget || j > budget {
                bad.push(format!("{what}: text {t}, json {j} > {budget}"));
            }
        }
        // A bad argument (a library as the target of path…) is a fine answer.
        Err(QueryError::BadArgument(_)) => {}
        Err(e) => bad.push(format!("{what}: {e}")),
    }
}

#[test]
#[ignore = "needs CARTOGRAPH_SWEEP_DB"]
fn budgets_hold_on_a_real_index() {
    let Some(db) = std::env::var_os("CARTOGRAPH_SWEEP_DB") else {
        return;
    };
    let store = Store::open(&db).unwrap();
    let (graph, syms) = store.load_graph_and_symbols().unwrap();
    let history = store.load_history().unwrap();
    let r = Repo::with_symbols(&graph, &syms, Config::default()).with_history(&history);
    let g = &r.graph;
    let mut ids: Vec<String> = (0..g.dirs.len() as u32)
        .map(|d| g.id(NodeRef::Dir(d)))
        .collect();
    let mut hot: Vec<u32> = (0..g.files.len() as u32).collect();
    hot.sort_by_key(|&f| std::cmp::Reverse(g.fan_in(f)));
    ids.extend(hot.iter().take(50).map(|&f| g.id(NodeRef::File(f))));
    // The most changed files: the longest owner and co-change lists.
    let mut churn: Vec<u32> = (0..g.files.len() as u32).collect();
    churn.sort_by_key(|&f| std::cmp::Reverse(r.git.of(f).map_or(0, |h| h.commits)));
    ids.extend(churn.iter().take(50).map(|&f| g.id(NodeRef::File(f))));
    for f in r.findings() {
        ids.extend(f.nodes.iter().cloned());
    }
    ids.sort();
    ids.dedup();
    let (mut bad, mut n) = (Vec::new(), 0usize);
    for id in &ids {
        if id.starts_with("dir:") || id == "repo:" {
            for depth in [1, 2, 4] {
                over(
                    format!("overview {id} {depth}"),
                    overview::overview(&r, Some(id), depth),
                    limits::OVERVIEW,
                    &mut bad,
                    &mut n,
                );
            }
            over(
                format!("findings within {id}"),
                list::findings(
                    &r,
                    &Filter {
                        within: Some(id.clone()),
                        ..Filter::default()
                    },
                ),
                limits::FINDINGS,
                &mut bad,
                &mut n,
            );
        }
        over(
            format!("explain {id}"),
            explain::explain(&r, id),
            limits::EXPLAIN,
            &mut bad,
            &mut n,
        );
        for (dir, depth, level) in [
            (Direction::Both, 3, Level::File),
            (Direction::Both, 3, Level::Folder),
            (Direction::In, 2, Level::File),
        ] {
            over(
                format!("deps {id} {dir:?} {depth} {level:?}"),
                deps::deps(&r, id, dir, depth, level),
                limits::DEPS,
                &mut bad,
                &mut n,
            );
        }
        over(
            format!("impact {id}"),
            impact::impact(&r, std::slice::from_ref(id)),
            limits::IMPACT,
            &mut bad,
            &mut n,
        );
        let name = id.rsplit('/').next().unwrap_or(id).to_string();
        if !name.trim().is_empty() && !name.ends_with(':') {
            over(
                format!("find {name}"),
                find::find(&r, &name, None),
                limits::FIND,
                &mut bad,
                &mut n,
            );
        }
    }
    // Paths between pairs of the hottest files and between top-level folders.
    for w in hot.iter().take(30).collect::<Vec<_>>().windows(2) {
        let (a, b) = (g.id(NodeRef::File(*w[0])), g.id(NodeRef::File(*w[1])));
        over(
            format!("path {a} {b}"),
            path::path(&r, &a, &b),
            limits::PATH,
            &mut bad,
            &mut n,
        );
        over(
            format!("path {b} {a}"),
            path::path(&r, &b, &a),
            limits::PATH,
            &mut bad,
            &mut n,
        );
    }
    let top: Vec<String> = g.dirs[0]
        .dirs
        .iter()
        .map(|&d| g.id(NodeRef::Dir(d)))
        .collect();
    for a in &top {
        for b in &top {
            if a != b {
                over(
                    format!("path {a} {b}"),
                    path::path(&r, a, b),
                    limits::PATH,
                    &mut bad,
                    &mut n,
                );
            }
        }
    }
    over(
        "findings".into(),
        list::findings(&r, &Filter::default()),
        limits::FINDINGS,
        &mut bad,
        &mut n,
    );
    over(
        "check".into(),
        Ok(check::check(&r)),
        limits::CHECK,
        &mut bad,
        &mut n,
    );
    // The symbol layer: every folder and checked file, the most-called symbols, entries.
    for id in &ids {
        if id.starts_with("dir:") || id == "repo:" || id.starts_with("file:") {
            over(
                format!("symbols {id}"),
                symbols::symbols(&r, id),
                limits::SYMBOLS,
                &mut bad,
                &mut n,
            );
        }
        if id.starts_with("dir:") || id == "repo:" {
            over(
                format!("hotspots within {id}"),
                hotspots::hotspots(&r, Some(id)),
                limits::HOTSPOTS,
                &mut bad,
                &mut n,
            );
        }
        if id.starts_with("dir:") {
            over(
                format!("entries within {id}"),
                symbols::entries(&r, None, Some(id), false),
                limits::ENTRIES,
                &mut bad,
                &mut n,
            );
        }
    }
    let mut called: Vec<(usize, String)> = {
        let mut m: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for c in &syms.calls {
            *m.entry(c.target.as_str()).or_default() += 1;
        }
        m.into_iter().map(|(k, v)| (v, k.to_string())).collect()
    };
    called.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for (_, id) in called.iter().take(60) {
        over(
            format!("callers {id}"),
            symbols::callers(&r, id),
            limits::CALLERS,
            &mut bad,
            &mut n,
        );
        let name = id.rsplit(['#', '.']).next().unwrap_or(id).to_string();
        over(
            format!("find sym {name}"),
            find::find_symbols(&r, &name),
            limits::FIND,
            &mut bad,
            &mut n,
        );
    }
    over(
        "entries".into(),
        symbols::entries(&r, None, None, false),
        limits::ENTRIES,
        &mut bad,
        &mut n,
    );
    over(
        "hotspots".into(),
        hotspots::hotspots(&r, None),
        limits::HOTSPOTS,
        &mut bad,
        &mut n,
    );
    eprintln!("{n} answers checked over {} nodes", ids.len());
    assert!(
        bad.is_empty(),
        "{} over budget:\n{}",
        bad.len(),
        bad.join("\n")
    );
}
