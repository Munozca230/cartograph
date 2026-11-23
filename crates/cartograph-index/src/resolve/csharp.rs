//! C# using resolution.
//!
//! **Index.** Every `.cs` file is indexed by the namespaces it declares (block namespaces,
//! nested ones composed, and file-scoped ones) and by the types it declares directly in a
//! namespace or at top level (`partial` types are declared by several files).
//!
//! **Target.** A using imports a namespace, which may span many files and dirs. The
//! using resolves to every file declaring the namespace (one import site per file). When
//! those files are exactly the `.cs` files of one dir, the target is that Dir instead
//! (one site), like a Go package.
//!
//! **Order** for a using of `N` written inside namespace `S` (`""` at top level), from file
//! `f`. Candidates are `S.N`, then `N` with each shorter prefix of `S` (C# looks in the
//! enclosing namespaces first), and for each candidate:
//! 1. `using static N` / `using A = N`: `N` is a declared type → its files (`Resolved`);
//!    else the longest prefix of `N` that is a declared type (a nested type) → its files
//!    (`Heuristic`).
//! 2. `N` is a declared namespace → its files or dir (`Resolved`). A type alias may name a
//!    namespace too.
//!
//! Not in the repo:
//! 3. The longest `PackageReference Include` of any `.csproj` prefixing `N` (on a `.`
//!    boundary, ignoring case) → External `nuget:<package id>`, `Resolved`.
//! 4. `System`, `Microsoft` and `Windows` namespaces → External `dotnet:<two segments>`
//!    (`dotnet:System.Collections`, `dotnet:System`), `Resolved`.
//! 5. A namespace only declared as a prefix of deeper ones (`using App;` when the repo has
//!    `App.Core` but no file in `App`), or one whose first segment is a top-level
//!    namespace of the repo → `not_found`.
//! 6. Otherwise External `nuget:<first two segments>`, `Heuristic`.
//!
//! Global usings resolve the same way; their edge starts at the file that declares them
//! (not at every file of the project, which they also affect). Implicit usings from
//! `<ImplicitUsings>` are not in any file and produce nothing.

use super::{Index, Outcome, Target, parent};
use crate::imports::Spec;
use cartograph_core::Confidence;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Immutable C# resolver (shared across threads).
pub struct Resolver {
    /// Mount roots (`""` for a single repo): never a module of their own, since their Dir
    /// node (if any) holds the whole mount.
    mount_roots: HashSet<String>,
    /// Namespace → declaring files (sorted).
    namespaces: HashMap<String, Vec<String>>,
    /// Every prefix of a declared namespace (`A.B.C` → `A`, `A.B`, `A.B.C`).
    namespace_prefixes: BTreeSet<String>,
    /// Fully qualified type → declaring files (sorted).
    types: HashMap<String, Vec<String>>,
    /// Dir → number of `.cs` files in it.
    cs_per_dir: HashMap<String, usize>,
    /// `PackageReference` ids, longest first.
    packages: Vec<String>,
}

/// Whether dotted `name` is `prefix` or starts with `prefix.` (ASCII case-insensitive).
fn has_prefix_ci(name: &str, prefix: &str) -> bool {
    let (n, p) = (name.as_bytes(), prefix.as_bytes());
    !p.is_empty()
        && n.len() >= p.len()
        && n[..p.len()].eq_ignore_ascii_case(p)
        && (n.len() == p.len() || n[p.len()] == b'.')
}

fn first_segments(name: &str, n: usize) -> String {
    name.split('.').take(n).collect::<Vec<_>>().join(".")
}

/// `Include` values of every `<PackageReference …>` in a project file.
fn package_references(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("<PackageReference") {
        let after = &rest[start + "<PackageReference".len()..];
        let tag_end = after.find('>').unwrap_or(after.len());
        let tag = &after[..tag_end];
        if after.starts_with(|c: char| c.is_whitespace())
            && let Some(i) = tag.find("Include=\"")
        {
            let v = &tag[i + "Include=\"".len()..];
            if let Some(q) = v.find('"') {
                out.push(v[..q].trim().to_string());
            }
        }
        rest = &after[tag_end..];
    }
    out
}

impl Resolver {
    pub fn new(ix: &Index<'_>, files: &[super::FileImports<'_>]) -> Self {
        let mut namespaces: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut types: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for f in files.iter().filter(|f| f.lang == "csharp") {
            for r in f.imports {
                match &r.spec {
                    Spec::Declares(n) => {
                        namespaces
                            .entry(n.clone())
                            .or_default()
                            .insert(f.path.to_string());
                    }
                    Spec::DeclaresType(t) => {
                        types
                            .entry(t.clone())
                            .or_default()
                            .insert(f.path.to_string());
                    }
                    _ => {}
                }
            }
        }
        let mut namespace_prefixes = BTreeSet::new();
        for n in namespaces.keys() {
            let segs: Vec<&str> = n.split('.').collect();
            for k in 1..=segs.len() {
                namespace_prefixes.insert(segs[..k].join("."));
            }
        }
        let mut cs_per_dir: HashMap<String, usize> = HashMap::new();
        let mut packages = BTreeSet::new();
        for f in &ix.files {
            let lower = f.to_ascii_lowercase();
            if lower.ends_with(".cs") {
                *cs_per_dir.entry(parent(f).to_string()).or_default() += 1;
            } else if (lower.ends_with(".csproj")
                || lower.ends_with("directory.packages.props")
                || lower.ends_with("directory.build.props"))
                && let Some(text) = ix.read_config(f)
            {
                packages.extend(package_references(&text));
            }
        }
        let mut packages: Vec<String> = packages.into_iter().filter(|p| !p.is_empty()).collect();
        packages.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        let collect = |m: BTreeMap<String, BTreeSet<String>>| {
            m.into_iter()
                .map(|(k, v)| (k, v.into_iter().collect()))
                .collect()
        };
        Resolver {
            mount_roots: ix.mounts.iter().map(|(p, _)| p.to_string()).collect(),
            namespaces: collect(namespaces),
            namespace_prefixes,
            types: collect(types),
            cs_per_dir,
            packages,
        }
    }

    /// Targets of a namespace: its dir when its files are exactly one dir's `.cs` files,
    /// else every file.
    fn namespace_targets(&self, files: &[String]) -> Vec<Target> {
        let dirs: BTreeSet<&str> = files.iter().map(|f| parent(f)).collect();
        if let [dir] = dirs.into_iter().collect::<Vec<_>>().as_slice()
            && !self.mount_roots.contains(*dir)
            && self.cs_per_dir.get(*dir) == Some(&files.len())
        {
            return vec![Target::Dir((*dir).to_string())];
        }
        files.iter().map(|f| Target::File(f.clone())).collect()
    }

    /// Resolve one using directive. Returns `(specifier, outcome)` per target.
    pub fn resolve(
        &self,
        _importer: &str,
        name: &str,
        is_static: bool,
        alias: bool,
        scope: &str,
    ) -> Vec<(String, Outcome)> {
        let many = |targets: Vec<Target>, confidence: Confidence| -> Vec<(String, Outcome)> {
            targets
                .into_iter()
                .map(|target| (name.to_string(), Outcome::Resolved { target, confidence }))
                .collect()
        };
        let files_of = |files: &[String]| -> Vec<Target> {
            files.iter().map(|f| Target::File(f.clone())).collect()
        };
        // Candidates: S.N, then N under each shorter prefix of S, then N.
        let mut candidates = Vec::new();
        let scope_segs: Vec<&str> = if scope.is_empty() {
            Vec::new()
        } else {
            scope.split('.').collect()
        };
        for k in (0..=scope_segs.len()).rev() {
            let s = scope_segs[..k].join(".");
            candidates.push(if s.is_empty() {
                name.to_string()
            } else {
                format!("{s}.{name}")
            });
        }
        for c in &candidates {
            if is_static || alias {
                if let Some(files) = self.types.get(c) {
                    return many(files_of(files), Confidence::Resolved);
                }
                let segs: Vec<&str> = c.split('.').collect();
                for k in (1..segs.len()).rev() {
                    if let Some(files) = self.types.get(&segs[..k].join(".")) {
                        return many(files_of(files), Confidence::Heuristic);
                    }
                }
            }
            if let Some(files) = self.namespaces.get(c) {
                return many(self.namespace_targets(files), Confidence::Resolved);
            }
        }
        let one = |o| vec![(name.to_string(), o)];
        if let Some(p) = self.packages.iter().find(|p| has_prefix_ci(name, p)) {
            return one(Outcome::external(
                format!("nuget:{p}"),
                Confidence::Resolved,
            ));
        }
        let first = name.split('.').next().unwrap_or(name);
        if matches!(first, "System" | "Microsoft" | "Windows") {
            return one(Outcome::external(
                format!("dotnet:{}", first_segments(name, 2)),
                Confidence::Resolved,
            ));
        }
        if let Some(c) = candidates
            .iter()
            .find(|c| self.namespace_prefixes.contains(*c))
        {
            return one(Outcome::not_found(format!(
                "namespace {c} has no files of its own in the repo, only nested namespaces"
            )));
        }
        if self.namespace_prefixes.contains(first) {
            return one(Outcome::not_found(format!(
                "{first} is a namespace of the repo but no file declares {name}"
            )));
        }
        one(Outcome::external(
            format!("nuget:{}", first_segments(name, 2)),
            Confidence::Heuristic,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_reference_parsing() {
        let text = r#"<Project Sdk="Microsoft.NET.Sdk"><ItemGroup>
            <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
            <PackageReferenceX Include="Nope" />
            <PackageReference Version="1" Include="Serilog.Sinks.Console"></PackageReference>
            </ItemGroup></Project>"#;
        assert_eq!(
            package_references(text),
            vec!["Newtonsoft.Json", "Serilog.Sinks.Console"]
        );
        assert!(has_prefix_ci("Newtonsoft.Json.Linq", "newtonsoft.json"));
        assert!(!has_prefix_ci("Newtonsoft.JsonX", "Newtonsoft.Json"));
    }
}
