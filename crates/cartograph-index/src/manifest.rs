//! Manifest detection and light parsing.
//!
//! Parsing is deliberately shallow: names, workspace members, module paths, Godot
//! autoloads. A manifest that fails to parse is still reported (with `error` set) so the
//! failure is visible instead of silently dropped.

use serde::Serialize;
use serde_json::{Value, json};

/// What kind of manifest a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManifestKind {
    Cargo,
    Npm,
    GoMod,
    Maven,
    Gradle,
    Csproj,
    Sln,
    Pyproject,
    Requirements,
    SetupPy,
    Godot,
    Cmake,
    CompileCommands,
}

impl ManifestKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ManifestKind::Cargo => "cargo",
            ManifestKind::Npm => "npm",
            ManifestKind::GoMod => "go-mod",
            ManifestKind::Maven => "maven",
            ManifestKind::Gradle => "gradle",
            ManifestKind::Csproj => "csproj",
            ManifestKind::Sln => "sln",
            ManifestKind::Pyproject => "pyproject",
            ManifestKind::Requirements => "requirements",
            ManifestKind::SetupPy => "setup-py",
            ManifestKind::Godot => "godot",
            ManifestKind::Cmake => "cmake",
            ManifestKind::CompileCommands => "compile-commands",
        }
    }

    /// Whether this manifest declares a package (a Package node). `compile_commands.json`
    /// is recorded for presence only.
    pub fn declares_package(self) -> bool {
        self != ManifestKind::CompileCommands
    }
}

/// A manifest found in the repo.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Manifest {
    /// Repo-relative path of the manifest file.
    pub path: String,
    pub kind: ManifestKind,
    /// Package name, when the manifest declares one.
    pub name: Option<String>,
    /// Kind-specific details (workspace members, autoloads…).
    pub details: Value,
    /// Parse error, if the manifest could not be read.
    pub error: Option<String>,
}

impl Manifest {
    /// Repo-relative directory holding the manifest (`""` for the root).
    pub fn dir(&self) -> &str {
        self.path.rsplit_once('/').map_or("", |(d, _)| d)
    }
}

/// Detect a manifest from its repo-relative path.
pub fn detect(path: &str) -> Option<ManifestKind> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let lower = name.to_ascii_lowercase();
    Some(match name {
        "Cargo.toml" => ManifestKind::Cargo,
        "package.json" => ManifestKind::Npm,
        "go.mod" => ManifestKind::GoMod,
        "pom.xml" => ManifestKind::Maven,
        "build.gradle" | "build.gradle.kts" => ManifestKind::Gradle,
        "pyproject.toml" => ManifestKind::Pyproject,
        "requirements.txt" => ManifestKind::Requirements,
        "setup.py" => ManifestKind::SetupPy,
        "project.godot" => ManifestKind::Godot,
        "CMakeLists.txt" => ManifestKind::Cmake,
        "compile_commands.json" => ManifestKind::CompileCommands,
        _ if lower.ends_with(".csproj") => ManifestKind::Csproj,
        _ if lower.ends_with(".sln") => ManifestKind::Sln,
        _ => return None,
    })
}

/// Parse a manifest. Never fails: problems end up in [`Manifest::error`].
pub fn parse(path: &str, kind: ManifestKind, content: &[u8]) -> Manifest {
    let text = String::from_utf8_lossy(content);
    let file_name = path.rsplit('/').next().unwrap_or(path);
    let result = match kind {
        ManifestKind::Cargo => parse_cargo(&text),
        ManifestKind::Npm => parse_npm(&text),
        ManifestKind::GoMod => Ok(parse_go_mod(&text)),
        ManifestKind::Maven => Ok(parse_maven(&text)),
        ManifestKind::Gradle => Ok((None, json!({ "kotlin_dsl": file_name.ends_with(".kts") }))),
        ManifestKind::Csproj => Ok(parse_csproj(file_name, &text)),
        ManifestKind::Sln => Ok(parse_sln(&text)),
        ManifestKind::Pyproject => parse_pyproject(&text),
        ManifestKind::Requirements => Ok(parse_requirements(&text)),
        ManifestKind::SetupPy => Ok((setup_py_name(&text), Value::Null)),
        ManifestKind::Godot => Ok(parse_godot(&text)),
        ManifestKind::Cmake => Ok(parse_cmake(&text)),
        ManifestKind::CompileCommands => Ok((None, Value::Null)),
    };
    let (name, details, error) = match result {
        Ok((name, details)) => (name, details, None),
        Err(e) => (None, Value::Null, Some(e)),
    };
    Manifest {
        path: path.to_string(),
        kind,
        name,
        details,
        error,
    }
}

type Parsed = (Option<String>, Value);

fn str_array(v: Option<&toml::Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_cargo(text: &str) -> Result<Parsed, String> {
    let t: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    let name = t
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_string);
    let mut details = serde_json::Map::new();
    if let Some(ws) = t.get("workspace") {
        details.insert(
            "workspace_members".into(),
            json!(str_array(ws.get("members"))),
        );
        details.insert(
            "virtual_workspace".into(),
            json!(t.get("package").is_none()),
        );
    }
    Ok((name, Value::Object(details)))
}

fn parse_npm(text: &str) -> Result<Parsed, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let name = v.get("name").and_then(Value::as_str).map(str::to_string);
    let workspaces = match v.get("workspaces") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Object(o)) => o
            .get("packages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut details = serde_json::Map::new();
    if !workspaces.is_empty() {
        details.insert("workspaces".into(), Value::Array(workspaces));
    }
    // The program a `package.json` runs (Electron's main process, a Node app): a flow
    // entry (see `symbols::resolve`).
    if let Some(main) = v.get("main").and_then(Value::as_str)
        && npm_program(&v, main)
    {
        details.insert("main".into(), Value::String(main.to_string()));
    }
    let entries = npm_entries(&v);
    if !entries.is_empty() {
        details.insert(
            "entries".into(),
            Value::Array(entries.into_iter().map(Value::String).collect()),
        );
    }
    Ok((name, Value::Object(details)))
}

/// Whether a `package.json` whose `main` is `main` describes a program rather than a
/// library: it has a `bin`, depends on Electron (its `main` is the main process), or a
/// script runs `main` (`node dist/index.js`) or Electron.
fn npm_program(v: &Value, main: &str) -> bool {
    let clean = |s: &str| s.trim_start_matches("./").replace('\\', "/");
    let main = clean(main);
    let stem = main
        .strip_suffix(".js")
        .or_else(|| main.strip_suffix(".cjs"))
        .or_else(|| main.strip_suffix(".mjs"))
        .unwrap_or(&main)
        .to_string();
    let electron = ["dependencies", "devDependencies"]
        .iter()
        .any(|k| v.get(k).and_then(|d| d.get("electron")).is_some());
    let runs = v
        .get("scripts")
        .and_then(Value::as_object)
        .is_some_and(|scripts| {
            scripts.values().filter_map(Value::as_str).any(|cmd| {
                cmd.split(|c: char| c.is_whitespace() || c == '"' || c == '\'')
                    .map(clean)
                    .any(|t| t == main || t == stem || t == "electron")
            })
        });
    v.get("bin").is_some() || electron || runs
}

/// Files a `package.json` runs or exposes, relative to its directory: `main`, `module`,
/// `browser`, `types`, `bin`, string leaves of `exports`, and the source files its `scripts`
/// name (`tsx drizzle/seed.ts`, `node scripts/x.mjs`). Nothing imports these; they are entry
/// points. Sorted, without duplicates. Kept as written: a path without extension or naming a
/// folder (`"main": "lib/server"`, `"bin": "./cli"`) is matched to files by the query
/// graph (`cartograph-query`'s `graph.rs`), which knows the indexed files.
fn npm_entries(v: &Value) -> Vec<String> {
    const EXTS: &[&str] = &[
        ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs", ".py",
    ];
    fn leaves(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => out.push(s.clone()),
            Value::Array(a) => a.iter().for_each(|x| leaves(x, out)),
            Value::Object(o) => o.values().for_each(|x| leaves(x, out)),
            _ => {}
        }
    }
    let mut raw = Vec::new();
    for key in [
        "main", "module", "browser", "types", "typings", "bin", "exports",
    ] {
        if let Some(x) = v.get(key) {
            leaves(x, &mut raw);
        }
    }
    if let Some(Value::Object(scripts)) = v.get("scripts") {
        for cmd in scripts.values().filter_map(Value::as_str) {
            for tok in cmd.split(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '=') {
                if EXTS.iter().any(|e| tok.ends_with(e)) && !tok.contains("://") {
                    raw.push(tok.to_string());
                }
            }
        }
    }
    let mut out: Vec<String> = raw
        .into_iter()
        .map(|s| {
            let s = s.replace('\\', "/");
            s.trim_start_matches("./").to_string()
        })
        .filter(|s| {
            !s.is_empty() && !s.starts_with('/') && !s.starts_with("..") && !s.contains('*')
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

fn parse_go_mod(text: &str) -> Parsed {
    let mut module = None;
    let mut go = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("module ") {
            module = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("go ") {
            go = Some(rest.trim().to_string());
        }
    }
    (module, json!({ "go": go }))
}

/// Remove every `<tag>…</tag>` block (non-nested).
fn strip_blocks(text: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        out.push_str(&rest[..start]);
        match rest[start..].find(&close) {
            Some(end) => rest = &rest[start + end + close.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Text of every `<tag>…</tag>` element (no attributes, non-nested).
fn tag_values(text: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else { break };
        out.push(after[..end].trim().to_string());
        rest = &after[end + close.len()..];
    }
    out
}

fn parse_maven(text: &str) -> Parsed {
    let mut own = text.to_string();
    for tag in [
        "parent",
        "dependencies",
        "dependencyManagement",
        "build",
        "profiles",
        "plugins",
    ] {
        own = strip_blocks(&own, tag);
    }
    let artifact = tag_values(&own, "artifactId").into_iter().next();
    let group = tag_values(&own, "groupId").into_iter().next();
    let modules = tag_values(text, "module");
    (artifact, json!({ "group_id": group, "modules": modules }))
}

/// Values of `attr="…"` on every element named `element`.
fn attr_values(text: &str, element: &str, attr: &str) -> Vec<String> {
    let open = format!("<{element}");
    let key = format!("{attr}=\"");
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        // `<Project` must not match `<ProjectReference`.
        let bounded = after
            .chars()
            .next()
            .is_none_or(|c| c.is_whitespace() || c == '>' || c == '/');
        if !bounded {
            rest = after;
            continue;
        }
        let tag_end = after.find('>').unwrap_or(after.len());
        let tag = &after[..tag_end];
        // The attribute name must start at a boundary too (`Sdk=` vs `MySdk=`).
        let mut search = tag;
        while let Some(k) = search.find(&key) {
            let at_boundary = search[..k]
                .chars()
                .next_back()
                .is_none_or(char::is_whitespace);
            let v = &search[k + key.len()..];
            if at_boundary {
                if let Some(q) = v.find('"') {
                    out.push(v[..q].to_string());
                }
                break;
            }
            search = v;
        }
        rest = &after[tag_end..];
    }
    out
}

fn parse_csproj(file_name: &str, text: &str) -> Parsed {
    let name = file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, _)| stem)
        .to_string();
    let sdk = attr_values(text, "Project", "Sdk").into_iter().next();
    let mut frameworks = tag_values(text, "TargetFramework");
    for multi in tag_values(text, "TargetFrameworks") {
        frameworks.extend(multi.split(';').map(|s| s.trim().to_string()));
    }
    let refs: Vec<String> = attr_values(text, "ProjectReference", "Include")
        .into_iter()
        .map(|p| p.replace('\\', "/"))
        .collect();
    (
        Some(name),
        json!({ "sdk": sdk, "target_frameworks": frameworks, "project_references": refs }),
    )
}

fn parse_sln(text: &str) -> Parsed {
    let mut projects = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with("Project(") {
            continue;
        }
        let Some((_, rhs)) = line.split_once('=') else {
            continue;
        };
        let parts: Vec<&str> = rhs.split(',').map(|s| s.trim().trim_matches('"')).collect();
        if let [name, path, ..] = parts.as_slice() {
            // Solution folders use the folder name as "path"; keep only real projects.
            if path.contains('.') {
                projects.push(json!({ "name": name, "path": path.replace('\\', "/") }));
            }
        }
    }
    (None, json!({ "projects": projects }))
}

fn parse_pyproject(text: &str) -> Result<Parsed, String> {
    let t: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    let name = t
        .get("project")
        .and_then(|p| p.get("name"))
        .or_else(|| {
            t.get("tool")
                .and_then(|x| x.get("poetry"))
                .and_then(|p| p.get("name"))
        })
        .and_then(|n| n.as_str())
        .map(str::to_string);
    Ok((name, Value::Null))
}

fn parse_requirements(text: &str) -> Parsed {
    let count = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('-'))
        .count();
    (None, json!({ "requirements": count }))
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Byte offsets of `word` in `text` where it is a whole identifier.
fn word_positions<'a>(text: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    text.match_indices(word).map(|(i, _)| i).filter(move |&i| {
        let before = text[..i].chars().next_back().is_none_or(|c| !is_ident(c));
        let after = text[i + word.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_ident(c));
        before && after
    })
}

/// The argument text of the first `setup(...)` call, up to its matching parenthesis.
fn setup_call_args(text: &str) -> Option<&str> {
    for i in word_positions(text, "setup") {
        let after = text[i + "setup".len()..].trim_start();
        let Some(args) = after.strip_prefix('(') else {
            continue;
        };
        let mut depth = 1usize;
        for (j, c) in args.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&args[..j]);
                    }
                }
                _ => {}
            }
        }
        return Some(args);
    }
    None
}

/// `name="..."` keyword argument of `setup(...)`.
fn setup_py_name(text: &str) -> Option<String> {
    let args = setup_call_args(text)?;
    for i in word_positions(args, "name") {
        let after = args[i + "name".len()..].trim_start();
        let Some(after) = after.strip_prefix('=') else {
            continue;
        };
        if after.starts_with('=') {
            continue;
        }
        let after = after.trim_start();
        if let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'') {
            let body = &after[1..];
            if let Some(end) = body.find(q) {
                return Some(body[..end].to_string());
            }
        }
    }
    None
}

fn parse_godot(text: &str) -> Parsed {
    let mut section = String::new();
    let mut name = None;
    let mut main_scene = None;
    let mut autoloads = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim().trim_matches('"'));
        match (section.as_str(), key) {
            ("application", "config/name") => name = Some(value.to_string()),
            ("application", "run/main_scene") => main_scene = Some(value.to_string()),
            ("autoload", _) => {
                let singleton = value.starts_with('*');
                autoloads.push(json!({
                    "name": key,
                    "path": value.trim_start_matches('*'),
                    "singleton": singleton,
                }));
            }
            _ => {}
        }
    }
    (
        name,
        json!({ "main_scene": main_scene, "autoloads": autoloads }),
    )
}

/// CMake source with `#` line comments removed (bracket comments are not handled).
fn strip_cmake_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let mut in_string = false;
        let mut prev = '\0';
        for c in line.chars() {
            if c == '"' && prev != '\\' {
                in_string = !in_string;
            } else if c == '#' && !in_string {
                break;
            }
            out.push(c);
            prev = c;
        }
        out.push('\n');
    }
    out
}

fn parse_cmake(text: &str) -> Parsed {
    let code = strip_cmake_comments(text);
    let lower = code.to_ascii_lowercase();
    // First argument of every call to `command` (case-insensitive, whole word,
    // whitespace allowed before the parenthesis).
    let arg_of = |command: &str| -> Vec<String> {
        let mut out = Vec::new();
        for i in word_positions(&lower, command) {
            let after = &code[i + command.len()..];
            let Some(args) = after.trim_start().strip_prefix('(') else {
                continue;
            };
            let args = &args[..args.find(')').unwrap_or(args.len())];
            if let Some(first) = args.split_whitespace().next() {
                out.push(first.trim_matches('"').to_string());
            }
        }
        out
    };
    let name = arg_of("project").into_iter().next();
    (
        name,
        json!({ "subdirectories": arg_of("add_subdirectory") }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(path: &str, text: &str) -> Manifest {
        parse(path, detect(path).expect("manifest"), text.as_bytes())
    }

    #[test]
    fn cargo_workspace() {
        let m = p(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n",
        );
        assert_eq!(m.name, None);
        assert_eq!(
            m.details["workspace_members"],
            json!(["crates/a", "crates/b"])
        );
        assert_eq!(m.details["virtual_workspace"], json!(true));
        let m = p("x/Cargo.toml", "[package]\nname = \"a\"\n");
        assert_eq!(m.name.as_deref(), Some("a"));
        let bad = p("Cargo.toml", "[package\n");
        assert!(bad.error.is_some());
    }

    #[test]
    fn npm_workspaces_both_forms() {
        let m = p("package.json", r#"{"name":"w","workspaces":["pkgs/*"]}"#);
        assert_eq!(m.name.as_deref(), Some("w"));
        assert_eq!(m.details["workspaces"], json!(["pkgs/*"]));
        let m = p("package.json", r#"{"workspaces":{"packages":["a"]}}"#);
        assert_eq!(m.details["workspaces"], json!(["a"]));
        assert!(p("package.json", "{").error.is_some());
    }

    #[test]
    fn npm_entries_from_main_bin_exports_and_scripts() {
        let m = p(
            "package.json",
            r#"{"main":"./dist/index.js","bin":{"x":"bin/x.mjs"},
                "exports":{".":{"import":"./src/index.ts"},"./util":"./src/util.ts","./*":"./src/*.ts"},
                "scripts":{"seed":"tsx drizzle/seed.ts --flag=a.ts","web":"vite","dl":"curl https://x.io/a.js",
                           "up":"node ../outside.js"}}"#,
        );
        assert_eq!(
            m.details["entries"],
            json!([
                "a.ts",
                "bin/x.mjs",
                "dist/index.js",
                "drizzle/seed.ts",
                "src/index.ts",
                "src/util.ts"
            ])
        );
        assert!(
            p("package.json", r#"{"name":"x"}"#)
                .details
                .get("entries")
                .is_none()
        );
    }

    #[test]
    fn go_maven_gradle_dotnet_python_cmake() {
        assert_eq!(
            p("go.mod", "module example.com/x\n\ngo 1.22\n")
                .name
                .as_deref(),
            Some("example.com/x")
        );
        let pom = "<project><parent><artifactId>parent</artifactId></parent>\
                   <groupId>g</groupId><artifactId>app</artifactId>\
                   <modules><module>core</module></modules>\
                   <dependencies><dependency><artifactId>dep</artifactId></dependency></dependencies></project>";
        let m = p("pom.xml", pom);
        assert_eq!(m.name.as_deref(), Some("app"));
        assert_eq!(m.details["modules"], json!(["core"]));
        assert_eq!(p("build.gradle.kts", "").details["kotlin_dsl"], json!(true));
        let m = p(
            "src/App.csproj",
            r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup>
               <ItemGroup><ProjectReference Include="..\Lib\Lib.csproj" /></ItemGroup></Project>"#,
        );
        assert_eq!(m.name.as_deref(), Some("App"));
        assert_eq!(
            m.details["project_references"],
            json!(["../Lib/Lib.csproj"])
        );
        let sln = "Project(\"{FAE04EC0}\") = \"App\", \"src\\App.csproj\", \"{1}\"\nEndProject\n\
                   Project(\"{2150E333}\") = \"Folder\", \"Folder\", \"{2}\"\nEndProject\n";
        assert_eq!(
            p("App.sln", sln).details["projects"],
            json!([{"name": "App", "path": "src/App.csproj"}])
        );
        assert_eq!(
            p("pyproject.toml", "[project]\nname = \"py\"\n")
                .name
                .as_deref(),
            Some("py")
        );
        assert_eq!(
            p("pyproject.toml", "[tool.poetry]\nname = \"po\"\n")
                .name
                .as_deref(),
            Some("po")
        );
        assert_eq!(
            p("setup.py", "setup(\n    name = 'legacy',\n)\n")
                .name
                .as_deref(),
            Some("legacy")
        );
        assert_eq!(
            p(
                "requirements.txt",
                "# c\nrequests\n-r other.txt\nflask==3\n"
            )
            .details["requirements"],
            json!(2)
        );
        let m = p(
            "CMakeLists.txt",
            "cmake_minimum_required(VERSION 3.20)\nPROJECT(engine CXX)\nadd_subdirectory(src)\n",
        );
        assert_eq!(m.name.as_deref(), Some("engine"));
        assert_eq!(m.details["subdirectories"], json!(["src"]));
        assert!(
            !detect("build/compile_commands.json")
                .unwrap()
                .declares_package()
        );
    }

    #[test]
    fn setup_py_needs_a_standalone_name_keyword_inside_setup() {
        let m = p(
            "setup.py",
            "package_name = 'wrong'\nsetup(\n    version='1',\n    name=\"right\",\n)\n",
        );
        assert_eq!(m.name.as_deref(), Some("right"));
        let m = p("setup.py", "setup(package_name='wrong', fullname='no')\n");
        assert_eq!(m.name, None);
        let m = p("setup.py", "setup(name == 'cmp')\n");
        assert_eq!(m.name, None);
    }

    #[test]
    fn csproj_element_names_need_a_boundary() {
        let m = p(
            "App.csproj",
            r#"<Project><ItemGroup><ProjectReference Include="a.csproj" Sdk="Fake" /></ItemGroup></Project>"#,
        );
        assert_eq!(m.details["sdk"], Value::Null);
        assert_eq!(m.details["project_references"], json!(["a.csproj"]));
    }

    #[test]
    fn cmake_calls_need_boundaries_and_skip_comments() {
        let text = "# project(commented)\nmy_project(wrong)\nproject (real CXX)\n\
                    add_subdirectory( lib ) # add_subdirectory(nope)\nmy_add_subdirectory(bad)\n";
        let m = p("CMakeLists.txt", text);
        assert_eq!(m.name.as_deref(), Some("real"));
        assert_eq!(m.details["subdirectories"], json!(["lib"]));
    }

    #[test]
    fn godot_project() {
        let text = "config_version=5\n\n[application]\n\nconfig/name=\"Demo\"\nrun/main_scene=\"res://main.tscn\"\n\n[autoload]\n\nGameState=\"*res://autoload/game_state.gd\"\n";
        let m = p("game/project.godot", text);
        assert_eq!(m.name.as_deref(), Some("Demo"));
        assert_eq!(m.dir(), "game");
        assert_eq!(
            m.details["autoloads"],
            json!([{"name": "GameState", "path": "res://autoload/game_state.gd", "singleton": true}])
        );
    }
}
