//! `tsconfig.json` / `jsconfig.json`: `compilerOptions.baseUrl` and `compilerOptions.paths`,
//! following `extends` chains (a string or an array, relative paths or packages in
//! `node_modules`, with or without `.json`), with comments and trailing commas allowed.
//!
//! Semantics follow TypeScript:
//! - `baseUrl` is relative to the config file that sets it; `paths` too when no `baseUrl`
//!   is in effect, otherwise `paths` are relative to the effective `baseUrl`.
//! - A config inherits `baseUrl` and `paths` from what it extends (later entries of an
//!   `extends` array win) and its own values override them.
//! - When several `paths` patterns match, the one with the longest prefix before `*` wins;
//!   an exact (star-less) pattern beats every wildcard.
//!
//! Extension (not TypeScript semantics): a "solution" config with neither `baseUrl` nor
//! `paths` borrows them from the first config in its `references` that has any, so a Vite
//! style `tsconfig.json` → `tsconfig.app.json` layout still resolves its aliases.

use super::{Index, join, jsonc, parent};
use cartograph_core::Diagnostic;
use serde_json::Value;
use std::collections::HashMap;

/// One `paths` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathMapping {
    pub pattern: String,
    pub targets: Vec<String>,
}

/// Effective module-resolution settings of one config file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompilerPaths {
    /// Path of the config file these settings belong to.
    pub config: String,
    /// Effective `baseUrl`, as a repo-relative dir.
    pub base_url: Option<String>,
    pub paths: Vec<PathMapping>,
    /// Dir that `paths` substitutions are relative to.
    pub paths_base: String,
}

impl CompilerPaths {
    pub fn is_empty(&self) -> bool {
        self.base_url.is_none() && self.paths.is_empty()
    }

    /// The best-matching `paths` pattern for `spec` and its substitutions as repo-relative
    /// paths, in order.
    pub fn candidates(&self, spec: &str) -> Option<(&str, Vec<String>)> {
        let mut best: Option<(&PathMapping, &str, usize)> = None;
        for m in &self.paths {
            let Some(capture) = match_pattern(&m.pattern, spec) else {
                continue;
            };
            let rank = match m.pattern.split_once('*') {
                Some((prefix, _)) => prefix.len(),
                None => usize::MAX,
            };
            if best.is_none_or(|(_, _, r)| rank > r) {
                best = Some((m, capture, rank));
            }
        }
        let (m, capture, _) = best?;
        let paths = m
            .targets
            .iter()
            .filter_map(|t| join(&self.paths_base, &t.replacen('*', capture, 1)))
            .collect();
        Some((&m.pattern, paths))
    }
}

/// What `spec` matches `pattern` with: the text under `*`, or `""` for an exact pattern.
pub fn match_pattern<'s>(pattern: &str, spec: &'s str) -> Option<&'s str> {
    match pattern.split_once('*') {
        Some((prefix, suffix)) => {
            if spec.len() >= prefix.len() + suffix.len()
                && spec.starts_with(prefix)
                && spec.ends_with(suffix)
            {
                spec.get(prefix.len()..spec.len() - suffix.len())
            } else {
                None
            }
        }
        None => (pattern == spec).then_some(""),
    }
}

#[derive(Debug, Clone, Default)]
struct Raw {
    base_url: Option<String>,
    /// Mappings and the dir of the config that declared them.
    paths: Option<(Vec<PathMapping>, String)>,
    references: Vec<String>,
}

/// Loads configs with a cache; problems are collected in `diagnostics`.
pub struct Loader<'i, 'a> {
    ix: &'i Index<'a>,
    cache: HashMap<String, Option<Raw>>,
    pub diagnostics: Vec<Diagnostic>,
}

impl<'i, 'a> Loader<'i, 'a> {
    pub fn new(ix: &'i Index<'a>) -> Self {
        Loader {
            ix,
            cache: HashMap::new(),
            diagnostics: Vec::new(),
        }
    }

    fn diag(&mut self, path: &str, detail: String) {
        tracing::warn!(path, %detail, "tsconfig problem");
        self.diagnostics.push(Diagnostic {
            path: path.to_string(),
            detail,
        });
    }

    /// Effective settings of the config file at `path`.
    pub fn effective(&mut self, path: &str) -> CompilerPaths {
        let mut raw = self.load(path, &mut Vec::new()).unwrap_or_default();
        if raw.base_url.is_none() && raw.paths.is_none() {
            for r in raw.references.clone() {
                if let Some(referenced) = self.load(&r, &mut Vec::new())
                    && (referenced.base_url.is_some() || referenced.paths.is_some())
                {
                    raw = referenced;
                    break;
                }
            }
        }
        let (paths, paths_dir) = raw
            .paths
            .unwrap_or_else(|| (Vec::new(), parent(path).to_string()));
        CompilerPaths {
            config: path.to_string(),
            paths_base: raw.base_url.clone().unwrap_or(paths_dir),
            base_url: raw.base_url,
            paths,
        }
    }

    fn exists(&self, path: &str) -> bool {
        self.ix.configs.contains_key(path)
            || self.ix.is_file(path)
            || self.ix.on_disk(path).is_file()
    }

    /// Path of the config an `extends` entry names.
    fn resolve_extends(&self, dir: &str, spec: &str) -> Option<String> {
        let with_variants = |p: String| -> Option<String> {
            [
                p.clone(),
                format!("{p}.json"),
                join(&p, "tsconfig.json").unwrap_or_default(),
            ]
            .into_iter()
            .find(|c| !c.is_empty() && self.exists(c))
        };
        if spec.starts_with('.') {
            return with_variants(join(dir, spec)?);
        }
        // A package: look in node_modules of this dir and every ancestor (on disk; the
        // walker never indexes node_modules).
        let mut d = Some(dir);
        while let Some(cur) = d {
            if let Some(found) = join(cur, &format!("node_modules/{spec}")).and_then(with_variants)
            {
                return Some(found);
            }
            d = (!cur.is_empty()).then(|| parent(cur));
        }
        None
    }

    fn load(&mut self, path: &str, stack: &mut Vec<String>) -> Option<Raw> {
        if let Some(c) = self.cache.get(path) {
            return c.clone();
        }
        if stack.iter().any(|p| p == path) {
            self.diag(path, "tsconfig extends cycle".into());
            return None;
        }
        let Some(text) = self.ix.read_config(path) else {
            self.diag(path, "tsconfig could not be read".into());
            self.cache.insert(path.to_string(), None);
            return None;
        };
        let v = match jsonc::parse(&text) {
            Ok(v) => v,
            Err(e) => {
                self.diag(path, format!("tsconfig: {e}"));
                self.cache.insert(path.to_string(), None);
                return None;
            }
        };
        stack.push(path.to_string());
        let dir = parent(path).to_string();
        let mut raw = Raw::default();

        let extends: Vec<String> = match v.get("extends") {
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        };
        for e in extends {
            match self.resolve_extends(&dir, &e) {
                Some(p) => {
                    if let Some(base) = self.load(&p, stack) {
                        if base.base_url.is_some() {
                            raw.base_url = base.base_url;
                        }
                        if base.paths.is_some() {
                            raw.paths = base.paths;
                        }
                    }
                }
                None => self.diag(path, format!("tsconfig extends {e:?} not found")),
            }
        }

        let opts = v.get("compilerOptions");
        if let Some(b) = opts.and_then(|o| o.get("baseUrl")).and_then(Value::as_str) {
            match join(&dir, b) {
                Some(p) => raw.base_url = Some(p),
                None => self.diag(path, format!("baseUrl {b:?} leaves the repo")),
            }
        }
        if let Some(p) = opts.and_then(|o| o.get("paths")).and_then(Value::as_object) {
            let mappings = p
                .iter()
                .map(|(pattern, targets)| PathMapping {
                    pattern: pattern.clone(),
                    targets: targets
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|t| t.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect();
            raw.paths = Some((mappings, dir.clone()));
        }
        if let Some(refs) = v.get("references").and_then(Value::as_array) {
            for r in refs {
                let Some(p) = r.get("path").and_then(Value::as_str) else {
                    continue;
                };
                let Some(p) = join(&dir, p) else { continue };
                let file = if p.ends_with(".json") {
                    p
                } else {
                    join(&p, "tsconfig.json").unwrap_or_default()
                };
                raw.references.push(file);
            }
        }

        stack.pop();
        self.cache.insert(path.to_string(), Some(raw.clone()));
        Some(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_match_like_typescript() {
        assert_eq!(match_pattern("@/*", "@/a/b"), Some("a/b"));
        assert_eq!(match_pattern("@/*", "@x"), None);
        assert_eq!(match_pattern("*.svg", "./a.svg"), Some("./a"));
        assert_eq!(match_pattern("exact", "exact"), Some(""));
        assert_eq!(match_pattern("exact", "exact/x"), None);
        let cfg = CompilerPaths {
            config: "tsconfig.json".into(),
            base_url: None,
            paths: vec![
                PathMapping {
                    pattern: "*".into(),
                    targets: vec!["vendor/*".into()],
                },
                PathMapping {
                    pattern: "@app/*".into(),
                    targets: vec!["src/app/*".into(), "gen/*".into()],
                },
            ],
            paths_base: "web".into(),
        };
        let (pattern, paths) = cfg.candidates("@app/x").unwrap();
        assert_eq!(pattern, "@app/*");
        assert_eq!(paths, vec!["web/src/app/x", "web/gen/x"]);
        assert_eq!(
            cfg.candidates("lodash").unwrap().1,
            vec!["web/vendor/lodash"]
        );
    }
}
