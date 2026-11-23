//! Data and tool folders are left out of the walk with a reason, unless an import names
//! them; minified bundles are skipped like oversized files.

use cartograph_core::SkipReason;
use cartograph_index::walk::{DEFAULT_MAX_FILE_BYTES, WalkHooks, walk};
use std::path::Path;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

#[test]
fn data_folders_are_skipped_unless_imported() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "package.json", r#"{"name":"app"}"#);
    write(
        root,
        "src/a.ts",
        "import { view } from './logs/view';\nview();\n",
    );
    write(root, "src/logs/view.ts", "export function view() {}\n");
    write(root, "logs/api.log", "started\n");
    write(root, "logs/old/api.log.1", "started\n");
    write(root, "tmp/scratch.ts", "export const x = 1;\n");
    for i in 0..6 {
        write(root, &format!("capturas/run/s{i}.json"), "{}\n");
        write(root, &format!("capturas/run/s{i}.png"), "png\n");
    }
    write(root, ".notes/a.md", "# notes\n");
    write(root, "vendor/chart.min.js", "var a=1;\n");
    let long = format!("var a={};\n", "1+".repeat(20_000));
    write(root, "vendor/bundle.js", &long);
    let out = walk(root, DEFAULT_MAX_FILE_BYTES, &[], WalkHooks::default());
    let files: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(files, vec!["package.json", "src/a.ts", "src/logs/view.ts"]);
    assert!(
        !out.dirs
            .iter()
            .any(|d| d.starts_with("logs") || d == "capturas/run")
    );
    let skipped = |p: &str| {
        out.skipped
            .iter()
            .find(|s| s.path == p)
            .map(|s| (s.reason, s.detail.clone().unwrap_or_default()))
    };
    for dir in ["logs", "tmp", "capturas", ".notes"] {
        let (reason, detail) = skipped(dir).unwrap_or_else(|| panic!("{dir}"));
        assert_eq!(reason, SkipReason::ExcludedDir, "{dir}");
        assert!(!detail.is_empty(), "{dir}");
    }
    assert!(skipped("capturas").unwrap().1.contains("named like output"));
    assert_eq!(
        skipped("vendor/chart.min.js").unwrap().0,
        SkipReason::Minified
    );
    let (reason, detail) = skipped("vendor/bundle.js").unwrap();
    assert_eq!(reason, SkipReason::Minified);
    assert!(detail.contains("bytes per line"), "{detail}");
}
