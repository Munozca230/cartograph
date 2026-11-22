//! Orphans: code nothing depends on that is not an entry point.
//!
//! - File level for languages whose imports name files: TS/JS, Python, Rust, GDScript and
//!   Godot scenes, C/C++ headers. Entry points (see `graph::entry_reason`: main/index/app
//!   names, routes, crate roots and cargo targets, translation units, Godot autoloads and
//!   main scene, package roots, declaration files, stories, tests, scripts, config) never
//!   count.
//! - Package level for Go: a package (folder) no other folder imports and without `main.go`.
//! - Not reported for Java, Kotlin and C#: classes of one package see each other without
//!   imports and frameworks wire classes by scanning, so "nothing imports it" says little
//!   without symbol-level usage.

use super::{Ctx, Finding, FindingKind, Severity};
use crate::graph::{FileIx, NodeRef};
use crate::kind::FileKind;
use std::collections::BTreeMap;

fn file_level(lang: Option<&str>) -> bool {
    matches!(
        lang,
        Some(
            "typescript"
                | "tsx"
                | "javascript"
                | "jsx"
                | "python"
                | "rust"
                | "gdscript"
                | "godot-scene"
                | "godot-resource"
                | "c"
                | "cpp"
        )
    )
}

pub(super) fn orphans(cx: &Ctx<'_>) -> Vec<Finding> {
    let g = cx.g;
    let mut out = Vec::new();
    // Distinct active importers per file, module declarations included (a Rust file only
    // `mod`-declared by its parent is used; one nobody declares is not even compiled).
    let indeg: Vec<usize> = (0..g.files.len())
        .map(|i| {
            g.inn[i]
                .iter()
                .filter(|d| cx.active[d.file as usize])
                .count()
        })
        .collect();
    for (i, f) in g.files.iter().enumerate() {
        if !cx.active[i]
            || f.kind != FileKind::Code
            || f.entry.is_some()
            || indeg[i] > 0
            || !file_level(f.lang.as_deref())
        {
            continue;
        }
        let id = g.id(NodeRef::File(i as FileIx));
        let uses = cx.adj[i].len();
        let tests: Vec<FileIx> = g.inn[i]
            .iter()
            .map(|d| d.file)
            .filter(|&t| g.files[t as usize].kind == FileKind::Test)
            .collect();
        let (severity, title) = if !tests.is_empty() {
            (
                Severity::Info,
                format!(
                    "only tests use it ({} test{}): dead outside tests?",
                    tests.len(),
                    if tests.len() == 1 { "" } else { "s" }
                ),
            )
        } else if uses == 0 {
            (
                Severity::Warning,
                "nothing depends on it and it depends on nothing (dead file?)".to_string(),
            )
        } else {
            (
                Severity::Warning,
                format!(
                    "nothing depends on it (dead file? it uses {})",
                    crate::text::files(uses)
                ),
            )
        };
        let mut x = Finding::new(FindingKind::Orphan, severity, &id, title);
        x.node(id);
        for &t in tests.iter().take(3) {
            x.line(format!("used by test {}", g.path(t)));
        }
        x.metric("loc", f.loc as f64);
        x.metric("fan_out", uses as f64);
        out.push(x);
    }
    out.extend(go_packages(cx));
    out
}

/// Go packages (folders of non-test `.go` files) that no other folder imports.
fn go_packages(cx: &Ctx<'_>) -> Vec<Finding> {
    let g = cx.g;
    let mut pkgs: BTreeMap<u32, Vec<FileIx>> = BTreeMap::new();
    for (i, f) in g.files.iter().enumerate() {
        if cx.active[i] && f.lang.as_deref() == Some("go") && f.kind == FileKind::Code {
            pkgs.entry(f.dir).or_default().push(i as FileIx);
        }
    }
    let mut out = Vec::new();
    for (dir, files) in pkgs {
        if files.iter().any(|&f| g.files[f as usize].name == "main.go") {
            continue;
        }
        let used = files.iter().any(|&f| {
            g.inn[f as usize]
                .iter()
                .any(|d| cx.active[d.file as usize] && g.files[d.file as usize].dir != dir)
        });
        if used {
            continue;
        }
        let tested = g.dirs[dir as usize]
            .files
            .iter()
            .any(|&f| g.files[f as usize].kind == FileKind::Test);
        let id = g.id(NodeRef::Dir(dir));
        let (severity, title) = if tested {
            (
                Severity::Info,
                format!(
                    "Go package no other package imports; only its own tests use it ({})",
                    crate::text::files(files.len())
                ),
            )
        } else {
            (
                Severity::Warning,
                format!(
                    "Go package nothing imports ({}, no main.go)",
                    crate::text::files(files.len())
                ),
            )
        };
        let mut x = Finding::new(FindingKind::Orphan, severity, &id, title);
        x.node(id);
        x.metric("files", files.len() as f64);
        out.push(x);
    }
    out
}
