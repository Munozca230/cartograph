//! Mount invariance: indexing a repo as one mount under a prefix (`m`, as a workspace
//! member) gives the same graph and symbol layer as indexing it alone, with every repo path
//! and id under `m/`, plus the member's own `dir:m` node between the repo and its top-level
//! entries. Nothing a resolver decides may depend on where the mount sits: the mount root
//! plays the part of the repo root.
//!
//! A single mount with the empty prefix is exactly the standalone index.

use cartograph_index::{IndexOptions, Mount, index, index_mounts, repo_name};
use serde_json::{Map, Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const PREFIX: &str = "m";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dst = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dst);
        } else {
            fs::copy(entry.path(), &dst).unwrap();
        }
    }
}

fn under(rel: &str) -> String {
    if rel.is_empty() {
        PREFIX.to_string()
    } else {
        format!("{PREFIX}/{rel}")
    }
}

/// `kind:path` ids of repo nodes moved under the prefix; others (repo, externals) kept.
fn move_id(id: &str) -> String {
    for kind in ["file:", "dir:", "sym:", "package:"] {
        if let Some(rest) = id.strip_prefix(kind) {
            return format!("{kind}{}", under(rest));
        }
    }
    id.to_string()
}

/// Keys whose string value is a repo-relative path.
const PATH_KEYS: [&str; 4] = ["path", "file", "dir", "old_path"];

/// Move every repo path and id in `v` under the prefix. `external` marks an External node,
/// whose path is an `ecosystem:name`, not a repo path.
fn move_value(v: &mut Value, key: Option<&str>, external: bool) {
    match v {
        Value::String(s) => {
            if key.is_some_and(|k| TEXT_KEYS.contains(&k)) {
                // Messages are compared with their paths unmoved (see `unmove_texts`).
            } else if key.is_some_and(|k| PATH_KEYS.contains(&k)) && !external && !s.contains("://")
            {
                *s = under(s);
            } else {
                *s = move_id(s);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| move_value(x, key, external)),
        Value::Object(o) => {
            // External nodes name an `ecosystem:name`; the repo node is the root itself.
            let external = matches!(
                o.get("kind").and_then(Value::as_str),
                Some("external" | "repo")
            );
            // A page entry point is named after the page's path.
            let page = o.get("target").is_some() && o.get("kind") == Some(&json!("main"));
            for (k, x) in o.iter_mut() {
                match x {
                    Value::String(n) if page && k == "name" && n.ends_with(".html") => {
                        *n = under(n);
                    }
                    _ => move_value(x, Some(k), external),
                }
            }
        }
        _ => {}
    }
}

/// Keys whose string value is a message that may name repo paths.
const TEXT_KEYS: [&str; 2] = ["detail", "reason"];

/// Messages name graph paths, which carry the prefix: compare them with every path that
/// starts with the prefix unmoved (`m/src/a.ts` → `src/a.ts`).
fn unmove_texts(v: &mut Value, key: Option<&str>) {
    match v {
        Value::String(s) if key.is_some_and(|k| TEXT_KEYS.contains(&k)) => {
            let token = format!("{PREFIX}/");
            let mut out = String::new();
            let mut rest = s.as_str();
            while let Some(i) = rest.find(&token) {
                let starts_word = rest[..i]
                    .chars()
                    .last()
                    .or_else(|| out.chars().last())
                    .is_none_or(|c| !(c.is_alphanumeric() || "_.-/".contains(c)));
                out.push_str(&rest[..i]);
                if !starts_word {
                    out.push_str(&token);
                }
                rest = &rest[i + token.len()..];
            }
            out.push_str(rest);
            *s = out;
        }
        Value::Array(a) => a.iter_mut().for_each(|x| unmove_texts(x, key)),
        Value::Object(o) => o.iter_mut().for_each(|(k, x)| unmove_texts(x, Some(k))),
        _ => {}
    }
}

/// Sort every array of the view by the text of its items, so the order the indexer sorted
/// paths in (which moving them may change) does not matter.
fn sort_arrays(v: &mut Value) {
    match v {
        Value::Array(a) => {
            a.iter_mut().for_each(sort_arrays);
            a.sort_by_cached_key(|x| x.to_string());
        }
        Value::Object(o) => o.values_mut().for_each(sort_arrays),
        _ => {}
    }
}

/// The standalone index of `root` as it must look mounted under the prefix.
fn expected(root: &Path) -> Value {
    let out = index(root, &IndexOptions::default()).unwrap();
    let mut g = serde_json::to_value(&out.graph).unwrap();
    let mut s = serde_json::to_value(&out.symbols).unwrap();
    // Top-level entries hang from the member's folder, which hangs from the repo.
    let top: Vec<String> = out
        .graph
        .edges
        .iter()
        .filter(|e| e.src == "repo:" && e.kind == cartograph_core::EdgeKind::Contains)
        .filter(|e| !e.dst.starts_with("package:"))
        .map(|e| e.dst.clone())
        .collect();
    move_value(&mut g, None, false);
    move_value(&mut s, None, false);
    let repo = out.graph.node("repo:").unwrap();
    let edges = g["edges"].as_array_mut().unwrap();
    let member = json!(format!("dir:{PREFIX}"));
    for e in edges.iter_mut() {
        let dst = e["dst"].as_str().unwrap().to_string();
        if e["src"] == "repo:" && top.iter().any(|t| move_id(t) == dst) {
            e["src"] = member.clone();
        }
        // Top-level files live in the member's folder, not in the repo.
        if e["kind"] == "depends_on" {
            for end in ["src", "dst"] {
                if e[end] == "repo:" {
                    e[end] = member.clone();
                }
            }
        }
    }
    // A package at the member's root contains the member's folder.
    for n in out.graph.nodes_of(cartograph_core::NodeKind::Package) {
        if n.meta["dir"] == "" {
            edges.push(json!({
                "src": move_id(&n.id), "dst": member, "kind": "contains",
                "confidence": "exact", "weight": 1
            }));
        }
    }
    edges.push(json!({
        "src": "repo:", "dst": format!("dir:{PREFIX}"), "kind": "contains",
        "confidence": "exact", "weight": 1
    }));
    let mut dir = Map::new();
    dir.insert("id".into(), json!(format!("dir:{PREFIX}")));
    dir.insert("kind".into(), json!("dir"));
    dir.insert("path".into(), json!(PREFIX));
    dir.insert("name".into(), json!(PREFIX));
    dir.insert("lang".into(), Value::Null);
    dir.insert("loc".into(), json!(repo.loc));
    dir.insert("bytes".into(), json!(repo.bytes));
    dir.insert("hash".into(), Value::Null);
    dir.insert("meta".into(), repo.meta.clone());
    g["nodes"].as_array_mut().unwrap().push(Value::Object(dir));
    let mut v = json!({ "graph": g, "symbols": s });
    sort_arrays(&mut v);
    v
}

fn mounted(root: &Path, prefix: &str) -> Value {
    let mounts = [Mount::new(prefix, root)];
    let out = index_mounts(&repo_name(root), &mounts, &IndexOptions::default()).unwrap();
    let mut v = json!({
        "graph": serde_json::to_value(&out.graph).unwrap(),
        "symbols": serde_json::to_value(&out.symbols).unwrap(),
    });
    if !prefix.is_empty() {
        unmove_texts(&mut v, None);
    }
    sort_arrays(&mut v);
    v
}

/// The first differences between two views, as readable lines.
fn differences(want: &Value, got: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for part in ["graph", "symbols"] {
        let (Some(w), Some(g)) = (want[part].as_object(), got[part].as_object()) else {
            continue;
        };
        for (k, wv) in w {
            let gv = g.get(k).unwrap_or(&Value::Null);
            if wv == gv {
                continue;
            }
            match (wv.as_array(), gv.as_array()) {
                (Some(wa), Some(ga)) => {
                    for x in wa.iter().filter(|x| !ga.contains(x)).take(4) {
                        out.push(format!("{part}.{k} missing: {x}"));
                    }
                    for x in ga.iter().filter(|x| !wa.contains(x)).take(4) {
                        out.push(format!("{part}.{k} extra:   {x}"));
                    }
                }
                _ => out.push(format!("{part}.{k}: want {wv}, got {gv}")),
            }
        }
        for k in g.keys().filter(|k| !w.contains_key(*k)) {
            out.push(format!("{part}.{k}: unexpected"));
        }
    }
    out
}

fn fixtures() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(repo_root().join("tests/fixtures"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        // A workspace is mounts already; it cannot be a member.
        .filter(|e| !e.path().join(cartograph_core::WORKSPACE_FILE).exists())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    names.sort();
    names
}

#[test]
fn every_fixture_mounted_under_a_prefix_is_the_standalone_index_moved() {
    let mut failures = Vec::new();
    for name in fixtures() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(&name);
        copy_dir(&repo_root().join("tests/fixtures").join(&name), &root);
        let want = expected(&root);
        let got = mounted(&root, PREFIX);
        if want != got {
            let diff = differences(&want, &got);
            failures.push(format!("{name}:\n  {}", diff.join("\n  ")));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn a_single_mount_without_prefix_is_the_standalone_index() {
    for name in ["imports-ts", "polyglot", "c-compdb"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(name);
        copy_dir(&repo_root().join("tests/fixtures").join(name), &root);
        let alone = index(&root, &IndexOptions::default()).unwrap();
        let mounts = [Mount::new("", &root)];
        let one = index_mounts(&repo_name(&root), &mounts, &IndexOptions::default()).unwrap();
        assert_eq!(alone.graph, one.graph, "{name}");
        assert_eq!(alone.symbols, one.symbols, "{name}");
    }
}
