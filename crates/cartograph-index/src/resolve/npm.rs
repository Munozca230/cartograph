//! `package.json` and workspaces: package names, entry points (`exports`, `module`, `main`)
//! and declared dependencies.
//!
//! Workspace roots are `package.json` files with a `workspaces` field (an array or
//! `{ "packages": [...] }`) and `pnpm-workspace.yaml` files. Their members are the
//! `package.json` dirs below the root that match the globs (`!` negates).

use super::{depth, is_ancestor, join, parent};
use cartograph_core::Diagnostic;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The parts of a `package.json` that resolution uses.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PackageJson {
    pub dir: String,
    pub name: Option<String>,
    pub main: Option<String>,
    pub module: Option<String>,
    pub exports: Option<Value>,
    /// Names in `dependencies`, `devDependencies`, `peerDependencies`,
    /// `optionalDependencies`.
    pub deps: BTreeSet<String>,
    pub workspaces: Vec<String>,
}

impl PackageJson {
    pub fn parse(path: &str, text: &str) -> Result<PackageJson, String> {
        let v: Value =
            serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let mut deps = BTreeSet::new();
        for k in [
            "dependencies",
            "devDependencies",
            "peerDependencies",
            "optionalDependencies",
        ] {
            if let Some(o) = v.get(k).and_then(Value::as_object) {
                deps.extend(o.keys().cloned());
            }
        }
        let workspaces = match v.get("workspaces") {
            Some(Value::Array(a)) => a.clone(),
            Some(Value::Object(o)) => o
                .get("packages")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            _ => Vec::new(),
        }
        .into_iter()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect();
        Ok(PackageJson {
            dir: parent(path).to_string(),
            name: s("name"),
            main: s("main"),
            module: s("module"),
            exports: v.get("exports").cloned(),
            deps,
            workspaces,
        })
    }
}

/// Globs listed under `packages:` in a `pnpm-workspace.yaml`. A minimal reader: block
/// lists (`- 'apps/*'`) and flow lists (`['apps/*', 'libs/*']`, also over several lines)
/// directly under the `packages` key.
pub fn pnpm_workspace_globs(text: &str) -> Vec<String> {
    let unquote = |s: &str| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
    let mut out = Vec::new();
    let mut in_packages = false;
    // Text of a flow list being read, from its `[`.
    let mut flow: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.split(" #").next().unwrap_or(line).trim();
        if let Some(buf) = flow.as_mut() {
            buf.push_str(trimmed);
            if trimmed.contains(']') {
                out.extend(flow_items(buf));
                flow = None;
                in_packages = false;
            }
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let top_level =
            !line.starts_with(' ') && !line.starts_with('\t') && !trimmed.starts_with('-');
        let value = if top_level {
            let (key, value) = trimmed.split_once(':').unwrap_or((trimmed, ""));
            in_packages = key.trim() == "packages";
            value.trim()
        } else {
            trimmed
        };
        if !in_packages {
            continue;
        }
        if value.starts_with('[') {
            if value.contains(']') {
                out.extend(flow_items(value));
                in_packages = false;
            } else {
                flow = Some(value.to_string());
            }
        } else if !top_level && let Some(item) = value.strip_prefix('-') {
            out.push(unquote(item));
        }
    }
    out
}

/// Items of a one-line flow list `[a, 'b', "c"]`.
fn flow_items(text: &str) -> Vec<String> {
    let inner = text
        .split_once('[')
        .map_or(text, |(_, r)| r)
        .split(']')
        .next()
        .unwrap_or("");
    inner
        .split(',')
        .map(|s| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// One workspace root and its member packages by name.
#[derive(Debug, Default)]
pub struct WorkspaceRoot {
    pub dir: String,
    pub members: BTreeMap<String, Vec<String>>,
}

/// Result of looking a bare package name up in the workspaces around a file.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup<'a> {
    None,
    One(&'a str),
    /// Several member packages of the same root share the name.
    Many(&'a [String]),
}

/// All workspace roots, deepest first.
#[derive(Debug, Default)]
pub struct Workspaces {
    pub roots: Vec<WorkspaceRoot>,
}

impl Workspaces {
    /// `globs_by_root`: workspace root dir → member globs. `packages`: every parsed
    /// `package.json` by dir.
    pub fn build(
        globs_by_root: &BTreeMap<String, (String, Vec<String>)>,
        packages: &BTreeMap<String, PackageJson>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Workspaces {
        let mut roots = Vec::new();
        for (root, (manifest, globs)) in globs_by_root {
            let (include, exclude) = compile(globs, manifest, diagnostics);
            let mut ws = WorkspaceRoot {
                dir: root.clone(),
                members: BTreeMap::new(),
            };
            for (dir, pkg) in packages {
                if dir == root || !is_ancestor(root, dir) {
                    continue;
                }
                let rel = if root.is_empty() {
                    dir.as_str()
                } else {
                    dir.get(root.len() + 1..).unwrap_or("")
                };
                if include.is_match(rel)
                    && !exclude.is_match(rel)
                    && let Some(name) = &pkg.name
                {
                    ws.members
                        .entry(name.clone())
                        .or_default()
                        .push(dir.clone());
                }
            }
            roots.push(ws);
        }
        roots.sort_by(|a, b| depth(&b.dir).cmp(&depth(&a.dir)).then(a.dir.cmp(&b.dir)));
        Workspaces { roots }
    }

    /// The member package called `name` in the nearest workspace root around `importer`
    /// that has one.
    pub fn lookup(&self, importer: &str, name: &str) -> Lookup<'_> {
        for root in &self.roots {
            if !is_ancestor(&root.dir, importer) {
                continue;
            }
            match root.members.get(name).map(Vec::as_slice) {
                Some([one]) => return Lookup::One(one),
                Some(many @ [_, _, ..]) => return Lookup::Many(many),
                _ => {}
            }
        }
        Lookup::None
    }
}

fn compile(
    globs: &[String],
    manifest: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> (GlobSet, GlobSet) {
    let mut include = GlobSetBuilder::new();
    let mut exclude = GlobSetBuilder::new();
    for g in globs {
        let (negated, pat) = match g.strip_prefix('!') {
            Some(p) => (true, p),
            None => (false, g.as_str()),
        };
        let pat = pat.trim_start_matches("./").trim_end_matches('/');
        match GlobBuilder::new(pat).literal_separator(true).build() {
            Ok(glob) if negated => {
                exclude.add(glob);
            }
            Ok(glob) => {
                include.add(glob);
            }
            Err(e) => diagnostics.push(Diagnostic {
                path: manifest.to_string(),
                detail: format!("workspace glob {g:?}: {e}"),
            }),
        }
    }
    let build = |b: GlobSetBuilder| b.build().unwrap_or_else(|_| GlobSet::empty());
    (build(include), build(exclude))
}

/// Conditions tried in `exports`, in order. Source-like conditions first: the graph wants
/// the file in the repo, and build outputs are usually not indexed.
const CONDITIONS: [&str; 8] = [
    "source", "import", "module", "default", "require", "node", "browser", "types",
];

/// Targets `exports` maps `key` (`"."` or `"./sub/path"`) to, in preference order.
pub fn export_targets(exports: &Value, key: &str) -> Vec<String> {
    match exports {
        Value::String(s) if key == "." => vec![s.clone()],
        Value::Object(map) if map.keys().any(|k| k.starts_with('.')) => {
            if let Some(v) = map.get(key) {
                return conditions(v);
            }
            // Subpath patterns ("./*", "./features/*.js"): like Node, only the pattern with
            // the longest prefix before `*` applies (ties: the longer pattern).
            let best = map
                .iter()
                .filter(|(pattern, _)| pattern.contains('*'))
                .filter_map(|(pattern, v)| {
                    let capture = super::tsconfig::match_pattern(pattern, key)?;
                    let prefix = pattern.split_once('*').map_or(0, |(p, _)| p.len());
                    Some(((prefix, pattern.len()), capture, v))
                })
                .max_by(|a, b| a.0.cmp(&b.0));
            match best {
                Some((_, capture, v)) => conditions(v)
                    .into_iter()
                    .map(|t| t.replacen('*', capture, 1))
                    .collect(),
                None => Vec::new(),
            }
        }
        Value::Object(_) | Value::Array(_) if key == "." => conditions(exports),
        _ => Vec::new(),
    }
}

fn conditions(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().flat_map(conditions).collect(),
        Value::Object(map) => {
            let mut out = Vec::new();
            for c in CONDITIONS {
                if let Some(x) = map.get(c) {
                    out.extend(conditions(x));
                }
            }
            for (k, x) in map {
                if !CONDITIONS.contains(&k.as_str()) {
                    out.extend(conditions(x));
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

/// Split a bare specifier into package name and subpath: `@a/b/c/d` → (`@a/b`, `c/d`),
/// `lodash/fp` → (`lodash`, `fp`). `None` if it is not a plausible npm package name.
pub fn split_package(spec: &str) -> Option<(&str, &str)> {
    let cut = if spec.starts_with('@') {
        // A scoped name needs a non-empty scope and a non-empty name: `@scope/name`.
        let first = spec.find('/').filter(|&i| i > 1)?;
        let rest = spec.get(first + 1..)?;
        if rest.is_empty() || rest.starts_with('/') {
            return None;
        }
        rest.find('/').map(|i| first + 1 + i)
    } else {
        spec.find('/')
    };
    let (name, sub) = match cut {
        Some(i) => (spec.get(..i)?, spec.get(i + 1..)?),
        None => (spec, ""),
    };
    valid_name(name).then_some((name, sub))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.starts_with('_')
        && !name.starts_with('~')
        && name.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '@' | '/' | '~' | '!')
        })
}

/// Map of every `package.json` by dir, plus workspace globs by root dir.
pub type Parsed = (
    BTreeMap<String, PackageJson>,
    BTreeMap<String, (String, Vec<String>)>,
);

/// Parse every retained `package.json` and `pnpm-workspace.yaml`.
pub fn parse_all(configs: &HashMap<&str, &str>) -> Parsed {
    let mut packages = BTreeMap::new();
    let mut roots: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    let mut paths: Vec<&&str> = configs.keys().collect();
    paths.sort();
    for path in paths {
        let Some(text) = configs.get(*path) else {
            continue;
        };
        let name = path.rsplit('/').next().unwrap_or(path);
        let dir = parent(path).to_string();
        if name == "package.json" {
            match PackageJson::parse(path, text) {
                Ok(p) => {
                    if !p.workspaces.is_empty() {
                        roots
                            .entry(dir.clone())
                            .or_insert_with(|| (path.to_string(), Vec::new()))
                            .1
                            .extend(p.workspaces.iter().cloned());
                    }
                    packages.insert(dir, p);
                }
                // Already reported as a manifest parse error by the N0 pass.
                Err(e) => tracing::debug!(path, error = %e, "package.json not usable"),
            }
        } else if name == "pnpm-workspace.yaml" {
            roots
                .entry(dir)
                .or_insert_with(|| (path.to_string(), Vec::new()))
                .1
                .extend(pnpm_workspace_globs(text));
        }
    }
    (packages, roots)
}

/// Dir of the nearest `package.json` at or above `dir`.
pub fn nearest_package<'p>(
    packages: &'p BTreeMap<String, PackageJson>,
    dir: &str,
) -> Option<&'p PackageJson> {
    let mut d = dir;
    loop {
        if let Some(p) = packages.get(d) {
            return Some(p);
        }
        if d.is_empty() {
            return None;
        }
        d = parent(d);
    }
}

/// `dir/rel` for an entry-point field (`./dist/index.js`, `index`, `/x` is relative too).
pub fn entry_path(dir: &str, rel: &str) -> Option<String> {
    join(dir, rel.trim_start_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn package_names() {
        assert_eq!(split_package("react"), Some(("react", "")));
        assert_eq!(split_package("lodash/fp"), Some(("lodash", "fp")));
        assert_eq!(split_package("@a/b"), Some(("@a/b", "")));
        assert_eq!(split_package("@a/b/c/d"), Some(("@a/b", "c/d")));
        assert_eq!(split_package("@a"), None);
        assert_eq!(split_package("@/x"), None);
        assert_eq!(split_package("~/x"), None);
        assert_eq!(split_package("has space"), None);
    }

    #[test]
    fn exports_forms() {
        assert_eq!(export_targets(&json!("./i.js"), "."), vec!["./i.js"]);
        assert_eq!(
            export_targets(&json!({"import": "./m.js", "require": "./c.cjs"}), "."),
            vec!["./m.js", "./c.cjs"]
        );
        let e = json!({".": {"types": "./t.d.ts", "default": "./d.js"}, "./sub": "./s.js", "./f/*": "./src/f/*.ts"});
        assert_eq!(export_targets(&e, "."), vec!["./d.js", "./t.d.ts"]);
        assert_eq!(export_targets(&e, "./sub"), vec!["./s.js"]);
        assert_eq!(export_targets(&e, "./f/x"), vec!["./src/f/x.ts"]);
        assert!(export_targets(&e, "./nope").is_empty());
    }

    #[test]
    fn pnpm_globs() {
        let y = "packages:\n  - 'packages/*'\n  - \"apps/**\" # comment\n  - '!**/test/**'\ncatalog:\n  - nope\n";
        assert_eq!(
            pnpm_workspace_globs(y),
            vec!["packages/*", "apps/**", "!**/test/**"]
        );
    }

    #[test]
    fn workspace_lookup_prefers_the_nearest_root_and_flags_duplicates() {
        let mut packages = BTreeMap::new();
        for (dir, name) in [
            ("", "root"),
            ("packages/a", "@w/a"),
            ("packages/b", "@w/b"),
            ("packages/b2", "@w/b"),
            ("backup", "root"),
            ("backup/packages/a", "@w/a"),
        ] {
            packages.insert(
                dir.to_string(),
                PackageJson {
                    dir: dir.into(),
                    name: Some(name.into()),
                    ..PackageJson::default()
                },
            );
        }
        let mut roots = BTreeMap::new();
        roots.insert(
            String::new(),
            ("package.json".to_string(), vec!["packages/*".to_string()]),
        );
        roots.insert(
            "backup".to_string(),
            (
                "backup/package.json".to_string(),
                vec!["packages/*".to_string()],
            ),
        );
        let ws = Workspaces::build(&roots, &packages, &mut Vec::new());
        assert_eq!(
            ws.lookup("packages/b/x.ts", "@w/a"),
            Lookup::One("packages/a")
        );
        assert_eq!(
            ws.lookup("backup/packages/a/x.ts", "@w/a"),
            Lookup::One("backup/packages/a")
        );
        assert!(matches!(ws.lookup("x.ts", "@w/b"), Lookup::Many(_)));
        assert_eq!(ws.lookup("x.ts", "react"), Lookup::None);
    }
}
