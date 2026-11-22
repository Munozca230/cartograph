//! `find` ranking: subsequence-only hits are noise next to substring hits.

mod common;

use cartograph_query::find::find;
use cartograph_query::{Config, Repo};
use common::index_dir;
use std::fs;
use std::path::Path;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

#[test]
fn find_drops_subsequence_noise_when_substring_hits_exist() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("f");
    write(&root, "src/resolve/mod.rs", "\n");
    write(&root, "src/resolve/ts.rs", "\n");
    // Only by subsequence: r-e-s-o-l-v-e spread over the path.
    write(&root, "tests/remote_solver.rs", "\n");
    write(&root, "levels/rest/old_level.tres", "\n");
    let repo = Repo::new(&index_dir(&root), Config::default());
    let hits: Vec<String> = find(&repo, "resolve", None)
        .unwrap()
        .data
        .hits
        .into_iter()
        .map(|h| h.label)
        .collect();
    assert!(hits.contains(&"src/resolve/".to_string()), "{hits:?}");
    assert!(
        !hits
            .iter()
            .any(|h| h == "tests/remote_solver.rs" || h == "levels/rest/old_level.tres"),
        "{hits:?}"
    );
    // A typo with no substring hit still finds something by subsequence.
    let typo = find(&repo, "rslv", None).unwrap().data.hits;
    assert!(!typo.is_empty());
}
