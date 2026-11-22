//! `cartograph.toml`: the intent layer (layers, allowed and forbidden dependencies, limits,
//! ignored paths, threshold overrides). Optional; documented in `docs/cartograph-toml.md`.
//!
//! Every error names the file, the line and the key, e.g.
//! `cartograph.toml:7: layers[1].allow: unknown layer "cor" (layers: ui, core)`.

use crate::findings::kind::FindingKind;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use toml::Spanned;

/// File name at the repo root.
pub const CONFIG_FILE: &str = "cartograph.toml";

/// A problem in `cartograph.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// Where the config came from (a path, or `cartograph.toml`).
    pub origin: String,
    /// 1-based line, when known.
    pub line: Option<usize>,
    /// Dotted key (`layers[1].allow`), when known.
    pub key: Option<String>,
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.origin)?;
        if let Some(l) = self.line {
            write!(f, ":{l}")?;
        }
        write!(f, ": ")?;
        if let Some(k) = &self.key {
            write!(f, "{k}: ")?;
        }
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ConfigError {}

/// A set of repo-relative path patterns. A pattern with glob characters (`*`, `?`, `[`, `{`)
/// is a glob where `*` stays inside one folder and `**` crosses folders; a plain path
/// (`src/ui` or `src/ui/`) matches that file or everything under that folder.
#[derive(Debug, Clone, Default)]
pub struct PathSet {
    pub patterns: Vec<String>,
    globs: Option<GlobSet>,
    prefixes: Vec<String>,
}

impl PathSet {
    pub(crate) fn new(patterns: &[String]) -> Result<PathSet, (usize, String)> {
        let mut b = GlobSetBuilder::new();
        let mut any_glob = false;
        let mut prefixes = Vec::new();
        for (i, p) in patterns.iter().enumerate() {
            let p = p.trim().replace('\\', "/");
            let p = p.strip_prefix("./").unwrap_or(&p).to_string();
            if p.is_empty() {
                return Err((i, "empty path pattern".into()));
            }
            if p.contains(['*', '?', '[', '{']) {
                let g = GlobBuilder::new(&p)
                    .literal_separator(true)
                    .build()
                    .map_err(|e| (i, format!("bad glob {p:?}: {e}")))?;
                b.add(g);
                any_glob = true;
            } else {
                prefixes.push(p.trim_end_matches('/').to_string());
            }
        }
        let globs = if any_glob {
            Some(b.build().map_err(|e| (0, e.to_string()))?)
        } else {
            None
        };
        Ok(PathSet {
            patterns: patterns.to_vec(),
            globs,
            prefixes,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    pub fn matches(&self, path: &str) -> bool {
        self.prefixes.iter().any(|p| {
            path == p
                || p.is_empty()
                || (path.len() > p.len()
                    && path.starts_with(p.as_str())
                    && path.as_bytes()[p.len()] == b'/')
        }) || self.globs.as_ref().is_some_and(|g| g.is_match(path))
    }
}

#[derive(Debug, Clone)]
pub struct Layer {
    pub name: String,
    pub paths: PathSet,
    /// Indexes of the layers it may depend on (besides itself); `None` = no allow-list.
    pub allow: Option<Vec<usize>>,
    /// Indexes of the layers it must never depend on.
    pub forbid: Vec<usize>,
}

/// Where a cycle breaks the rules.
#[derive(Debug, Clone, Default)]
pub enum NoCycles {
    /// Cycles are reported, never a rule violation.
    #[default]
    Off,
    /// Every cycle is a violation.
    All,
    /// A cycle is a violation when a file of its shortest cycle matches.
    Scopes(PathSet),
}

/// Threshold overrides for the findings that measure the repo against itself.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Thresholds {
    /// Tukey fence multiplier: a value is an outlier above Q3 + `fence` × IQR of the repo.
    pub fence: f64,
    /// God module floors (a file must also clear the fences).
    pub god_min_fan_in: usize,
    pub god_min_fan_out: usize,
    pub god_min_loc: u64,
    /// At most this many bottleneck (betweenness) findings.
    pub max_hubs: usize,
    /// Minimum share of all dependency paths through a file to call it a bottleneck.
    pub hub_min_share: f64,
    /// A dependency chain is deep from this many files on (and above the fence).
    pub chain_min: usize,
    /// At most this many deep-chain findings.
    pub max_chains: usize,
    /// Stable Dependencies: the depended-on folder must be this much less stable.
    pub sdp_margin: f64,
    /// Stable Dependencies: only folders with at least this many code files count.
    pub sdp_min_files: usize,
    /// Stable Dependencies: the depended-on folder must have at least this many dependents.
    pub sdp_min_dependents: usize,
    /// At most this many Stable Dependencies findings.
    pub max_sdp: usize,
    /// Hidden coupling: at least this many shared commits (and above the fence).
    pub coupling_min_shared: usize,
    /// Hidden coupling: shared commits over the mean of both files' commits, at least.
    pub coupling_min_degree: f64,
    /// At most this many hidden-coupling findings.
    pub max_coupling: usize,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            fence: 1.5,
            god_min_fan_in: 8,
            god_min_fan_out: 8,
            god_min_loc: 300,
            max_hubs: 5,
            hub_min_share: 0.05,
            chain_min: 10,
            max_chains: 3,
            sdp_margin: 0.4,
            sdp_min_files: 3,
            sdp_min_dependents: 3,
            max_sdp: 10,
            coupling_min_shared: 5,
            coupling_min_degree: 0.5,
            max_coupling: 10,
        }
    }
}

/// A validated `cartograph.toml` (or the defaults when there is none).
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Where it was read from; `None` = defaults.
    pub origin: Option<String>,
    pub layers: Vec<Layer>,
    pub max_fan_out: Option<usize>,
    pub no_cycles: NoCycles,
    /// Files left out of every finding.
    pub ignore: PathSet,
    pub thresholds: Thresholds,
    /// Finding kinds turned off.
    pub disabled: BTreeSet<FindingKind>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    layers: Vec<Spanned<RawLayer>>,
    #[serde(default)]
    rules: Option<RawRules>,
    #[serde(default)]
    ignore: Option<Spanned<Vec<String>>>,
    #[serde(default)]
    thresholds: Option<Spanned<Thresholds>>,
    #[serde(default)]
    findings: Option<RawFindings>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLayer {
    name: Spanned<String>,
    paths: Spanned<Vec<String>>,
    #[serde(default)]
    allow: Option<Spanned<Vec<String>>>,
    #[serde(default)]
    forbid: Option<Spanned<Vec<String>>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRules {
    #[serde(default)]
    max_fan_out: Option<Spanned<i64>>,
    #[serde(default)]
    no_cycles: Option<Spanned<NoCyclesRaw>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum NoCyclesRaw {
    Flag(bool),
    Scopes(Vec<String>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFindings {
    #[serde(default)]
    disable: Option<Spanned<Vec<String>>>,
}

fn line_of(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1
}

impl Config {
    /// Read `<root>/cartograph.toml`; the defaults when it does not exist.
    /// Errors and `origin` name it `cartograph.toml` (relative to the repo).
    pub fn load(root: &Path) -> Result<Config, ConfigError> {
        Config::read(&root.join(CONFIG_FILE), true, CONFIG_FILE.to_string())
    }

    /// Read a config file. With `optional`, a missing file gives the defaults.
    pub fn load_file(path: &Path, optional: bool) -> Result<Config, ConfigError> {
        let origin = path.display().to_string().replace('\\', "/");
        Config::read(path, optional, origin)
    }

    /// Only the top-level `ignore` of `<root>/cartograph.toml` (for `infer`, which proposes
    /// the rest and may be replacing a broken file): anything else in the file, valid or
    /// not, is not looked at. No file or no `ignore` gives the defaults; a file that is not
    /// TOML, or an `ignore` that is not a list of valid patterns, is an error.
    pub fn load_ignore(root: &Path) -> Result<Config, ConfigError> {
        let origin = CONFIG_FILE.to_string();
        let err = |line: Option<usize>, key: Option<&str>, message: String| ConfigError {
            origin: CONFIG_FILE.to_string(),
            line,
            key: key.map(str::to_string),
            message,
        };
        let text = match std::fs::read_to_string(root.join(CONFIG_FILE)) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(err(None, None, format!("cannot read: {e}"))),
        };
        let table: toml::Table = toml::from_str(&text).map_err(|e| {
            let line = e.span().map(|s| line_of(&text, s.start));
            let msg = e.message().lines().next().unwrap_or("invalid TOML");
            err(line, None, msg.to_string())
        })?;
        let Some(raw) = table.get("ignore") else {
            return Ok(Config::default());
        };
        let patterns: Vec<String> = raw
            .as_array()
            .and_then(|a| a.iter().map(|v| v.as_str().map(str::to_string)).collect())
            .ok_or_else(|| {
                err(
                    None,
                    Some("ignore"),
                    "must be a list of path patterns".into(),
                )
            })?;
        let ignore = PathSet::new(&patterns).map_err(|(_, m)| err(None, Some("ignore"), m))?;
        Ok(Config {
            origin: Some(origin),
            ignore,
            ..Config::default()
        })
    }

    fn read(path: &Path, optional: bool, origin: String) -> Result<Config, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text, &origin),
            Err(e) if optional && e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(ConfigError {
                origin,
                line: None,
                key: None,
                message: format!("cannot read: {e}"),
            }),
        }
    }

    /// Parse and validate config text. `origin` names it in errors.
    pub fn parse(text: &str, origin: &str) -> Result<Config, ConfigError> {
        let err = |line: Option<usize>, key: Option<String>, message: String| ConfigError {
            origin: origin.to_string(),
            line,
            key,
            message,
        };
        let raw: RawConfig = toml::from_str(text).map_err(|e| {
            let line = e.span().map(|s| line_of(text, s.start));
            // The message of toml's error repeats the source snippet; keep its first line.
            let msg = e
                .message()
                .lines()
                .next()
                .unwrap_or("invalid TOML")
                .to_string();
            err(line, None, msg)
        })?;
        let at = |s: std::ops::Range<usize>| Some(line_of(text, s.start));

        let mut cfg = Config {
            origin: Some(origin.to_string()),
            ..Config::default()
        };
        // Layers: names first, so allow/forbid can refer to later layers.
        let mut names: Vec<String> = Vec::new();
        for (i, l) in raw.layers.iter().enumerate() {
            let l = l.get_ref();
            let name = l.name.get_ref().trim().to_string();
            let key = Some(format!("layers[{i}].name"));
            if name.is_empty() {
                return Err(err(at(l.name.span()), key, "layer name is empty".into()));
            }
            if names.contains(&name) {
                return Err(err(
                    at(l.name.span()),
                    key,
                    format!("duplicate layer {name:?}"),
                ));
            }
            names.push(name);
        }
        let find = |list: &Spanned<Vec<String>>, key: String| -> Result<Vec<usize>, ConfigError> {
            list.get_ref()
                .iter()
                .map(|n| {
                    names.iter().position(|x| x == n.trim()).ok_or_else(|| {
                        err(
                            at(list.span()),
                            Some(key.clone()),
                            format!("unknown layer {n:?} (layers: {})", names.join(", ")),
                        )
                    })
                })
                .collect()
        };
        for (i, l) in raw.layers.iter().enumerate() {
            let l = l.get_ref();
            if l.paths.get_ref().is_empty() {
                return Err(err(
                    at(l.paths.span()),
                    Some(format!("layers[{i}].paths")),
                    "a layer needs at least one path".into(),
                ));
            }
            let paths = PathSet::new(l.paths.get_ref())
                .map_err(|(_, m)| err(at(l.paths.span()), Some(format!("layers[{i}].paths")), m))?;
            let allow = match &l.allow {
                Some(a) => Some(find(a, format!("layers[{i}].allow"))?),
                None => None,
            };
            let forbid = match &l.forbid {
                Some(f) => find(f, format!("layers[{i}].forbid"))?,
                None => Vec::new(),
            };
            if let (Some(a), Some(f)) = (&allow, &l.forbid)
                && let Some(both) = forbid.iter().find(|x| a.contains(x))
            {
                return Err(err(
                    at(f.span()),
                    Some(format!("layers[{i}].forbid")),
                    format!("layer {:?} is both allowed and forbidden", names[*both]),
                ));
            }
            // Allowing yourself is harmless; forbidding yourself is a mistake.
            if forbid.contains(&i) {
                return Err(err(
                    at(l.forbid.as_ref().map_or(l.name.span(), |f| f.span())),
                    Some(format!("layers[{i}].forbid")),
                    "a layer cannot forbid itself (dependencies inside a layer are always allowed)"
                        .into(),
                ));
            }
            cfg.layers.push(Layer {
                name: names[i].clone(),
                paths,
                allow,
                forbid,
            });
        }
        if let Some(rules) = &raw.rules {
            if let Some(m) = &rules.max_fan_out {
                let v = *m.get_ref();
                if v < 1 {
                    return Err(err(
                        at(m.span()),
                        Some("rules.max_fan_out".into()),
                        format!("must be at least 1, got {v}"),
                    ));
                }
                cfg.max_fan_out = Some(v as usize);
            }
            if let Some(nc) = &rules.no_cycles {
                cfg.no_cycles = match nc.get_ref() {
                    NoCyclesRaw::Flag(true) => NoCycles::All,
                    NoCyclesRaw::Flag(false) => NoCycles::Off,
                    NoCyclesRaw::Scopes(list) if list.is_empty() => NoCycles::Off,
                    NoCyclesRaw::Scopes(list) => {
                        NoCycles::Scopes(PathSet::new(list).map_err(|(_, m)| {
                            err(at(nc.span()), Some("rules.no_cycles".into()), m)
                        })?)
                    }
                };
            }
        }
        if let Some(ig) = &raw.ignore {
            cfg.ignore = PathSet::new(ig.get_ref())
                .map_err(|(_, m)| err(at(ig.span()), Some("ignore".into()), m))?;
        }
        if let Some(t) = &raw.thresholds {
            let th = t.get_ref().clone();
            let bad = |key: &str, msg: &str| {
                Err(err(
                    at(t.span()),
                    Some(format!("thresholds.{key}")),
                    msg.to_string(),
                ))
            };
            if !(th.fence.is_finite() && th.fence >= 0.0) {
                return bad("fence", "must be a number of at least 0");
            }
            if !(th.hub_min_share.is_finite() && (0.0..=1.0).contains(&th.hub_min_share)) {
                return bad("hub_min_share", "must be between 0 and 1");
            }
            if !(th.sdp_margin.is_finite() && (0.0..=1.0).contains(&th.sdp_margin)) {
                return bad("sdp_margin", "must be between 0 and 1");
            }
            if !(th.coupling_min_degree.is_finite()
                && (0.0..=1.0).contains(&th.coupling_min_degree))
            {
                return bad("coupling_min_degree", "must be between 0 and 1");
            }
            if th.chain_min < 2 {
                return bad("chain_min", "must be at least 2");
            }
            cfg.thresholds = th;
        }
        if let Some(f) = &raw.findings
            && let Some(d) = &f.disable
        {
            for k in d.get_ref() {
                let kind: FindingKind = k.parse().map_err(|_| {
                    err(
                        at(d.span()),
                        Some("findings.disable".into()),
                        format!(
                            "unknown finding kind {k:?} (kinds: {})",
                            FindingKind::ALL
                                .iter()
                                .map(|k| k.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    )
                })?;
                cfg.disabled.insert(kind);
            }
        }
        Ok(cfg)
    }

    /// Index of the first layer whose paths match `path`.
    pub fn layer_of(&self, path: &str) -> Option<usize> {
        self.layers.iter().position(|l| l.paths.matches(path))
    }

    /// Whether `from` may depend on `to` (two layer indexes).
    pub fn allowed(&self, from: usize, to: usize) -> bool {
        if from == to {
            return true;
        }
        let l = &self.layers[from];
        if l.forbid.contains(&to) {
            return false;
        }
        l.allow.as_ref().is_none_or(|a| a.contains(&to))
    }

    pub fn enabled(&self, kind: FindingKind) -> bool {
        !self.disabled.contains(&kind)
    }

    /// Whether the config declares any rule `check` enforces.
    pub fn has_rules(&self) -> bool {
        !self.layers.is_empty()
            || self.max_fan_out.is_some()
            || !matches!(self.no_cycles, NoCycles::Off)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(t: &str) -> Result<Config, ConfigError> {
        Config::parse(t, "cartograph.toml")
    }

    #[test]
    fn a_full_config_parses() {
        let c = parse(
            r#"
ignore = ["**/generated/**"]

[[layers]]
name = "ui"
paths = ["src/ui/**"]
allow = ["core"]

[[layers]]
name = "core"
paths = ["src/core"]
forbid = ["ui"]

[rules]
max_fan_out = 20
no_cycles = ["src/core/**"]

[thresholds]
fence = 2.0

[findings]
disable = ["sdp"]
"#,
        )
        .unwrap();
        assert_eq!(c.layers.len(), 2);
        assert_eq!(c.layer_of("src/ui/a.ts"), Some(0));
        assert_eq!(c.layer_of("src/core/x/b.ts"), Some(1));
        assert_eq!(c.layer_of("src/corex/b.ts"), None);
        assert!(c.allowed(0, 1));
        assert!(!c.allowed(1, 0));
        assert_eq!(c.max_fan_out, Some(20));
        assert!(c.ignore.matches("a/generated/x.ts"));
        assert!(!c.enabled(FindingKind::Sdp));
        assert_eq!(c.thresholds.fence, 2.0);
        assert!(c.has_rules());
    }

    #[test]
    fn errors_name_the_line_and_key() {
        let e =
            parse("[[layers]]\nname = \"ui\"\npaths = [\"a\"]\nallow = [\"cor\"]\n").unwrap_err();
        assert_eq!(e.line, Some(4), "{e}");
        assert_eq!(e.key.as_deref(), Some("layers[0].allow"));
        assert!(
            e.to_string()
                .starts_with("cartograph.toml:4: layers[0].allow: unknown layer \"cor\""),
            "{e}"
        );

        let e = parse("[rules]\nmax_fanout = 3\n").unwrap_err();
        assert_eq!(e.line, Some(2), "{e}");
        assert!(e.message.contains("max_fanout"), "{e}");

        let e = parse("[[layers]]\nname = \"a\"\npaths = [\"[\"]\n").unwrap_err();
        assert_eq!(e.line, Some(3), "{e}");

        let e = parse("[rules]\nmax_fan_out = 0\n").unwrap_err();
        assert_eq!(e.key.as_deref(), Some("rules.max_fan_out"));

        let e = parse("[findings]\ndisable = [\"nope\"]\n").unwrap_err();
        assert!(e.message.contains("unknown finding kind"), "{e}");

        let e = parse("[[layers]]\nname = \"a\"\npaths = []\n").unwrap_err();
        assert!(e.message.contains("at least one path"), "{e}");

        let e = parse("[[layers]]\nname = \"a\"\npaths = [\"x\"]\n[[layers]]\nname = \"a\"\npaths = [\"y\"]\n").unwrap_err();
        assert_eq!(e.line, Some(5), "{e}");

        let e = parse("not toml at all =").unwrap_err();
        assert_eq!(e.line, Some(1), "{e}");
    }

    #[test]
    fn no_file_means_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config::load(dir.path()).unwrap();
        assert!(c.origin.is_none());
        assert!(!c.has_rules());
    }
}
