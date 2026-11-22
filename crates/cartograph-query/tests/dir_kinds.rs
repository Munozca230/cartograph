//! The kind of a folder (code, test, data…) as overview shows it.

mod common;

use cartograph_query::{Config, Repo, overview};
use common::index_dir;
use std::fs;
use std::path::Path;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

#[test]
fn a_package_folder_full_of_code_is_code_not_data() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("k");
    // A web app: many config/data files at the top, the code under src/.
    for f in [
        "web/package.json",
        "web/package-lock.json",
        "web/tsconfig.json",
        "web/README.md",
        "web/DESIGN.md",
        "web/vite.config.ts",
        "web/vitest.config.ts",
        "web/eslint.config.js",
    ] {
        write(&root, f, "{}\n");
    }
    for i in 0..12 {
        write(&root, &format!("web/src/m{i}.ts"), "export const x = 1;\n");
    }
    write(&root, "web/test/a.test.ts", "export {};\n");
    // A folder that really is data: docs with one sample script.
    for i in 0..5 {
        write(&root, &format!("docs/p{i}.md"), "# p\n");
    }
    write(&root, "docs/sample.ts", "export const s = 1;\n");
    let repo = Repo::new(&index_dir(&root), Config::default());
    let o = overview::overview(&repo, None, 1).unwrap().data;
    let kind = |id: &str| o.tree.iter().find(|r| r.id == id).map(|r| r.kind.as_str());
    assert_eq!(kind("dir:web"), Some("code"), "{:?}", o.tree);
    assert_eq!(kind("dir:docs"), Some("data"), "{:?}", o.tree);
}
