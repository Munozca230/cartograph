//! Folders that hold data, not code: captures and screenshots, logs, backups, temporary
//! and coverage output, test snapshots, tool caches, and hidden folders at the top of the
//! repo (tooling: CI, editor, agent and hook settings and their scripts).
//! The walk finds them from paths alone ([`candidates`]) and reads nothing in them unless
//! something the walk did read names them ([`referenced`]): an import (`./logs/x`,
//! `from tmp import y`) or a `package.json` entry. Those are read like any other folder.
//!
//! A folder that `.gitignore` ignores is never walked in the first place; these rules are
//! for the ones a repository keeps, or that sit next to it in a folder above it.

use crate::imports::{RawImport, Spec};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// Folder names that hold output, at any depth (compared in lower case): left out even
/// when they hold code (coverage reports carry `.js`).
const OUTPUT_NAMES: &[&str] = &[
    "capturas",
    "captures",
    "screenshots",
    "coverage",
    "__snapshots__",
    ".nyc_output",
];
/// Folder names that hold logs, temporary files or backups, but also name code packages
/// (`cmd/backup/`, a `log` module): left out only when they hold no code.
const DATA_NAMES: &[&str] = &["logs", "log", "tmp", "temp", "backup", "backups"];
/// Caches of tools (a scanner's work folder, Python bytecode and checkers), at any depth.
const CACHE_NAMES: &[&str] = &[
    ".scannerwork",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".parcel-cache",
    ".sass-cache",
    ".turbo",
];
/// Extensions of files that are data: images, video and audio, logs, archives, documents,
/// dumps.
const DATA_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff", "ico", "heic", "svg", "mp4", "webm",
    "mov", "avi", "mkv", "mp3", "wav", "ogg", "log", "zip", "tar", "gz", "tgz", "bz2", "xz", "7z",
    "rar", "bundle", "pdf", "json", "jsonl", "ndjson", "har", "csv", "tsv",
];
/// Of those, the ones that may be a project's data rather than output (a folder of only
/// these is kept: locales, fixtures, settings).
const JSON_EXTS: &[&str] = &["json", "jsonl", "ndjson", "csv", "tsv"];
/// Fewest files that make a folder of images, logs or archives a data folder by content
/// alone (an `assets/` folder with a logo is not).
const MIN_DATA_FILES: usize = 5;
/// Extensions of documentation and configuration: what a hidden tool folder holds when it
/// holds no code.
const CONFIG_EXTS: &[&str] = &[
    "md",
    "txt",
    "json",
    "yml",
    "yaml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "lock",
    "xml",
    "properties",
];

fn ext(name: &str) -> String {
    // `api.log.1`: a rotated log.
    let mut parts = name.rsplit('.');
    let last = parts.next().unwrap_or("").to_ascii_lowercase();
    if !last.is_empty() && last.chars().all(|c| c.is_ascii_digit()) {
        return parts.next().unwrap_or("").to_ascii_lowercase();
    }
    if name.contains('.') {
        last
    } else {
        String::new()
    }
}

/// Why a folder name alone marks output or data, if it does, and whether that holds even
/// when the folder has code: caches, output, and copies (`_backup*`, `backup-<date>`,
/// `backup_<x>`, `*.bak`) always; `logs`, `log`, `tmp`, `temp`, `backup`, `backups` only
/// without code.
pub(crate) fn by_name(name: &str) -> Option<(&'static str, bool)> {
    let lower = name.to_ascii_lowercase();
    if CACHE_NAMES.contains(&lower.as_str()) {
        return Some(("a tool's cache", true));
    }
    const WHY: &str =
        "named like output or data (logs, captures, backups, temporary files, coverage, snapshots)";
    let copy = lower.starts_with("_backup")
        || lower.starts_with("backup-")
        || lower.starts_with("backup_")
        || lower.ends_with(".bak");
    if OUTPUT_NAMES.contains(&lower.as_str()) || copy {
        return Some((WHY, true));
    }
    DATA_NAMES.contains(&lower.as_str()).then_some((WHY, false))
}

fn n_files(n: usize) -> String {
    if n == 1 {
        "1 file".into()
    } else {
        format!("{n} files")
    }
}

#[derive(Default)]
struct Count {
    files: usize,
    data: usize,
    json: usize,
    config: usize,
    code: usize,
}

/// Folders (repo-relative, topmost only) whose files the walk should not read, each with
/// the reason, from the paths of the files below the root.
pub fn candidates<'a>(files: impl Iterator<Item = &'a str>) -> Vec<(String, String)> {
    let mut by_dir: BTreeMap<String, Count> = BTreeMap::new();
    for rel in files {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let e = ext(name);
        let code = crate::lang::detect(rel).is_some();
        let mut dir = rel;
        while let Some((parent, _)) = dir.rsplit_once('/') {
            let c = by_dir.entry(parent.to_string()).or_default();
            c.files += 1;
            if DATA_EXTS.contains(&e.as_str()) {
                c.data += 1;
                if JSON_EXTS.contains(&e.as_str()) {
                    c.json += 1;
                }
            }
            if CONFIG_EXTS.contains(&e.as_str()) {
                c.config += 1;
            }
            if code {
                c.code += 1;
            }
            dir = parent;
        }
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for (dir, c) in &by_dir {
        if out
            .iter()
            .any(|(d, _)| dir.starts_with(d.as_str()) && dir.as_bytes().get(d.len()) == Some(&b'/'))
        {
            continue;
        }
        let name = dir.rsplit('/').next().unwrap_or(dir);
        let named = by_name(name).filter(|(_, even_code)| *even_code || c.code == 0);
        let why = if let Some((why, _)) = named {
            Some(format!("{why}: {}", n_files(c.files)))
        } else if c.files >= MIN_DATA_FILES && c.data == c.files && c.data > c.json {
            Some(format!(
                "data folder: {}, all images, logs, archives or dumps",
                n_files(c.files)
            ))
        } else if !dir.contains('/') && name.starts_with('.') {
            // A hidden folder at the top: tooling (CI, editor, agent and hook settings, their
            // scripts), not the program. Kept when something outside imports it.
            Some(if c.code == 0 && c.config == c.files {
                format!(
                    "hidden tool folder: {} of docs and configuration, no code",
                    n_files(c.files)
                )
            } else {
                format!(
                    "hidden tool folder: {}, nothing outside it imports them",
                    n_files(c.files)
                )
            })
        } else {
            None
        };
        if let Some(why) = why {
            out.push((dir.clone(), why));
        }
    }
    out
}

/// Words of a specifier, in lower case: `./logs/x` → `.`, `logs`, `x`; `from tmp.a import
/// b` → `tmp`, `a`; `com.x.log.Logger` → `com`, `x`, `log`, `logger`.
fn words(s: &str, out: &mut HashSet<String>) {
    for w in s.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-' || c == '.')) {
        out.insert(w.to_ascii_lowercase());
        for part in w.split('.') {
            out.insert(part.to_ascii_lowercase());
        }
    }
}

/// What the files read point at: the folder-relative paths of their relative imports and
/// of manifest entries (lower case, from the repo root), and the words of every other
/// specifier (see [`words`]).
#[derive(Debug, Default)]
pub struct Named {
    paths: Vec<String>,
    words: HashSet<String>,
}

/// `dir/rel` with `.` and `..` applied; `None` above the root.
fn join(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/").to_ascii_lowercase())
}

impl Named {
    /// From `(importing file, import)` pairs and `(manifest folder, entry)` pairs.
    pub fn of<'a>(
        imports: impl Iterator<Item = (&'a str, &'a RawImport)>,
        entries: impl Iterator<Item = (&'a str, &'a str)>,
    ) -> Named {
        let mut n = Named::default();
        for (file, i) in imports {
            let dir = file.rsplit_once('/').map_or("", |(d, _)| d);
            match &i.spec {
                Spec::Path(s) | Spec::TypePath(s)
                    if s.starts_with("./") || s.starts_with("../") =>
                {
                    n.paths.extend(join(dir, s));
                }
                // Relative to the file, or found through an include path: both count.
                Spec::Include {
                    path,
                    system: false,
                } => {
                    n.paths.extend(join(dir, path));
                    words(path, &mut n.words);
                }
                Spec::Path(s) | Spec::TypePath(s) | Spec::Go(s) => words(s, &mut n.words),
                Spec::Include { path, .. } => words(path, &mut n.words),
                Spec::Python { module, .. } => words(module, &mut n.words),
                Spec::Java { name, .. } | Spec::CSharp { name, .. } => words(name, &mut n.words),
                Spec::Dynamic(_)
                | Spec::ParseError(_)
                | Spec::Declares(_)
                | Spec::DeclaresType(_) => {}
                other => words(&format!("{other:?}"), &mut n.words),
            }
        }
        for (dir, e) in entries {
            n.paths.extend(join(dir, e));
        }
        n
    }

    /// Whether something read points into the folder `dir`: a relative import or a manifest
    /// entry below it, or another specifier with its name as a word (an alias, a package
    /// path, a dotted module). A guess that keeps too much rather than too little.
    pub fn names(&self, dir: &str) -> bool {
        let dir = dir.to_ascii_lowercase();
        let name = dir.rsplit('/').next().unwrap_or(&dir).to_string();
        self.words.contains(&name)
            || self.paths.iter().any(|p| {
                p == &dir
                    || (p.starts_with(dir.as_str()) && p.as_bytes().get(dir.len()) == Some(&b'/'))
            })
    }
}

/// A change the watcher can ignore without a walk: a minified bundle (by name), a file
/// below a sibling repository's worktree copies, or a file that is not code inside a
/// cache, an output folder, or a log, temporary or backup folder (`logs/api.log`). Kept
/// conservative: a code file in a `tmp/` or `log/` folder may be imported, and is watched.
pub fn watch_ignores(root: &Path, rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase();
    if [".min.js", ".min.mjs", ".min.cjs", ".min.css"]
        .iter()
        .any(|e| name.ends_with(e))
    {
        return true;
    }
    let mut dir = root.to_path_buf();
    let parts: Vec<&str> = rel.split('/').collect();
    let code = crate::lang::detect(rel).is_some();
    for d in &parts[..parts.len().saturating_sub(1)] {
        dir.push(d);
        let lower = d.to_ascii_lowercase();
        if (lower.ends_with("-worktrees") || lower.contains("-worktree-"))
            && sibling_worktrees(&dir, Some(d)).is_some()
        {
            return true;
        }
        if !code && by_name(d).is_some() {
            return true;
        }
    }
    false
}

/// The second rule of `walk::copy_of_repo`: `dir` is `<repo>-worktrees` or
/// `<repo>-worktree-<name>` next to a `<repo>` folder that is a git repository.
pub fn sibling_worktrees(dir: &Path, name: Option<&str>) -> Option<String> {
    let name = name?;
    let repo = name
        .strip_suffix("-worktrees")
        .or_else(|| name.split_once("-worktree-").map(|(r, _)| r))
        .filter(|r| !r.is_empty())?;
    let sibling = dir.parent()?.join(repo);
    sibling
        .join(".git")
        .exists()
        .then(|| format!("worktrees of the sibling repository `{repo}` (copies of it)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(files: &[&str]) -> Vec<String> {
        candidates(files.iter().copied())
            .into_iter()
            .map(|(d, _)| d)
            .collect()
    }

    #[test]
    fn data_folders_by_name_and_by_content() {
        let shots: Vec<String> = (0..6).map(|i| format!("capturas/run1/s{i}.png")).collect();
        let pics: Vec<String> = (0..6).map(|i| format!("docs/img/p{i}.png")).collect();
        let mut files: Vec<&str> = shots
            .iter()
            .chain(pics.iter())
            .map(String::as_str)
            .collect();
        files.extend([
            "src/app.ts",
            "docs/guide.md",
            "src/logs/view.ts",
            "logs/api.log",
            "logs/api.log.1",
            "_backup-2026-01-01/x.json",
            "old.bak/a.ts",
            "src/locales/en.json",
            "src/locales/es.json",
            "src/locales/fr.json",
            "src/locales/de.json",
            "src/locales/it.json",
            "assets/logo.png",
            "src/__snapshots__/a.test.ts.snap",
            ".scannerwork/report-task.txt",
            ".notes/a.md",
            ".notes/b.json",
            ".hooks/check.mjs",
            "web/.cache-like/x.md",
        ]);
        assert_eq!(
            dirs(&files),
            vec![
                ".hooks",
                ".notes",
                ".scannerwork",
                "_backup-2026-01-01",
                "capturas",
                "docs/img",
                "logs",
                "old.bak",
                "src/__snapshots__",
            ]
        );
    }

    #[test]
    fn a_folder_is_kept_when_an_import_points_into_it() {
        let rel = RawImport {
            line: 1,
            spec: Spec::Path("./logs/view".into()),
        };
        let py = RawImport {
            line: 1,
            spec: Spec::Python {
                module: "app.tmp.cache".into(),
                names: vec![],
            },
        };
        let named = Named::of(
            [("src/a.ts", &rel), ("main.py", &py)].into_iter(),
            [("tools", "scratch/run.mjs")].into_iter(),
        );
        // A quoted include goes through include paths too: its words count.
        let inc = RawImport {
            line: 1,
            spec: Spec::Include {
                path: "log/Logger.h".into(),
                system: false,
            },
        };
        let c = Named::of([("src/main.cpp", &inc)].into_iter(), std::iter::empty());
        assert!(c.names("include/log"));
        // The relative import points into `src/logs`, not into a top-level `logs`.
        assert!(named.names("src/logs"));
        assert!(!named.names("logs"));
        assert!(named.names("app/tmp"));
        assert!(named.names("tools/scratch"));
        assert!(!named.names("capturas"));
    }

    #[test]
    fn the_watcher_ignores_output_but_not_code() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("app/.git")).unwrap();
        assert!(watch_ignores(root, "logs/api.log"));
        assert!(watch_ignores(root, "src/coverage/lcov.info"));
        assert!(watch_ignores(root, "vendor/chart.min.js"));
        assert!(watch_ignores(root, "app-worktrees/feat/src/a.ts"));
        assert!(!watch_ignores(root, "lib-worktrees/a.ts"));
        assert!(!watch_ignores(root, "src/log/logger.ts"));
        assert!(!watch_ignores(root, "src/app.ts"));
    }
}
