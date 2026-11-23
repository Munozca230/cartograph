//! Java import resolution.
//!
//! **Index.** Every `.java` file is indexed by what it declares: its `package` and its
//! top-level types (`package.Type`), plus `package.<file stem>` in case the types did not
//! parse. Source roots never have to be guessed, so `src/main/java`, `src/test/java`,
//! Maven multi-module (`<modules>`), Gradle conventions and odd layouts all work the same.
//! Module roots (dirs with `pom.xml`, `build.gradle` or `build.gradle.kts`) and the
//! `src/test/` source set only break ties between files declaring the same name.
//!
//! **Order** for `import [static] N[.*]` from file `f`:
//! 1. Non-static `a.b.*` where `a.b` is a declared package → its files, as the package's
//!    Dir when every `.java` file of that dir declares the package (one site per dir), or
//!    as files otherwise. Only the dirs in `f`'s module when the package has files there,
//!    and only main sources when `f` is main code (not under `src/test/`).
//! 2. The longest prefix of `N` that is a declared type → its file (`Resolved`). This
//!    covers nested classes (`a.b.Outer.Inner` → `Outer.java`), static members
//!    (`static a.b.C.m`) and static wildcards (`static a.b.C.*`). Several files → the ones
//!    in `f`'s module, then in `f`'s source set (main or test); still several →
//!    `ambiguous`.
//! 3. JDK packages (`java.*`, the JDK's `javax.*`, `jdk.*`, `sun.*`, the JDK's `com.sun.*`,
//!    `org.w3c.dom`, `org.xml.sax`, `org.ietf.jgss`) → External `jdk:<two segments>`
//!    (`jdk:java.util`), `Exact`.
//! 4. The package is declared in the repo but the class is missing → `not_found` (the
//!    detail names the file when the walker skipped it, e.g. as binary).
//! 5. The longest dependency `groupId` (from every `pom.xml` `<dependency>`, a `<parent>`
//!    that is not the project's own, and `group:artifact[:version]` strings of Gradle
//!    build files) prefixing the package →
//!    External `maven:<groupId>`, `Resolved`.
//! 6. The package starts with the project's own `groupId` / Gradle `group` → `not_found`
//!    (local code not in the index, e.g. generated sources under the excluded `target/`).
//! 7. Otherwise External `maven:<guess>`, `Heuristic`: the first two segments when the
//!    first is a domain-like prefix (`com`, `org`, `io`, `jakarta`, `javax`…), else the
//!    first (`lombok`).
//!
//! The package of `N` is its segments up to the first capitalised one (`a.b.C.m` → `a.b`),
//! or all but the last when none is capitalised.

use super::{Index, Outcome, Target, is_ancestor, parent};
use crate::imports::Spec;
use cartograph_core::Confidence;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Immutable Java resolver (shared across threads).
pub struct Resolver {
    /// Fully qualified type name → declaring files (sorted).
    types: HashMap<String, Vec<String>>,
    /// The same, only types a file declares (not the `<file stem>` fallback).
    declared: HashMap<String, Vec<String>>,
    /// Package → files declaring it (sorted).
    packages: HashMap<String, Vec<String>>,
    /// File → declared package.
    package_of: HashMap<String, String>,
    /// Dir → `.java` files in it.
    dir_files: HashMap<String, Vec<String>>,
    /// Module roots, deepest first.
    module_roots: Vec<String>,
    /// Dependency groupIds, longest first.
    dep_groups: Vec<String>,
    /// The project's own groupIds.
    own_groups: Vec<String>,
    /// Skipped `.java` files and why (to explain a missing class).
    skipped: Vec<(String, String)>,
}

const BUILD_FILES: [&str; 3] = ["pom.xml", "build.gradle", "build.gradle.kts"];

const JDK_JAVAX: [&str; 17] = [
    "javax.accessibility",
    "javax.annotation.processing",
    "javax.crypto",
    "javax.imageio",
    "javax.lang.model",
    "javax.management",
    "javax.naming",
    "javax.net",
    "javax.print",
    "javax.rmi",
    "javax.script",
    "javax.security",
    "javax.sound",
    "javax.sql",
    "javax.swing",
    "javax.tools",
    "javax.xml",
];

const JDK_OTHER: &[&str] = &[
    "java",
    "jdk",
    "sun",
    // `com.sun.*` packages of the JDK only: `com.sun.mail`, `com.sun.jna` and
    // `com.sun.xml.bind` are third-party.
    "com.sun.net.httpserver",
    "com.sun.management",
    "com.sun.security",
    "com.sun.jdi",
    "com.sun.tools",
    "com.sun.source",
    "com.sun.nio",
    "com.sun.crypto",
    "com.sun.jndi",
    "com.sun.java",
    "com.sun.org.apache",
    "com.sun.rowset",
    "com.sun.imageio",
    "com.sun.media.sound",
    "com.sun.jarsigner",
    "com.sun.javadoc",
    "com.sun.net.ssl",
    "com.sun.jmx",
    "org.w3c.dom",
    "org.xml.sax",
    "org.ietf.jgss",
];

const DOMAIN_LIKE: [&str; 24] = [
    "com", "org", "net", "io", "edu", "gov", "dev", "app", "co", "me", "info", "biz", "de", "fr",
    "uk", "nl", "ch", "es", "it", "br", "ru", "jp", "javax", "jakarta",
];

/// Whether dotted `name` is `prefix` or starts with `prefix.`.
fn has_prefix(name: &str, prefix: &str) -> bool {
    !prefix.is_empty()
        && (name == prefix
            || (name.len() > prefix.len()
                && name.starts_with(prefix)
                && name.as_bytes()[prefix.len()] == b'.'))
}

/// Package part of an imported name (see the module docs).
fn package_part(name: &str) -> String {
    let segs: Vec<&str> = name.split('.').collect();
    let cut = segs
        .iter()
        .position(|s| s.starts_with(|c: char| c.is_ascii_uppercase()))
        .unwrap_or(segs.len().saturating_sub(1));
    segs[..cut].join(".")
}

fn first_segments(name: &str, n: usize) -> String {
    name.split('.').take(n).collect::<Vec<_>>().join(".")
}

fn is_test_path(path: &str) -> bool {
    path.starts_with("src/test/") || path.contains("/src/test/")
}

/// Values of `<tag>…</tag>` (no attributes) in `text`.
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

/// `(dependency groups, parent groups, own group)` of a pom.xml.
fn pom_groups(text: &str) -> (Vec<String>, Vec<String>, Option<String>) {
    let first_group = |blocks: Vec<String>| -> Vec<String> {
        blocks
            .iter()
            .flat_map(|b| tag_values(b, "groupId").into_iter().take(1))
            .collect()
    };
    let deps = first_group(tag_values(text, "dependency"));
    let parents = first_group(tag_values(text, "parent"));
    let own = crate::manifest::parse(
        "pom.xml",
        crate::manifest::ManifestKind::Maven,
        text.as_bytes(),
    )
    .details
    .get("group_id")
    .and_then(|g| g.as_str())
    .map(str::to_string);
    (deps, parents, own)
}

/// `(dependency groups, own group)` of a Gradle build file: every quoted
/// `group:artifact[:version]` string, `group: 'x'` map notation, and `group = 'x'`.
fn gradle_groups(text: &str) -> (Vec<String>, Option<String>) {
    let mut deps = Vec::new();
    let mut own = None;
    for line in text.lines() {
        let line = line.trim();
        let mut quoted = Vec::new();
        let mut rest = line;
        while let Some(start) = rest.find(['\'', '"']) {
            let q = rest.as_bytes()[start] as char;
            let after = &rest[start + 1..];
            let Some(end) = after.find(q) else { break };
            quoted.push(&after[..end]);
            rest = &after[end + 1..];
        }
        if let Some(stripped) = line.strip_prefix("group")
            && let Some(v) = quoted.first()
        {
            let s = stripped.trim_start();
            if s.starts_with('=') {
                own = Some(v.to_string());
                continue;
            }
        }
        if line.contains("group:")
            && let Some(v) = line
                .split("group:")
                .nth(1)
                .and_then(|r| r.split(['\'', '"']).nth(1))
        {
            deps.push(v.to_string());
        }
        for q in quoted {
            let parts: Vec<&str> = q.split(':').collect();
            let ok = |s: &str| {
                !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            };
            if (parts.len() == 2 || parts.len() == 3)
                && ok(parts[0])
                && ok(parts[1])
                && parts[0].contains('.')
            {
                deps.push(parts[0].to_string());
            }
        }
    }
    (deps, own)
}

impl Resolver {
    pub fn new(ix: &Index<'_>, files: &[super::FileImports<'_>]) -> Self {
        let mut types: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut declared: HashMap<String, Vec<String>> = HashMap::new();
        let mut packages: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut package_of = HashMap::new();
        let mut declared_somewhere: BTreeSet<&str> = BTreeSet::new();
        for f in files.iter().filter(|f| f.lang == "java") {
            declared_somewhere.insert(f.path);
            let pkg = f
                .imports
                .iter()
                .find_map(|r| match &r.spec {
                    Spec::Declares(p) => Some(p.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            for r in f.imports {
                if let Spec::DeclaresType(t) = &r.spec {
                    types
                        .entry(t.clone())
                        .or_default()
                        .insert(f.path.to_string());
                    declared
                        .entry(t.clone())
                        .or_default()
                        .push(f.path.to_string());
                }
            }
            add_file(&mut types, &mut packages, &mut package_of, f.path, pkg);
        }
        // Files with nothing extracted (an empty file, or one with only comments) are not
        // in `files`; they still declare `<stem>` in the default package.
        for f in ix.files.iter().filter(|f| f.ends_with(".java")) {
            if !declared_somewhere.contains(*f) {
                add_file(&mut types, &mut packages, &mut package_of, f, String::new());
            }
        }
        let mut dir_files: HashMap<String, Vec<String>> = HashMap::new();
        for f in package_of.keys() {
            dir_files
                .entry(parent(f).to_string())
                .or_default()
                .push(f.clone());
        }
        let mut module_roots = Vec::new();
        let mut dep_groups = BTreeSet::new();
        let mut parent_groups = BTreeSet::new();
        let mut own_groups = BTreeSet::new();
        let mut build_files: Vec<&str> = ix
            .files
            .iter()
            .copied()
            .filter(|f| BUILD_FILES.contains(&f.rsplit('/').next().unwrap_or(f)))
            .collect();
        build_files.sort_unstable();
        for b in build_files {
            module_roots.push(parent(b).to_string());
            let Some(text) = ix.read_config(b) else {
                continue;
            };
            let (deps, parents, own) = if b.ends_with("pom.xml") {
                pom_groups(&text)
            } else {
                let (deps, own) = gradle_groups(&text);
                (deps, Vec::new(), own)
            };
            dep_groups.extend(deps.into_iter().filter(|g| !g.is_empty()));
            parent_groups.extend(parents.into_iter().filter(|g| !g.is_empty()));
            own_groups.extend(own.filter(|g| !g.is_empty()));
        }
        module_roots.sort_by(|a, b| super::depth(b).cmp(&super::depth(a)).then(a.cmp(b)));
        module_roots.dedup();
        // A parent pom is a dependency (`spring-boot-starter-parent`) unless it is the
        // project's own parent (a groupId some pom of the repo declares as its own).
        dep_groups.extend(
            parent_groups
                .into_iter()
                .filter(|g| !own_groups.contains(g)),
        );
        let mut dep_groups: Vec<String> = dep_groups.into_iter().collect();
        dep_groups.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        for v in declared.values_mut() {
            v.sort_unstable();
            v.dedup();
        }
        Resolver {
            types: types
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().collect()))
                .collect(),
            declared,
            packages: packages
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().collect()))
                .collect(),
            package_of,
            dir_files,
            module_roots,
            dep_groups,
            own_groups: own_groups.into_iter().collect(),
            skipped: {
                let mut v: Vec<(String, String)> = ix
                    .skipped
                    .iter()
                    .filter(|(p, _)| p.ends_with(".java"))
                    .map(|(p, r)| (p.to_string(), r.as_str().to_string()))
                    .collect();
                v.sort();
                v
            },
        }
    }

    fn module_of(&self, path: &str) -> Option<&str> {
        self.module_roots
            .iter()
            .find(|r| is_ancestor(r, parent(path)))
            .map(String::as_str)
    }

    /// Narrow `cands` to the importer's module, then to its source set.
    fn prefer<'c>(&self, importer: &str, cands: &'c [String]) -> Vec<&'c String> {
        let mut v: Vec<&String> = cands.iter().collect();
        let module = self.module_of(importer);
        let same: Vec<&String> = v
            .iter()
            .copied()
            .filter(|c| self.module_of(c) == module)
            .collect();
        if !same.is_empty() {
            v = same;
        }
        let test = is_test_path(importer);
        let same_set: Vec<&String> = v
            .iter()
            .copied()
            .filter(|c| is_test_path(c) == test)
            .collect();
        if !same_set.is_empty() {
            v = same_set;
        }
        v
    }

    /// Wildcard import of a declared package.
    fn package_targets(&self, importer: &str, pkg: &str, files: &[String]) -> Vec<Target> {
        let module = self.module_of(importer);
        let in_module: Vec<&String> = files
            .iter()
            .filter(|f| self.module_of(f) == module)
            .collect();
        let mut chosen: Vec<&String> = if in_module.is_empty() {
            files.iter().collect()
        } else {
            in_module
        };
        // Main code does not see test sources.
        if !is_test_path(importer) && chosen.iter().any(|f| !is_test_path(f)) {
            chosen.retain(|f| !is_test_path(f));
        }
        let mut by_dir: BTreeMap<&str, Vec<&String>> = BTreeMap::new();
        for f in chosen {
            by_dir.entry(parent(f)).or_default().push(f);
        }
        let mut out = Vec::new();
        for (dir, fs) in by_dir {
            let whole_dir = !dir.is_empty()
                && self.dir_files.get(dir).is_some_and(|all| {
                    all.iter()
                        .all(|f| self.package_of.get(f).is_some_and(|p| p == pkg))
                });
            if whole_dir {
                out.push(Target::Dir(dir.to_string()));
            } else {
                out.extend(fs.into_iter().map(|f| Target::File(f.clone())));
            }
        }
        out
    }

    /// The file that declares `name` as a top-level type of `user`'s own package, for a
    /// name `user` uses without an import ([`Spec::TypeRef`]): the one candidate left in
    /// `user`'s module, then its source set (as for an import). Main code never sees test
    /// sources; the default package counts only within `user`'s dir. `None` when no type
    /// or several fit.
    pub fn same_package(&self, user: &str, name: &str) -> Option<String> {
        let pkg = self.package_of.get(user)?;
        let fqn = if pkg.is_empty() {
            name.to_string()
        } else {
            format!("{pkg}.{name}")
        };
        let cands: Vec<String> = self
            .declared
            .get(&fqn)?
            .iter()
            .filter(|c| c.as_str() != user && (!pkg.is_empty() || parent(c) == parent(user)))
            .cloned()
            .collect();
        match self.prefer(user, &cands).as_slice() {
            [one] if is_test_path(user) || !is_test_path(one) => Some((*one).clone()),
            _ => None,
        }
    }

    /// Resolve one import. Returns `(specifier, outcome)` per target.
    pub fn resolve(
        &self,
        importer: &str,
        name: &str,
        is_static: bool,
        wildcard: bool,
    ) -> Vec<(String, Outcome)> {
        let spec = if wildcard {
            format!("{name}.*")
        } else {
            name.to_string()
        };
        let one = |o| vec![(spec.clone(), o)];
        if !is_static
            && wildcard
            && let Some(files) = self.packages.get(name)
        {
            return self
                .package_targets(importer, name, files)
                .into_iter()
                .map(|target| {
                    (
                        spec.clone(),
                        Outcome::Resolved {
                            target,
                            confidence: Confidence::Resolved,
                        },
                    )
                })
                .collect();
        }
        let segs: Vec<&str> = name.split('.').collect();
        for n in (1..=segs.len()).rev() {
            let prefix = segs[..n].join(".");
            if let Some(cands) = self.types.get(&prefix) {
                let best = self.prefer(importer, cands);
                return match best.as_slice() {
                    [one_file] => one(Outcome::file((*one_file).clone(), Confidence::Resolved)),
                    many => {
                        let list: Vec<&str> = many.iter().take(3).map(|s| s.as_str()).collect();
                        one(Outcome::ambiguous(format!(
                            "{prefix} is declared in {} files: {}",
                            many.len(),
                            list.join(", ")
                        )))
                    }
                };
            }
        }
        let pkg = if wildcard && !is_static {
            name.to_string()
        } else {
            package_part(name)
        };
        if JDK_OTHER
            .iter()
            .chain(JDK_JAVAX.iter())
            .any(|j| has_prefix(&pkg, j))
        {
            return one(Outcome::external(
                format!("jdk:{}", first_segments(&pkg, 2)),
                Confidence::Exact,
            ));
        }
        if self.packages.contains_key(&pkg) {
            let class = name
                .get(pkg.len() + 1..)
                .unwrap_or(name)
                .split('.')
                .next()
                .unwrap_or(name);
            let dir_suffix = format!("{}/{class}.java", pkg.replace('.', "/"));
            let skipped = self
                .skipped
                .iter()
                .find(|(p, _)| p.as_str() == dir_suffix || p.ends_with(&format!("/{dir_suffix}")));
            let detail = match skipped {
                Some((p, why)) => format!("{p} was not indexed ({why})"),
                None => format!(
                    "package {pkg} is in the repo but no indexed file declares {class} (generated code?)"
                ),
            };
            return one(Outcome::not_found(detail));
        }
        if let Some(g) = self.dep_groups.iter().find(|g| has_prefix(&pkg, g)) {
            return one(Outcome::external(
                format!("maven:{g}"),
                Confidence::Resolved,
            ));
        }
        if let Some(g) = self.own_groups.iter().find(|g| has_prefix(&pkg, g)) {
            return one(Outcome::not_found(format!(
                "{pkg} is under the project's groupId {g} but no file declares it (generated code?)"
            )));
        }
        let first = pkg.split('.').next().unwrap_or("");
        let n = if DOMAIN_LIKE.contains(&first) { 2 } else { 1 };
        let group = if pkg.is_empty() {
            first_segments(name, 1)
        } else {
            first_segments(&pkg, n)
        };
        one(Outcome::external(
            format!("maven:{group}"),
            Confidence::Heuristic,
        ))
    }
}

fn add_file(
    types: &mut BTreeMap<String, BTreeSet<String>>,
    packages: &mut BTreeMap<String, BTreeSet<String>>,
    package_of: &mut HashMap<String, String>,
    path: &str,
    pkg: String,
) {
    let stem = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .strip_suffix(".java")
        .unwrap_or(path);
    let fqn = if pkg.is_empty() {
        stem.to_string()
    } else {
        format!("{pkg}.{stem}")
    };
    types.entry(fqn).or_default().insert(path.to_string());
    packages
        .entry(pkg.clone())
        .or_default()
        .insert(path.to_string());
    package_of.insert(path.to_string(), pkg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_parts() {
        assert_eq!(package_part("a.b.C"), "a.b");
        assert_eq!(package_part("a.b.C.m"), "a.b");
        assert_eq!(package_part("a.b.C.Inner"), "a.b");
        assert_eq!(package_part("a.b.c"), "a.b");
        assert_eq!(package_part("lombok.Data"), "lombok");
        assert!(has_prefix("java.util", "java"));
        assert!(!has_prefix("javax.persistence", "java"));
        let jdk = |p: &str| {
            JDK_OTHER
                .iter()
                .chain(JDK_JAVAX.iter())
                .any(|j| has_prefix(p, j))
        };
        assert!(jdk("com.sun.net.httpserver"));
        assert!(jdk("javax.swing.event"));
        assert!(!jdk("com.sun.mail.smtp"));
        assert!(!jdk("javax.persistence"));
    }

    #[test]
    fn build_file_groups() {
        let pom = "<project><parent><groupId>org.springframework.boot</groupId></parent>\
            <groupId>com.acme</groupId><dependencies><dependency><groupId>org.mockito</groupId>\
            <artifactId>m</artifactId></dependency></dependencies></project>";
        let (deps, parents, own) = pom_groups(pom);
        assert_eq!(deps, vec!["org.mockito"]);
        assert_eq!(parents, vec!["org.springframework.boot"]);
        assert_eq!(own.as_deref(), Some("com.acme"));
        let gradle = "group = 'com.acme'\ndependencies {\n  implementation 'com.google.guava:guava:33.0'\n\
            testImplementation(\"org.junit.jupiter:junit-jupiter\")\n  compile group: 'org.x', name: 'y'\n}\n";
        let (deps, own) = gradle_groups(gradle);
        assert_eq!(own.as_deref(), Some("com.acme"));
        assert!(deps.contains(&"com.google.guava".to_string()), "{deps:?}");
        assert!(deps.contains(&"org.junit.jupiter".to_string()), "{deps:?}");
        assert!(deps.contains(&"org.x".to_string()), "{deps:?}");
    }
}
