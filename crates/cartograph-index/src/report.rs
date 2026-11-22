//! Markdown report over a stored graph (`cartograph report`).

use crate::Timings;
use cartograph_core::{
    Confidence, EdgeKind, Graph, NodeKind, Store, UnresolvedImport, UnresolvedReason,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

/// How many directories the "top directories" table lists.
pub const TOP_DIRS: usize = 10;
/// How many rows the import tables list (examples, files, externals, dir dependencies).
pub const TOP_IMPORTS: usize = 10;

/// Render the Markdown report for the graph in `store`.
pub fn render(store: &Store) -> Result<String, cartograph_core::Error> {
    let (graph, meta) = store.load_snapshot()?;
    let repo = meta
        .get("repo_name")
        .cloned()
        .unwrap_or_else(|| "repo".into());
    let manifests: Vec<Value> = meta
        .get("manifests")
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let timings: Option<Timings> = meta
        .get("timings")
        .and_then(|s| serde_json::from_str(s).ok());
    Ok(render_parts(&repo, &graph, &manifests, timings.as_ref()))
}

/// Escape a value for a Markdown table cell.
fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn render_parts(
    repo: &str,
    graph: &Graph,
    manifests: &[Value],
    timings: Option<&Timings>,
) -> String {
    let mut md = String::new();
    let files: Vec<_> = graph.nodes_of(NodeKind::File).collect();
    let total_loc: u64 = files.iter().map(|f| f.loc).sum();
    let total_bytes: u64 = files.iter().map(|f| f.bytes).sum();

    // `write!` into a String cannot fail; results are ignored on purpose.
    let _ = writeln!(md, "# cartograph report: {}\n", cell(repo));
    let _ = writeln!(
        md,
        "{} files, {} directories, {} packages, {} LOC, {} bytes, {} skipped.\n",
        files.len(),
        graph.nodes_of(NodeKind::Dir).count(),
        graph.nodes_of(NodeKind::Package).count(),
        total_loc,
        total_bytes,
        graph.skipped.len()
    );

    // Languages.
    let mut langs: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for f in &files {
        let e = langs
            .entry(f.lang.as_deref().unwrap_or("(other)"))
            .or_default();
        e.0 += 1;
        e.1 += f.loc;
    }
    let mut langs: Vec<_> = langs.into_iter().collect();
    langs.sort_by(|a, b| b.1.1.cmp(&a.1.1).then(a.0.cmp(b.0)));
    let _ = writeln!(
        md,
        "## Languages\n\n| Language | Files | LOC |\n|---|---:|---:|"
    );
    for (lang, (n, loc)) in langs {
        let _ = writeln!(md, "| {} | {n} | {loc} |", cell(lang));
    }

    // Manifests and packages.
    let _ = writeln!(md, "\n## Manifests and packages\n");
    if manifests.is_empty() {
        let _ = writeln!(md, "No manifests found.");
    } else {
        let _ = writeln!(
            md,
            "| Manifest | Kind | Package | Notes |\n|---|---|---|---|"
        );
        for m in manifests {
            let path = m["path"].as_str().unwrap_or("?");
            let kind = m["kind"].as_str().unwrap_or("?");
            let package = graph
                .node(&cartograph_core::node_id(NodeKind::Package, path))
                .map(|n| n.name.clone())
                .unwrap_or_else(|| "(presence only)".into());
            let _ = writeln!(
                md,
                "| {} | {} | {} | {} |",
                cell(path),
                cell(kind),
                cell(&package),
                cell(&manifest_notes(m))
            );
        }
    }

    // Top directories by LOC.
    let mut dirs: Vec<_> = graph.nodes_of(NodeKind::Dir).collect();
    dirs.sort_by(|a, b| b.loc.cmp(&a.loc).then(a.path.cmp(&b.path)));
    let _ = writeln!(
        md,
        "\n## Top directories by LOC\n\n| Directory | Files | LOC |\n|---|---:|---:|"
    );
    for d in dirs.iter().take(TOP_DIRS) {
        let _ = writeln!(
            md,
            "| {} | {} | {} |",
            cell(&d.path),
            d.meta["files"].as_u64().unwrap_or(0),
            d.loc
        );
    }

    render_imports(&mut md, graph);

    // Skipped.
    let _ = writeln!(md, "\n## Skipped ({})\n", graph.skipped.len());
    if graph.skipped.is_empty() {
        let _ = writeln!(md, "Nothing skipped.");
    } else {
        let _ = writeln!(md, "| Path | Reason | Detail |\n|---|---|---|");
        for s in &graph.skipped {
            let _ = writeln!(
                md,
                "| {} | {} | {} |",
                cell(&s.path),
                s.reason,
                cell(s.detail.as_deref().unwrap_or(""))
            );
        }
    }

    // Diagnostics: problems in config files that were indexed (not skipped).
    let _ = writeln!(md, "\n## Diagnostics ({})\n", graph.diagnostics.len());
    if graph.diagnostics.is_empty() {
        let _ = writeln!(md, "No config problems.");
    } else {
        let _ = writeln!(md, "| File | Problem |\n|---|---|");
        for d in &graph.diagnostics {
            let _ = writeln!(md, "| {} | {} |", cell(&d.path), cell(&d.detail));
        }
    }

    // Timing.
    let _ = writeln!(md, "\n## Index timing\n");
    match timings {
        Some(t) => {
            let _ = writeln!(
                md,
                "| Phase | ms |\n|---|---:|\n| walk | {} |\n| read + hash + parse imports | {} |\n| build graph | {} |\n| resolve imports | {} |\n| write db | {} |\n| total | {} |",
                t.walk_ms, t.read_ms, t.build_ms, t.resolve_ms, t.write_ms, t.total_ms
            );
        }
        None => {
            let _ = writeln!(md, "No timing recorded.");
        }
    }
    md
}

fn manifest_notes(m: &Value) -> String {
    let mut notes = Vec::new();
    if let Some(e) = m["error"].as_str() {
        notes.push(format!("parse error: {e}"));
    }
    let d = &m["details"];
    let list = |v: &Value| -> Option<String> {
        let a = v.as_array().filter(|a| !a.is_empty())?;
        Some(
            a.iter()
                .map(|x| match x {
                    Value::String(s) => s.clone(),
                    Value::Object(o) => o
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string(),
                    other => other.to_string(),
                })
                .collect::<Vec<_>>()
                .join(", "),
        )
    };
    for (key, label) in [
        ("workspace_members", "members"),
        ("workspaces", "workspaces"),
        ("modules", "modules"),
        ("projects", "projects"),
        ("autoloads", "autoloads"),
        ("project_references", "references"),
        ("subdirectories", "subdirectories"),
    ] {
        if let Some(s) = list(&d[key]) {
            notes.push(format!("{label}: {s}"));
        }
    }
    if let Some(s) = d["main_scene"].as_str() {
        notes.push(format!("main scene: {s}"));
    }
    notes.join("; ")
}

fn pct(n: usize, total: usize) -> String {
    if total == 0 {
        "-".into()
    } else {
        format!("{:.1}%", n as f64 * 100.0 / total as f64)
    }
}

/// Language family label of a file for the import tables.
fn family(lang: Option<&str>) -> &'static str {
    match lang {
        Some("typescript" | "tsx" | "javascript" | "jsx") => "TS/JS",
        Some("python") => "Python",
        Some("rust") => "Rust",
        Some("c" | "cpp") => "C/C++",
        Some("go") => "Go",
        Some("java") => "Java",
        Some("csharp") => "C#",
        Some("gdscript" | "godot-scene" | "godot-resource" | "godot-project") => "Godot",
        _ => "other",
    }
}

#[derive(Default)]
struct Counts {
    internal: usize,
    external: usize,
    unresolved: usize,
}

impl Counts {
    fn total(&self) -> usize {
        self.internal + self.external + self.unresolved
    }
}

/// Import sections: totals, unresolved, externals, fan-in / fan-out, dir dependencies.
fn render_imports(md: &mut String, graph: &Graph) {
    let langs: HashMap<&str, Option<&str>> = graph
        .nodes_of(NodeKind::File)
        .map(|n| (n.path.as_str(), n.lang.as_deref()))
        .collect();
    let fam = |file: &str| family(langs.get(file).copied().flatten());
    let confidence: HashMap<(&str, &str), Confidence> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import)
        .map(|e| ((e.src.as_str(), e.dst.as_str()), e.confidence))
        .collect();

    let mut by_family: BTreeMap<&str, Counts> = BTreeMap::new();
    let mut by_confidence: BTreeMap<Confidence, usize> = BTreeMap::new();
    // Sites that resolve to their own file (Rust `use super::*` in `mod tests`) have no edge.
    let mut same_file = 0usize;
    let mut all = Counts::default();
    for s in &graph.imports {
        let c = by_family.entry(fam(&s.file)).or_default();
        if s.target.starts_with("external:") {
            c.external += 1;
            all.external += 1;
        } else {
            c.internal += 1;
            all.internal += 1;
            let src = format!("file:{}", s.file);
            if src == s.target {
                same_file += 1;
            } else if let Some(conf) = confidence.get(&(src.as_str(), s.target.as_str())) {
                *by_confidence.entry(*conf).or_default() += 1;
            }
        }
    }
    for u in &graph.unresolved {
        by_family.entry(fam(&u.file)).or_default().unresolved += 1;
        all.unresolved += 1;
    }

    let _ = writeln!(md, "\n## Imports\n");
    let total = all.total();
    if total == 0 {
        let _ = writeln!(
            md,
            "No imports found (N1 covers TS/JS, Python, Rust, C/C++, Go, Java, C# and Godot)."
        );
        return;
    }
    let _ = writeln!(
        md,
        "{total} imports: {} resolved to files ({}), {} external ({}), {} unresolved ({}).\n",
        all.internal,
        pct(all.internal, total),
        all.external,
        pct(all.external, total),
        all.unresolved,
        pct(all.unresolved, total)
    );
    let _ = writeln!(
        md,
        "| Language | Imports | Resolved | External | Unresolved | Resolved + external |\n|---|---:|---:|---:|---:|---:|"
    );
    for (f, c) in by_family.iter().chain(std::iter::once((&"all", &all))) {
        let t = c.total();
        let _ = writeln!(
            md,
            "| {f} | {t} | {} ({}) | {} ({}) | {} ({}) | {} |",
            c.internal,
            pct(c.internal, t),
            c.external,
            pct(c.external, t),
            c.unresolved,
            pct(c.unresolved, t),
            pct(c.internal + c.external, t)
        );
    }
    if !by_confidence.is_empty() || same_file > 0 {
        let mut parts: Vec<String> = by_confidence
            .iter()
            .map(|(c, n)| format!("{c} {n}"))
            .collect();
        if same_file > 0 {
            parts.push(format!("own file {same_file}"));
        }
        let _ = writeln!(md, "\nFile imports by confidence: {}.", parts.join(", "));
    }

    // Unresolved by reason, then examples.
    let _ = writeln!(md, "\n### Unresolved imports ({})\n", all.unresolved);
    if graph.unresolved.is_empty() {
        let _ = writeln!(md, "None.");
    } else {
        let mut reasons: BTreeMap<UnresolvedReason, usize> = BTreeMap::new();
        for u in &graph.unresolved {
            *reasons.entry(u.reason).or_default() += 1;
        }
        let _ = writeln!(md, "| Reason | Count |\n|---|---:|");
        for (r, n) in &reasons {
            let _ = writeln!(md, "| {r} | {n} |");
        }
        // Group identical (reason, specifier) pairs so one line copied into many files
        // (or a common gap) shows once, with its count and first occurrence.
        let mut groups: BTreeMap<(UnresolvedReason, &str), (usize, &UnresolvedImport)> =
            BTreeMap::new();
        for u in &graph.unresolved {
            groups
                .entry((u.reason, u.specifier.as_str()))
                .or_insert((0, u))
                .0 += 1;
        }
        let mut groups: Vec<_> = groups.into_iter().collect();
        groups.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
        let _ = writeln!(
            md,
            "\nTop {TOP_IMPORTS} by specifier ({} distinct):\n\n| Reason | Specifier | Count | First at | Detail |\n|---|---|---:|---|---|",
            groups.len()
        );
        for ((reason, spec), (n, u)) in groups.iter().take(TOP_IMPORTS) {
            let _ = writeln!(
                md,
                "| {reason} | `{}` | {n} | {}:{} | {} |",
                cell(spec).replace('`', "'"),
                cell(&u.file),
                u.line,
                cell(u.detail.as_deref().unwrap_or(""))
            );
        }
    }

    // Top external packages.
    let mut externals: BTreeMap<&str, (usize, std::collections::BTreeSet<&str>)> = BTreeMap::new();
    for s in graph
        .imports
        .iter()
        .filter(|s| s.target.starts_with("external:"))
    {
        let e = externals
            .entry(s.target.strip_prefix("external:").unwrap_or(&s.target))
            .or_default();
        e.0 += 1;
        e.1.insert(s.file.as_str());
    }
    let mut externals: Vec<_> = externals.into_iter().collect();
    externals.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
    let _ = writeln!(
        md,
        "\n### Top external packages ({} distinct)\n",
        externals.len()
    );
    if externals.is_empty() {
        let _ = writeln!(md, "None.");
    } else {
        let _ = writeln!(md, "| Package | Imports | Files |\n|---|---:|---:|");
        for (name, (n, files)) in externals.iter().take(TOP_IMPORTS) {
            let _ = writeln!(md, "| {} | {n} | {} |", cell(name), files.len());
        }
    }

    // Fan-in / fan-out over distinct file -> file import edges.
    let mut fan_in: BTreeMap<&str, usize> = BTreeMap::new();
    let mut fan_out: BTreeMap<&str, usize> = BTreeMap::new();
    for e in graph.edges.iter().filter(|e| e.kind == EdgeKind::Import) {
        if let Some(src) = e.src.strip_prefix("file:") {
            *fan_out.entry(src).or_default() += 1;
        }
        // Files only: a namespace-package Dir target is not a file.
        if let Some(dst) = e.dst.strip_prefix("file:") {
            *fan_in.entry(dst).or_default() += 1;
        }
    }
    for (title, map) in [("fan-in", fan_in), ("fan-out", fan_out)] {
        let mut rows: Vec<_> = map.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let _ = writeln!(md, "\n### Top files by {title}\n");
        if rows.is_empty() {
            let _ = writeln!(md, "None.");
            continue;
        }
        let _ = writeln!(md, "| File | Files |\n|---|---:|");
        for (f, n) in rows.iter().take(TOP_IMPORTS) {
            let _ = writeln!(md, "| {} | {n} |", cell(f));
        }
    }

    // Dir -> Dir dependencies.
    let mut deps: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::DependsOn && !e.src.starts_with("package:"))
        .collect();
    deps.sort_by(|a, b| {
        b.weight
            .cmp(&a.weight)
            .then(a.src.cmp(&b.src))
            .then(a.dst.cmp(&b.dst))
    });
    let _ = writeln!(md, "\n### Top directory dependencies ({})\n", deps.len());
    if deps.is_empty() {
        let _ = writeln!(md, "None.");
    } else {
        let show = |id: &str| -> String {
            match id.strip_prefix("dir:") {
                Some(p) => p.to_string(),
                None => "(repo root)".into(),
            }
        };
        let _ = writeln!(md, "| From | To | File imports |\n|---|---|---:|");
        for e in deps.iter().take(TOP_IMPORTS) {
            let _ = writeln!(
                md,
                "| {} | {} | {} |",
                cell(&show(&e.src)),
                cell(&show(&e.dst)),
                e.weight
            );
        }
    }
}
