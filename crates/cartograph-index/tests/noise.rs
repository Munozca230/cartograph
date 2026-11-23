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
    write(root, "tmp/scratch.txt", "x\n");
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

#[test]
fn long_lines_skip_only_js_bundles() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // A Godot scene or resource keeps big arrays on one line: still indexed.
    let array = format!(
        "[node name=\"Main\"]\ndata = PackedFloat32Array({})\n",
        "1.0, ".repeat(9_000)
    );
    write(root, "game/main.tscn", &array);
    write(root, "game/tiles.tres", &array);
    let out = walk(root, DEFAULT_MAX_FILE_BYTES, &[], WalkHooks::default());
    let files: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(files, vec!["game/main.tscn", "game/tiles.tres"]);
}

#[test]
fn a_data_folder_imported_from_another_kept_one_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "src/a.ts",
        "import { take } from './screenshots/take';\ntake();\n",
    );
    write(
        root,
        "src/screenshots/take.ts",
        "import { fmt } from '../coverage/fmt';\nexport function take() { fmt(); }\n",
    );
    write(root, "src/coverage/fmt.ts", "export function fmt() {}\n");
    let out = walk(root, DEFAULT_MAX_FILE_BYTES, &[], WalkHooks::default());
    let files: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        files,
        vec!["src/a.ts", "src/coverage/fmt.ts", "src/screenshots/take.ts"]
    );
}

#[test]
fn code_folders_named_like_logs_or_backups_are_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Programs nothing imports, in folders whose names are also used for output.
    write(
        root,
        "cmd/backup/main.go",
        "package main\n\nfunc main() {}\n",
    );
    write(root, "backupctl/run.go", "package main\n\nfunc main() {}\n");
    write(root, "src/bin/backup/main.rs", "fn main() {}\n");
    write(root, "tools/log/tail.py", "print(1)\n");
    // Output and copies are still left out, code or not.
    write(root, "_backup-2026-01-01/old.ts", "export const a = 1;\n");
    write(root, "src.bak/old.ts", "export const a = 1;\n");
    write(root, "logs/api.log", "x\n");
    let out = walk(root, DEFAULT_MAX_FILE_BYTES, &[], WalkHooks::default());
    let files: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        files,
        vec![
            "backupctl/run.go",
            "cmd/backup/main.go",
            "src/bin/backup/main.rs",
            "tools/log/tail.py"
        ]
    );
}
