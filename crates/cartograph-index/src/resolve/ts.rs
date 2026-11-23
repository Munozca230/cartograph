//! TS/JS import resolution.
//!
//! Order for a specifier (after dropping a webpack `loader!` prefix and a `?query`):
//! 1. Relative (`./`, `../`): `.`, `..` and specifiers ending in `/` name a directory and
//!    only probe `<dir>/index.*`. Otherwise probe the path as written, the TS-ESM mapping (`./x.js` →
//!    `x.ts`/`x.tsx`/`x.d.ts`, `.mjs` → `.mts`, `.cjs` → `.cts`; tried first from TS files),
//!    then extensions [`EXTENSIONS`], then `/index.*`. Hit → `Import`, `Exact`.
//! 2. Runtime schemes: `node:`, `bun:`, `cloudflare:` → External `<scheme>:<name>`, `Exact`;
//!    Deno-style `npm:` / `jsr:` → External `npm:<pkg>` / `jsr:<pkg>` (version dropped),
//!    `Resolved`.
//! 3. The nearest `tsconfig.json` / `jsconfig.json`: `paths` (best pattern, each
//!    substitution in order), then `baseUrl`. Hit → `Resolved`. A matching non-`*` pattern
//!    with no file is `not_found`.
//! 4. Node built-in (known list) → External `node:<name>`, `Exact`.
//! 5. Workspace package of the nearest workspace root → its `exports` (`.` or the subpath),
//!    `module`, `main`, `src/index.*`, `index.*` → `Resolved`. Two members with the same
//!    name → `ambiguous`; a member with no matching file → `not_found`.
//! 6. **Heuristic:** a bare specifier that is not a declared dependency of the nearest
//!    `package.json` and matches a file relative to that package's dir (or the repo root
//!    when there is none) → `Import`, `Heuristic`. This is a guess at a bundler alias or
//!    `NODE_PATH` setup (`import x from 'src/x'`).
//! 7. Anything else that is a valid package name → External `npm:<name>`, `Resolved`.
//!    Otherwise (`~/x`, `@/x` with no alias configured) → `not_found`.

use super::npm::{self, Lookup, PackageJson, Workspaces};
use super::tsconfig::{CompilerPaths, Loader};
use super::{Index, Outcome, builtins, join, parent};
use cartograph_core::{Confidence, Diagnostic};
use std::collections::{BTreeMap, HashMap};

/// Extensions probed after the path as written, in order.
pub const EXTENSIONS: [&str; 10] = [
    ".ts", ".tsx", ".d.ts", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts", ".json",
];

/// Immutable TS/JS resolver (shared across threads).
pub struct Resolver<'i, 'a> {
    ix: &'i Index<'a>,
    /// Effective tsconfig/jsconfig settings by the dir holding the config.
    configs: HashMap<String, CompilerPaths>,
    packages: BTreeMap<String, PackageJson>,
    workspaces: Workspaces,
}

impl<'i, 'a> Resolver<'i, 'a> {
    pub fn new(ix: &'i Index<'a>) -> (Self, Vec<Diagnostic>) {
        let mut loader = Loader::new(ix);
        let mut configs = HashMap::new();
        let mut names: Vec<&str> = ix
            .configs
            .keys()
            .copied()
            .filter(|p| {
                let name = p.rsplit('/').next().unwrap_or(p);
                name == "tsconfig.json" || name == "jsconfig.json"
            })
            .collect();
        // tsconfig.json sorts after jsconfig.json, so it wins when both are in one dir.
        names.sort();
        for path in names {
            configs.insert(parent(path).to_string(), loader.effective(path));
        }
        let mut diagnostics = loader.diagnostics;
        let (packages, roots) = npm::parse_all(&ix.configs);
        let workspaces = Workspaces::build(&roots, &packages, &mut diagnostics);
        (
            Resolver {
                ix,
                configs,
                packages,
                workspaces,
            },
            diagnostics,
        )
    }

    /// The nearest config at or above `dir`.
    fn config_for(&self, dir: &str) -> Option<&CompilerPaths> {
        let mut d = dir;
        loop {
            if let Some(c) = self.configs.get(d) {
                return Some(c);
            }
            if d.is_empty() {
                return None;
            }
            d = parent(d);
        }
    }

    /// First indexed file for `base` (see the module docs for the order).
    pub fn probe(&self, base: &str, ts_importer: bool) -> Option<String> {
        let exact = (!base.is_empty() && self.ix.is_file(base)).then(|| base.to_string());
        let esm = || {
            esm_alternatives(base)
                .into_iter()
                .find(|p| self.ix.is_file(p))
        };
        let first = if ts_importer {
            esm().or(exact)
        } else {
            exact.or_else(esm)
        };
        if first.is_some() {
            return first;
        }
        for ext in EXTENSIONS {
            let p = format!("{base}{ext}");
            if self.ix.is_file(&p) {
                return Some(p);
            }
        }
        self.probe_index(base)
    }

    /// `<base>/index.*`, for a specifier that names a directory.
    pub fn probe_index(&self, base: &str) -> Option<String> {
        let index = join(base, "index")?;
        EXTENSIONS
            .iter()
            .map(|ext| format!("{index}{ext}"))
            .find(|p| self.ix.is_file(p))
    }

    /// Resolve `spec` imported from the file `importer` (language `lang`).
    pub fn resolve(&self, importer: &str, lang: &str, spec: &str) -> Outcome {
        let ts_importer = matches!(lang, "typescript" | "tsx");
        let spec = clean(spec);
        let dir = parent(importer);
        if spec.is_empty() {
            return Outcome::not_found("empty specifier");
        }
        if spec == "." || spec == ".." || spec.starts_with("./") || spec.starts_with("../") {
            let Some(base) = self.ix.join(dir, spec) else {
                return Outcome::not_found("relative path leaves the repo");
            };
            // `.`, `..` and `./dir/` name a directory: only its index can match.
            let dir_only = spec == "." || spec == ".." || spec.ends_with('/');
            let hit = if dir_only {
                self.probe_index(&base)
            } else {
                self.probe(&base, ts_importer)
            };
            return match hit {
                Some(p) => Outcome::file(p, Confidence::Exact),
                None => Outcome::not_found(self.missing_detail(&base)),
            };
        }
        if spec.starts_with('/') {
            return Outcome::not_found("absolute path specifier");
        }
        if spec.contains("://") || spec.starts_with("data:") {
            return Outcome::not_found("URL specifier");
        }
        if let Some(outcome) = runtime_scheme(spec) {
            return outcome;
        }
        if spec.starts_with('#') {
            return Outcome::not_found("package.json \"imports\" (#) specifiers are not resolved");
        }

        if let Some(cfg) = self.config_for(dir) {
            if let Some((pattern, candidates)) = cfg.candidates(spec, |d, r| self.ix.join(d, r)) {
                for c in &candidates {
                    if let Some(p) = self.probe(c, ts_importer) {
                        return Outcome::file(p, Confidence::Resolved);
                    }
                }
                if pattern != "*" {
                    return Outcome::not_found(format!(
                        "{} paths {pattern:?} matched but no file exists",
                        cfg.config
                    ));
                }
            }
            if let Some(base_url) = &cfg.base_url
                && let Some(p) = self
                    .ix
                    .join(base_url, spec)
                    .and_then(|b| self.probe(&b, ts_importer))
            {
                return Outcome::file(p, Confidence::Resolved);
            }
        }

        let Some((name, sub)) = npm::split_package(spec) else {
            return Outcome::not_found(
                "not a relative path, a configured alias or a valid package name",
            );
        };
        if builtins::is_node_builtin(name) {
            return Outcome::external(format!("node:{name}"), Confidence::Exact);
        }
        match self.workspaces.lookup(importer, name) {
            Lookup::One(pkg_dir) => {
                return match self.package_file(pkg_dir, sub, ts_importer) {
                    Some(p) => Outcome::file(p, Confidence::Resolved),
                    None => Outcome::not_found(format!(
                        "workspace package {name} ({pkg_dir}) has no file for {spec:?}"
                    )),
                };
            }
            Lookup::Many(dirs) => {
                return Outcome::ambiguous(format!(
                    "workspace package {name} is declared in {}",
                    dirs.join(", ")
                ));
            }
            Lookup::None => {}
        }
        let nearest = npm::nearest_package(&self.packages, dir);
        let declared = nearest.is_some_and(|p| p.deps.contains(name));
        if !declared {
            let base_dir = nearest.map_or("", |p| p.dir.as_str());
            if let Some(p) = join(base_dir, spec).and_then(|b| self.probe(&b, ts_importer)) {
                return Outcome::file(p, Confidence::Heuristic);
            }
        }
        Outcome::external(format!("npm:{name}"), Confidence::Resolved)
    }

    /// File for `sub` (`""` = the package entry) inside the workspace package at `dir`.
    fn package_file(&self, dir: &str, sub: &str, ts_importer: bool) -> Option<String> {
        let pkg = self.packages.get(dir)?;
        let probe = |rel: &str| npm::entry_path(dir, rel).and_then(|p| self.probe(&p, ts_importer));
        if let Some(exports) = &pkg.exports {
            let key = if sub.is_empty() {
                ".".to_string()
            } else {
                format!("./{sub}")
            };
            if let Some(p) = npm::export_targets(exports, &key)
                .iter()
                .find_map(|t| probe(t))
            {
                return Some(p);
            }
        }
        if sub.is_empty() {
            [pkg.module.as_deref(), pkg.main.as_deref()]
                .into_iter()
                .flatten()
                .chain(["src/index", "index"])
                .find_map(probe)
        } else {
            probe(sub).or_else(|| probe(&format!("src/{sub}")))
        }
    }

    /// Why a relative target is missing, when the walker knows.
    fn missing_detail(&self, base: &str) -> String {
        let mut candidates = vec![base.to_string()];
        candidates.extend(EXTENSIONS.iter().map(|e| format!("{base}{e}")));
        for c in &candidates {
            if let Some(reason) = self.ix.skip_reason(c) {
                return format!("{c} was skipped ({reason})");
            }
        }
        if candidates.iter().any(|c| self.ix.on_disk(c).is_file()) {
            return format!("{base} exists on disk but is not indexed (ignored)");
        }
        format!("no file for {base}")
    }
}

/// Drop a webpack loader prefix (`style-loader!./x.css`) and a `?query` / `#hash` suffix
/// (`./x.svg?react`). A leading `#` (package imports) is kept.
fn clean(spec: &str) -> &str {
    let spec = spec.rsplit('!').next().unwrap_or(spec);
    let cut = spec
        .char_indices()
        .skip(1)
        .find(|(_, c)| *c == '?' || *c == '#')
        .map_or(spec.len(), |(i, _)| i);
    spec.get(..cut).unwrap_or(spec).trim()
}

/// Specifiers with a runtime scheme (`node:fs`, `bun:sqlite`, `cloudflare:sockets`,
/// `npm:react@18/x`, `jsr:@std/path@1`). `None` for anything else.
fn runtime_scheme(spec: &str) -> Option<Outcome> {
    let (scheme, rest) = spec.split_once(':')?;
    match scheme {
        "node" | "bun" | "cloudflare" => {
            let top = rest.split('/').next().unwrap_or(rest);
            Some(Outcome::external(
                format!("{scheme}:{top}"),
                Confidence::Exact,
            ))
        }
        "npm" | "jsr" => {
            let rest = rest.trim_start_matches('/');
            Some(match npm::split_package(rest) {
                Some((name, _)) => {
                    // Drop a version: `react@18` → `react`, `@std/path@1` → `@std/path`.
                    let name = match name.get(1..).and_then(|n| n.find('@')) {
                        Some(i) => name.get(..i + 1).unwrap_or(name),
                        None => name,
                    };
                    Outcome::external(format!("{scheme}:{name}"), Confidence::Resolved)
                }
                None => Outcome::not_found(format!("invalid {scheme}: specifier")),
            })
        }
        _ => None,
    }
}

/// TS-ESM: a `.js`-style extension written in a specifier may name a TS source.
fn esm_alternatives(base: &str) -> Vec<String> {
    let map: [(&str, &[&str]); 4] = [
        (".js", &[".ts", ".tsx", ".d.ts"]),
        (".jsx", &[".tsx"]),
        (".mjs", &[".mts", ".d.mts"]),
        (".cjs", &[".cts", ".d.cts"]),
    ];
    for (from, to) in map {
        if let Some(stem) = base.strip_suffix(from) {
            return to.iter().map(|t| format!("{stem}{t}")).collect();
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleaning_and_esm_mapping() {
        assert_eq!(clean("style-loader!css-loader!./a.css"), "./a.css");
        assert_eq!(clean("./icon.svg?react"), "./icon.svg");
        assert_eq!(clean("#internal/x"), "#internal/x");
        assert_eq!(
            esm_alternatives("a/x.js"),
            vec!["a/x.ts", "a/x.tsx", "a/x.d.ts"]
        );
        assert!(esm_alternatives("a/x").is_empty());
    }

    #[test]
    fn runtime_schemes_are_externals() {
        let ext = |s: &str| match runtime_scheme(s) {
            Some(Outcome::Resolved {
                target: super::super::Target::External(e),
                confidence,
            }) => Some((e, confidence)),
            _ => None,
        };
        assert_eq!(
            ext("node:fs/promises"),
            Some(("node:fs".into(), Confidence::Exact))
        );
        assert_eq!(
            ext("cloudflare:sockets"),
            Some(("cloudflare:sockets".into(), Confidence::Exact))
        );
        assert_eq!(
            ext("bun:sqlite"),
            Some(("bun:sqlite".into(), Confidence::Exact))
        );
        assert_eq!(
            ext("npm:react@18/jsx-runtime"),
            Some(("npm:react".into(), Confidence::Resolved))
        );
        assert_eq!(
            ext("jsr:@std/path@1.0.0"),
            Some(("jsr:@std/path".into(), Confidence::Resolved))
        );
        assert!(runtime_scheme("react").is_none());
        assert!(runtime_scheme("./a:b").is_none());
    }
}
