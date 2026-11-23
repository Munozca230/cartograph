//! Runnable tests for `impact`: how close each test is to the change, whether it is slow
//! (an integration or end-to-end test), and the command that runs the ones listed.
//!
//! Closeness: a test named after a changed file (`FooTest` or `FooServiceTest` for
//! `FooServiceImpl`, `foo.test.ts`, `test_foo.py`, `foo_test.go`), then the tests that
//! depend on it directly from its own package (or folder), then the other direct ones, then
//! those that reach it through more files. Files of a test tree that hold no test (helpers,
//! base classes) come last and are never in a command. Tests of a changed file's package
//! that no import reaches are named apart ([`same_package`]).
//!
//! Slow: what the test imports (`@SpringBootTest`, a Spring test slice such as
//! `@DataMongoTest`, Testcontainers, Playwright, Cypress), itself or through a base class
//! or helper of the test tree it imports; its name (`FooIT`, `FooIntegrationTest`,
//! `a.e2e.ts`); or its folder (`integration`, `it`, `e2e`).
//!
//! Commands, one per build root and speed: Maven (`./mvnw` where the wrapper is, else
//! `mvn`, from the top of the build; `-pl` for a module of a multi-module build), Gradle,
//! the JS runner `package.json` uses (vitest, jest, mocha; Playwright and Cypress for their
//! own tests), pytest, `go test` and `cargo test -p`. In a workspace a command runs from
//! its member's root, and says which member.

use crate::graph::{FileIx, Manifest, RepoGraph};
use crate::kind::{self, FileKind};
use serde::Serialize;

/// How close a test is to the change (lower first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Closeness {
    /// Named after a changed file.
    Named,
    /// Depends on it directly, from its package or folder.
    Package,
    /// Depends on it directly.
    Direct,
    /// Reaches it through other files.
    Reaches,
    /// Holds no test (a helper or base class of the test tree).
    Helper,
}

impl Closeness {
    /// What the answer says about it.
    pub(crate) fn note(self) -> Option<&'static str> {
        match self {
            Closeness::Named => Some("named after it"),
            Closeness::Helper => Some("no test in it"),
            _ => None,
        }
    }
}

/// A test to run, ranked.
#[derive(Debug, Clone)]
pub(crate) struct Ranked {
    pub file: FileIx,
    pub hops: u32,
    pub closeness: Closeness,
    /// Why it is slow (`@SpringBootTest`, `Playwright`, `integration folder`), if it is.
    pub slow: Option<String>,
}

/// A ready command that runs some of the tests listed.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TestCommand {
    /// `maven`, `gradle`, `vitest`, `jest`, `mocha`, `playwright`, `cypress`, `npm`,
    /// `pytest`, `go`, `cargo`.
    pub framework: String,
    /// The workspace member it runs in (from its root); `None` in a single repo (from the
    /// repo root).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// The command, with a `cd` first when it runs below that root.
    pub command: String,
    /// Tests it runs.
    pub tests: usize,
    /// It runs only slow tests (integration, end to end).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub slow: bool,
}

/// Rank `tests` (`(hops, file)`, every one a test file that reaches `targets`).
pub(crate) fn rank(g: &RepoGraph, targets: &[FileIx], tests: &[(u32, FileIx)]) -> Vec<Ranked> {
    let subjects: Vec<String> = targets
        .iter()
        .flat_map(|&t| subjects_of(&g.files[t as usize].name))
        .collect();
    let packages: Vec<String> = targets.iter().flat_map(|&t| package_dirs(g, t)).collect();
    let mut out: Vec<Ranked> = tests
        .iter()
        .map(|&(hops, f)| {
            let file = &g.files[f as usize];
            let closeness = if !holds_tests(g, f) {
                Closeness::Helper
            } else if test_subject(&file.name).is_some_and(|s| subjects.contains(&s)) {
                Closeness::Named
            } else if hops <= 1 && packages.contains(&g.dirs[file.dir as usize].path) {
                Closeness::Package
            } else if hops <= 1 {
                Closeness::Direct
            } else {
                Closeness::Reaches
            };
            Ranked {
                file: f,
                hops,
                closeness,
                slow: slow(g, f),
            }
        })
        .collect();
    out.sort_by(|a, b| {
        (a.closeness, a.hops, g.path(a.file)).cmp(&(b.closeness, b.hops, g.path(b.file)))
    });
    out
}

/// Tests of a changed file's package (its folder, or the test tree's mirror of it) that
/// `reached` leaves out: no import reaches the change from them, though they may still
/// exercise it.
pub(crate) fn same_package(
    g: &RepoGraph,
    targets: &[FileIx],
    reached: impl Fn(FileIx) -> bool,
) -> Vec<FileIx> {
    let mut out: Vec<FileIx> = Vec::new();
    for &t in targets {
        for d in package_dirs(g, t) {
            let Some(dir) = g.dir_by_path(&d) else {
                continue;
            };
            for &f in &g.dirs[dir as usize].files {
                if g.files[f as usize].kind == FileKind::Test
                    && !reached(f)
                    && holds_tests(g, f)
                    && !out.contains(&f)
                {
                    out.push(f);
                }
            }
        }
    }
    out.sort_by(|a, b| g.path(*a).cmp(g.path(*b)));
    out
}

/// The folder of `t` and its mirror in a test tree (`src/main/java/a` → `src/test/java/a`).
fn package_dirs(g: &RepoGraph, t: FileIx) -> Vec<String> {
    let dir = g.dirs[g.files[t as usize].dir as usize].path.clone();
    let mut out = vec![dir.clone()];
    let segs: Vec<&str> = dir.split('/').collect();
    if let Some(i) = segs.iter().position(|s| *s == "main") {
        let mut m = segs.clone();
        m[i] = "test";
        out.push(m.join("/"));
    }
    out
}

/// Lower-case names a test of `name` may be named after: its stem, and without `Impl`.
fn subjects_of(name: &str) -> Vec<String> {
    let stem = name.split('.').next().unwrap_or(name).to_lowercase();
    let mut out = vec![stem.clone()];
    if let Some(s) = stem.strip_suffix("impl")
        && !s.is_empty()
    {
        out.push(s.to_string());
    }
    out
}

/// What a test file is named after, lower case: `FooTest.java`, `TestFoo.java`,
/// `FooIT.java` → `foo`; `foo.test.ts` → `foo`; `test_foo.py`, `foo_test.go` → `foo`.
fn test_subject(name: &str) -> Option<String> {
    let mut parts = name.split('.');
    let stem = parts.next()?;
    let has_marker = name
        .split('.')
        .rev()
        .nth(1)
        .is_some_and(|m| matches!(m, "test" | "spec" | "e2e"));
    let s = if has_marker && name.split('.').count() >= 3 {
        stem
    } else if let Some(s) = stem.strip_prefix("test_") {
        s
    } else if let Some(s) = stem.strip_suffix("_test") {
        s
    } else if let Some(s) = ["IntegrationTest", "Tests", "Test", "ITCase", "IT"]
        .iter()
        .find_map(|x| stem.strip_suffix(x))
    {
        s
    } else {
        stem.strip_prefix("Test")
            .filter(|s| s.starts_with(|c: char| c.is_ascii_uppercase()))?
    };
    (!s.is_empty()).then(|| s.to_lowercase())
}

/// Imports of a Java or Kotlin file that declare tests (`@Test` of JUnit or TestNG).
fn imports_test_api(g: &RepoGraph, f: FileIx) -> bool {
    g.sites[f as usize].iter().any(|s| {
        let spec = s.specifier.trim_start_matches("static ");
        (spec.starts_with("org.junit.") || spec.starts_with("org.testng."))
            && matches!(
                spec.rsplit('.').next(),
                Some(
                    "Test"
                        | "ParameterizedTest"
                        | "RepeatedTest"
                        | "TestFactory"
                        | "TestTemplate"
                        | "*"
                )
            )
    })
}

/// Whether a test file holds tests a runner picks up (not a helper of the test tree).
pub(crate) fn holds_tests(g: &RepoGraph, f: FileIx) -> bool {
    let file = &g.files[f as usize];
    let path = g.path(f);
    match file.lang.as_deref() {
        Some("java" | "kotlin") => imports_test_api(g, f) || kind::test_named(&file.name),
        Some("rust") => {
            // An integration test is a file right in a crate's `tests/`.
            let dir = &g.dirs[file.dir as usize];
            kind::test_named(&file.name) || (dir.name == "tests" && file.name.ends_with(".rs"))
        }
        Some("javascript" | "typescript" | "tsx" | "jsx") => {
            kind::test_named(&file.name) || path.split('/').any(|d| d == "__tests__")
        }
        _ => kind::test_named(&file.name),
    }
}

/// What makes an import mark a test slow.
fn import_marker(spec: &str) -> Option<String> {
    let spec = spec.trim_start_matches("static ");
    let last = spec.rsplit(['.', '/']).next().unwrap_or(spec);
    if spec == "org.springframework.boot.test.context.SpringBootTest" {
        return Some("@SpringBootTest".into());
    }
    if spec.starts_with("org.springframework.boot.test.autoconfigure.")
        && last.ends_with("Test")
        && last.starts_with(|c: char| c.is_ascii_uppercase())
    {
        return Some(format!("@{last}"));
    }
    if spec.starts_with("org.testcontainers")
        || spec == "testcontainers"
        || spec.starts_with("testcontainers.")
        || spec.starts_with("@testcontainers/")
    {
        return Some("Testcontainers".into());
    }
    if spec == "@playwright/test" || spec == "playwright" || spec.starts_with("playwright.") {
        return Some("Playwright".into());
    }
    if spec == "cypress" {
        return Some("Cypress".into());
    }
    None
}

/// Folders whose tests are slow, and what they are called.
const SLOW_DIRS: &[(&str, &str)] = &[
    ("integration", "integration folder"),
    ("integration-test", "integration folder"),
    ("integration-tests", "integration folder"),
    ("integration_tests", "integration folder"),
    ("integrationtest", "integration folder"),
    ("it", "integration folder"),
    ("e2e", "e2e folder"),
    ("cypress", "e2e folder"),
    ("playwright", "e2e folder"),
];

/// Why `f` itself is slow: its imports, then its name, then its folder.
fn own_slow(g: &RepoGraph, f: FileIx) -> Option<String> {
    if let Some(m) = g.sites[f as usize]
        .iter()
        .find_map(|s| import_marker(&s.specifier))
    {
        return Some(m);
    }
    let file = &g.files[f as usize];
    let stem = file.name.split('.').next().unwrap_or("");
    if kind::integration_named(&file.name)
        || stem.ends_with("IntegrationTest")
        || stem.ends_with("IntegrationTests")
        || stem.ends_with("ITCase")
    {
        return Some("integration test (name)".into());
    }
    if file.name.split('.').any(|p| p == "e2e") {
        return Some("e2e (name)".into());
    }
    let path = g.path(f);
    let below = match g.members.member_of(path) {
        Some(m) => &path[m.len() + 1..],
        None => path,
    };
    let dirs: Vec<String> = below
        .split('/')
        .rev()
        .skip(1)
        .map(str::to_ascii_lowercase)
        .collect();
    SLOW_DIRS
        .iter()
        .find(|(d, _)| dirs.iter().any(|x| x == d))
        .map(|(_, why)| why.to_string())
}

/// Why `f` is slow: [`own_slow`], else an import marker of a file of the test tree it
/// depends on (a base class, a helper), at most two steps away.
fn slow(g: &RepoGraph, f: FileIx) -> Option<String> {
    if let Some(m) = own_slow(g, f) {
        return Some(m);
    }
    let mut frontier = vec![f];
    let mut seen = vec![f];
    for _ in 0..2 {
        let mut next = Vec::new();
        for &u in &frontier {
            for d in &g.out[u as usize] {
                let v = d.file;
                if seen.contains(&v) || g.files[v as usize].kind != FileKind::Test {
                    continue;
                }
                seen.push(v);
                if let Some(m) = g.sites[v as usize]
                    .iter()
                    .find_map(|s| import_marker(&s.specifier))
                {
                    let name = &g.files[v as usize].name;
                    let stem = name.split('.').next().unwrap_or(name);
                    return Some(format!("{m} (via {stem})"));
                }
                next.push(v);
            }
        }
        frontier = next;
    }
    None
}

fn parent(dir: &str) -> Option<&str> {
    if dir.is_empty() {
        None
    } else {
        Some(dir.rsplit_once('/').map_or("", |(p, _)| p))
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// `path` relative to `dir` (an ancestor of it, or itself).
fn rel<'p>(path: &'p str, dir: &str) -> &'p str {
    if dir.is_empty() {
        path
    } else if path == dir {
        ""
    } else {
        path.strip_prefix(dir)
            .and_then(|r| r.strip_prefix('/'))
            .unwrap_or(path)
    }
}

/// The folders from `dir` up to `top` (both included), nearest first.
fn ancestors<'a>(dir: &'a str, top: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut d = Some(dir);
    while let Some(x) = d {
        out.push(x);
        if x == top {
            break;
        }
        d = parent(x);
    }
    out
}

fn manifest<'g>(g: &'g RepoGraph, dir: &str, kinds: &[&str]) -> Option<&'g Manifest> {
    g.manifests
        .iter()
        .find(|m| m.dir == dir && kinds.contains(&m.kind.as_str()))
}

/// Where a command runs and what it runs a test with.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tool {
    /// `run`: where the build runs from; `bin`: `./mvnw` or `mvn`.
    Maven {
        run: String,
        bin: &'static str,
    },
    Gradle {
        run: String,
        bin: &'static str,
        project: String,
    },
    /// A JS runner (`vitest`…, or `npm` when none is known), from the package's folder.
    Js {
        run: String,
        runner: String,
    },
    Pytest {
        run: String,
    },
    Go {
        run: String,
    },
    Cargo {
        run: String,
        krate: String,
    },
}

impl Tool {
    fn framework(&self) -> &str {
        match self {
            Tool::Maven { .. } => "maven",
            Tool::Gradle { .. } => "gradle",
            Tool::Js { runner, .. } => runner,
            Tool::Pytest { .. } => "pytest",
            Tool::Go { .. } => "go",
            Tool::Cargo { .. } => "cargo",
        }
    }

    fn run_dir(&self) -> &str {
        match self {
            Tool::Maven { run, .. }
            | Tool::Gradle { run, .. }
            | Tool::Js { run, .. }
            | Tool::Pytest { run }
            | Tool::Go { run }
            | Tool::Cargo { run, .. } => run,
        }
    }
}

/// How the test `f` runs, if its build is known.
fn tool_of(g: &RepoGraph, f: FileIx) -> Option<(Tool, String)> {
    let path = g.path(f);
    let file = &g.files[f as usize];
    let dir = g.dirs[file.dir as usize].path.as_str();
    let top = g.members.member_of(path).unwrap_or("");
    let up = ancestors(dir, top);
    let find = |kinds: &[&str]| up.iter().find_map(|d| manifest(g, d, kinds));
    let topmost = |kinds: &[&str]| up.iter().rev().find_map(|d| manifest(g, d, kinds));
    match file.lang.as_deref() {
        Some("java" | "kotlin" | "groovy" | "scala") => {
            let stem = file
                .name
                .split('.')
                .next()
                .unwrap_or(&file.name)
                .to_string();
            if let Some(module) = find(&["maven"]) {
                let root = topmost(&["maven"]).map_or(module.dir.as_str(), |m| m.dir.as_str());
                let wrapper = ancestors(&module.dir, top)
                    .into_iter()
                    .find(|d| g.file_by_path(&join(d, "mvnw")).is_some());
                let (run, bin) = match wrapper {
                    Some(w) => (w.to_string(), "./mvnw"),
                    None => (root.to_string(), "mvn"),
                };
                let module = rel(&module.dir, &run).to_string();
                return Some((Tool::Maven { run, bin }, format!("{module}\t{stem}")));
            }
            if let Some(project) = find(&["gradle"]) {
                let root = ancestors(&project.dir, top)
                    .into_iter()
                    .rev()
                    .find(|d| {
                        ["settings.gradle", "settings.gradle.kts", "gradlew"]
                            .iter()
                            .any(|n| g.file_by_path(&join(d, n)).is_some())
                    })
                    .unwrap_or(project.dir.as_str())
                    .to_string();
                let bin = if g.file_by_path(&join(&root, "gradlew")).is_some() {
                    "./gradlew"
                } else {
                    "gradle"
                };
                let sub = rel(&project.dir, &root);
                let project = if sub.is_empty() {
                    String::new()
                } else {
                    format!(":{}:", sub.replace('/', ":"))
                };
                return Some((
                    Tool::Gradle {
                        run: root,
                        bin,
                        project,
                    },
                    stem,
                ));
            }
            None
        }
        Some("javascript" | "typescript" | "tsx" | "jsx") => {
            let imports = |p: &str| g.sites[f as usize].iter().any(|s| s.specifier == p);
            let nearest = find(&["npm"])?;
            let own = if imports("@playwright/test") {
                Some("playwright")
            } else if imports("cypress") || path.split('/').any(|d| d == "cypress") {
                Some("cypress")
            } else {
                None
            };
            let (run, runner) = match own {
                Some(r) => {
                    // The package that declares it, else the nearest one.
                    let at = up
                        .iter()
                        .filter_map(|d| manifest(g, d, &["npm"]))
                        .find(|m| m.test_runners.iter().any(|x| x == r))
                        .unwrap_or(nearest);
                    (at.dir.clone(), r.to_string())
                }
                None => {
                    let declared =
                        up.iter()
                            .filter_map(|d| manifest(g, d, &["npm"]))
                            .find_map(|m| {
                                ["vitest", "jest", "mocha"]
                                    .iter()
                                    .find(|r| m.test_runners.iter().any(|x| x == *r))
                                    .map(|r| (m.dir.clone(), r.to_string()))
                            });
                    let scripted = || {
                        up.iter()
                            .filter_map(|d| manifest(g, d, &["npm"]))
                            .find(|m| m.test_runners.iter().any(|x| x == "npm"))
                            .map(|m| (m.dir.clone(), "npm".to_string()))
                    };
                    match declared {
                        Some(d) => d,
                        None if imports("vitest") => (nearest.dir.clone(), "vitest".into()),
                        // `npm test` only where a `test` script is.
                        None => scripted()?,
                    }
                }
            };
            let arg = rel(path, &run).to_string();
            Some((Tool::Js { run, runner }, arg))
        }
        Some("python") => {
            let run = find(&["pyproject", "setup-py", "requirements"])
                .map_or(top.to_string(), |m| m.dir.clone());
            let arg = rel(path, &run).to_string();
            Some((Tool::Pytest { run }, arg))
        }
        Some("go") => {
            let m = find(&["go-mod"])?;
            let pkg = rel(dir, &m.dir);
            let arg = if pkg.is_empty() {
                ".".to_string()
            } else {
                format!("./{pkg}")
            };
            Some((Tool::Go { run: m.dir.clone() }, arg))
        }
        Some("rust") => {
            let m = find(&["cargo"])?;
            let krate = m.name.clone()?;
            // A file right in the crate's `tests/` is a test target of its own.
            let target = match rel(path, &m.dir).strip_prefix("tests/") {
                Some(t) if !t.contains('/') => t.trim_end_matches(".rs").to_string(),
                _ => String::new(),
            };
            Some((
                Tool::Cargo {
                    run: top.to_string(),
                    krate,
                },
                target,
            ))
        }
        _ => None,
    }
}

/// Test functions of a Go test file (`TestX`), from the symbol layer.
fn go_tests(symbols: &crate::symbols::SymbolTable, path: &str) -> Vec<String> {
    let mut out: Vec<String> = symbols
        .of_file(path)
        .iter()
        .map(|&i| &symbols.symbols[i])
        .filter(|s| {
            s.container.is_none()
                && s.name.starts_with("Test")
                && s.name[4..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_ascii_lowercase())
        })
        .map(|s| s.name.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Tests that run with one command: `(tool, slow)`, the arguments, the files.
type Group = ((Tool, bool), Vec<String>, Vec<FileIx>);

/// The commands that run `tests` (helpers left out), fast ones first: one per build root
/// and runner, slow tests in their own.
pub(crate) fn commands(
    g: &RepoGraph,
    symbols: &crate::symbols::SymbolTable,
    tests: &[Ranked],
) -> Vec<TestCommand> {
    // (tool, slow) → arguments, in the order first met.
    let mut groups: Vec<Group> = Vec::new();
    for slow_pass in [false, true] {
        for t in tests {
            if t.closeness == Closeness::Helper || t.slow.is_some() != slow_pass {
                continue;
            }
            let Some((tool, arg)) = tool_of(g, t.file) else {
                continue;
            };
            let key = (tool, slow_pass);
            match groups.iter_mut().find(|x| x.0 == key) {
                Some(x) => {
                    if !x.1.contains(&arg) {
                        x.1.push(arg);
                    }
                    x.2.push(t.file);
                }
                None => groups.push((key, vec![arg], vec![t.file])),
            }
        }
    }
    groups
        .into_iter()
        .map(|((tool, slow), args, files)| {
            let body = match &tool {
                Tool::Maven { bin, .. } => {
                    let mut modules: Vec<&str> = Vec::new();
                    let mut classes: Vec<&str> = Vec::new();
                    for a in &args {
                        let (m, c) = a.split_once('\t').unwrap_or(("", a));
                        if !m.is_empty() && !modules.contains(&m) {
                            modules.push(m);
                        }
                        if !classes.contains(&c) {
                            classes.push(c);
                        }
                    }
                    let mut s = format!("{bin} test");
                    if !modules.is_empty() {
                        s.push_str(&format!(" -pl {}", modules.join(",")));
                    }
                    s.push_str(&format!(" -Dtest={}", classes.join(",")));
                    if modules.len() > 1 {
                        s.push_str(" -Dsurefire.failIfNoSpecifiedTests=false");
                    }
                    s
                }
                Tool::Gradle { bin, project, .. } => {
                    let tests: Vec<String> = args.iter().map(|a| format!("--tests {a}")).collect();
                    format!("{bin} {project}test {}", tests.join(" "))
                }
                Tool::Js { runner, .. } => {
                    let files = args.join(" ");
                    match runner.as_str() {
                        "vitest" => format!("npx vitest run {files}"),
                        "jest" => format!("npx jest {files}"),
                        "mocha" => format!("npx mocha {files}"),
                        "playwright" => format!("npx playwright test {files}"),
                        "cypress" => format!("npx cypress run --spec {}", args.join(",")),
                        _ => format!("npm test -- {files}"),
                    }
                }
                Tool::Pytest { .. } => format!("pytest {}", args.join(" ")),
                Tool::Go { .. } => {
                    let names: Vec<String> = files
                        .iter()
                        .flat_map(|&f| go_tests(symbols, g.path(f)))
                        .collect();
                    let mut s = format!("go test {}", args.join(" "));
                    if !names.is_empty() {
                        s.push_str(&format!(" -run '^({})$'", names.join("|")));
                    }
                    s
                }
                Tool::Cargo { krate, .. } => {
                    let mut s = format!("cargo test -p {krate}");
                    // Only test targets: name each; a unit test runs with the crate.
                    if args.iter().all(|a| !a.is_empty()) {
                        for a in &args {
                            s.push_str(&format!(" --test {a}"));
                        }
                    }
                    s
                }
            };
            let run = tool.run_dir();
            let member = g.members.member_of(run).map(str::to_string);
            let below = match &member {
                Some(m) => rel(run, m),
                None => run,
            };
            let command = if below.is_empty() {
                body
            } else {
                format!("cd {below} && {body}")
            };
            TestCommand {
                framework: tool.framework().to_string(),
                member,
                command,
                tests: files.len(),
                slow,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_test_is_named_after() {
        for (name, want) in [
            ("FooServiceImplTest.java", Some("fooserviceimpl")),
            ("FooServiceTest.java", Some("fooservice")),
            ("FooTests.java", Some("foo")),
            ("FooIT.java", Some("foo")),
            ("FooIntegrationTest.java", Some("foo")),
            ("TestFoo.java", Some("foo")),
            ("foo.test.ts", Some("foo")),
            ("foo.spec.tsx", Some("foo")),
            ("test_foo.py", Some("foo")),
            ("foo_test.go", Some("foo")),
            ("Testing.java", None),
            ("helpers.ts", None),
            ("Test.java", None),
        ] {
            assert_eq!(test_subject(name).as_deref(), want, "{name}");
        }
        assert_eq!(
            subjects_of("FooServiceImpl.java"),
            ["fooserviceimpl", "fooservice"]
        );
        assert_eq!(subjects_of("Impl.java"), ["impl"]);
    }

    #[test]
    fn imports_that_make_a_test_slow() {
        for (spec, want) in [
            (
                "org.springframework.boot.test.context.SpringBootTest",
                Some("@SpringBootTest"),
            ),
            (
                "org.springframework.boot.test.autoconfigure.data.mongo.DataMongoTest",
                Some("@DataMongoTest"),
            ),
            (
                "org.springframework.boot.test.autoconfigure.web.servlet.AutoConfigureMockMvc",
                None,
            ),
            (
                "org.testcontainers.containers.MongoDBContainer",
                Some("Testcontainers"),
            ),
            ("@playwright/test", Some("Playwright")),
            ("cypress", Some("Cypress")),
            ("org.junit.jupiter.api.Test", None),
            ("vitest", None),
        ] {
            assert_eq!(import_marker(spec).as_deref(), want, "{spec}");
        }
    }

    #[test]
    fn paths_relative_to_a_folder() {
        assert_eq!(rel("a/b/c.ts", "a"), "b/c.ts");
        assert_eq!(rel("a/b", "a/b"), "");
        assert_eq!(rel("a/b", ""), "a/b");
        assert_eq!(ancestors("a/b/c", "a"), ["a/b/c", "a/b", "a"]);
        assert_eq!(ancestors("a/b", ""), ["a/b", "a", ""]);
    }
}
