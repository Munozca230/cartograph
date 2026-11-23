//! File-system walk: respects `.gitignore` (including nested ones), always skips a fixed
//! set of directories, the checkouts of other repositories (a folder below the root
//! holding a `.git` file: a submodule, a linked worktree; a `<repo>-worktrees` folder next
//! to `<repo>`) and the notes file of `annotate`, reads files in parallel (parsing imports
//! on the same rayon threads), and records every path it does not index.

use crate::imports::{self, RawImport};
use crate::lang;
use crate::manifest::{self, Manifest};
use cartograph_core::{SkipReason, SkippedFile};
use ignore::WalkBuilder;
use rayon::prelude::*;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Directory names that are never walked, at any depth. Each one found is recorded as
/// skipped with [`SkipReason::ExcludedDir`]. `.codevgraph` is the data dir of the project's
/// former name, still excluded so an old index is never walked. `.godot` is the Godot 4
/// editor's cache (imported assets, class and uid caches), regenerated from the project.
pub const EXCLUDED_DIRS: [&str; 6] = [
    ".git",
    ".cartograph",
    ".codevgraph",
    "node_modules",
    "target",
    ".godot",
];

/// How many leading bytes are checked for NUL to classify a file as binary.
pub const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// Import extraction slower than this (per file) is logged as a warning.
pub const SLOW_PARSE_MS: u128 = 1000;

/// Default size limit: files larger than this are skipped as [`SkipReason::TooLarge`].
pub const DEFAULT_MAX_FILE_BYTES: u64 = 1024 * 1024;

/// One indexed text file.
#[derive(Debug, Clone)]
pub struct FileRecord {
    /// Repo-relative path with forward slashes.
    pub path: String,
    pub lang: Option<&'static str>,
    pub loc: u64,
    pub bytes: u64,
    /// BLAKE3 hex digest.
    pub hash: String,
    /// Set when the file is a recognised manifest.
    pub manifest: Option<Manifest>,
    /// Imports found by the N1 extractor (TS/JS and Python), parsed during the read.
    pub imports: Vec<RawImport>,
    /// Symbols, calls, bindings and entry points (N2), from the same parse.
    pub code: Option<crate::symbols::Code>,
    /// Text of a config file the resolvers need (see [`is_resolver_config`]).
    pub config: Option<String>,
    /// `true` when `imports` and `code` came from the extraction cache instead of a parse.
    pub cached: bool,
}

/// Extractions (raw imports and symbols) by [`cache_key`], from a previous run (see
/// [`crate::index_to_store`]).
pub type ExtractCache = HashMap<String, imports::Extract>;

/// Key of a file's entry in an [`ExtractCache`]: language and content hash.
pub fn cache_key(lang: &str, hash: &str) -> String {
    format!("{lang}|{hash}")
}

/// Optional extras for [`walk`].
#[derive(Debug, Default, Clone, Copy)]
pub struct WalkHooks<'a> {
    /// Reuse these extractions for files whose language and hash match.
    pub cache: Option<&'a ExtractCache>,
    /// Updated as files are read.
    pub progress: Option<&'a crate::Progress>,
}

/// Files whose text the import resolvers read: `package.json`, `pnpm-workspace.yaml`, and
/// `tsconfig*.json` / `jsconfig*.json`.
pub fn is_resolver_config(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name == "package.json"
        || name == "pnpm-workspace.yaml"
        || ((name.starts_with("tsconfig") || name.starts_with("jsconfig"))
            && name.ends_with(".json"))
}

/// Result of a walk.
#[derive(Debug, Default)]
pub struct WalkOutput {
    pub files: Vec<FileRecord>,
    /// Repo-relative paths of every walked directory (the root excluded).
    pub dirs: Vec<String>,
    pub skipped: Vec<SkippedFile>,
    pub walk_ms: u128,
    pub read_ms: u128,
}

/// Walk `root` and read every candidate file in parallel. `extra_excluded` holds
/// repo-relative paths to skip as well (a custom data dir inside the repo, or the database
/// files when the data dir is the repo root); they are recorded as
/// [`SkipReason::ExcludedDir`] like the fixed dirs.
pub fn walk(
    root: &Path,
    max_file_bytes: u64,
    extra_excluded: &[String],
    hooks: WalkHooks<'_>,
) -> WalkOutput {
    let started = std::time::Instant::now();
    let excluded: Arc<Mutex<Vec<(String, Option<String>)>>> = Arc::default();
    let excluded_sink = Arc::clone(&excluded);
    let filter_root = root.to_path_buf();
    let extra: Vec<String> = extra_excluded.to_vec();

    let walker = WalkBuilder::new(root)
        .hidden(false)
        .parents(false)
        .require_git(false)
        .git_global(false)
        .follow_links(false)
        .filter_entry(move |entry| {
            let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
            let name = entry.file_name().to_str();
            let excluded_name = name.is_some_and(|n| EXCLUDED_DIRS.contains(&n));
            // A `.git` *file* is the pointer of a linked worktree or submodule to its git
            // dir: git metadata, like the `.git` dir.
            let git_file = !is_dir && name == Some(".git");
            // The repo's own notes file (`annotate`): never part of the map, so writing a
            // note changes no hash.
            let notes = !is_dir
                && entry.depth() == 1
                && name.is_some_and(|n| crate::NOTES_FILES.contains(&n));
            if entry.depth() == 0 || (!is_dir && !git_file && !notes && extra.is_empty()) {
                return true;
            }
            let rel = relative_escaped(&filter_root, entry.path());
            let detail = if is_dir && !excluded_name {
                copy_of_repo(entry.path(), name)
            } else if notes {
                Some("notes of `annotate`, read by `explain`".to_string())
            } else {
                None
            };
            if (is_dir && excluded_name)
                || git_file
                || notes
                || detail.is_some()
                || extra.contains(&rel)
            {
                if let Ok(mut v) = excluded_sink.lock() {
                    v.push((rel, detail));
                }
                return false;
            }
            true
        })
        .build();

    let mut out = WalkOutput::default();
    let mut candidates: Vec<(PathBuf, String)> = Vec::new();

    let cancelled = || hooks.progress.is_some_and(|p| p.cancelled());
    for result in walker {
        if cancelled() {
            break;
        }
        let entry = match result {
            Ok(e) => e,
            Err(err) => {
                // Errors without a path are recorded against the repo root (""); several of
                // them merge into one entry (see `merge_skipped`).
                let path = error_path(&err)
                    .map(|p| relative_escaped(root, p))
                    .unwrap_or_default();
                tracing::warn!(%path, error = %err, "walk error");
                out.skipped.push(SkippedFile {
                    path,
                    reason: SkipReason::WalkError,
                    detail: Some(err.to_string()),
                });
                continue;
            }
        };
        // `ignore` attaches problems reading a directory's ignore files (e.g. a bad glob
        // in `.gitignore`) to an otherwise successful entry. The valid lines still apply.
        if let Some(err) = entry.error() {
            let path = error_path(err)
                .map(|p| relative_escaped(root, p))
                .unwrap_or_else(|| relative_escaped(root, entry.path()));
            tracing::warn!(%path, error = %err, "ignore file error");
            out.skipped.push(SkippedFile {
                path,
                reason: SkipReason::ParseError,
                detail: Some(format!("ignore rules: {err}")),
            });
        }
        if entry.depth() == 0 {
            continue;
        }
        let Some(rel) = relative(root, entry.path()) else {
            out.skipped.push(SkippedFile {
                path: relative_escaped(root, entry.path()),
                reason: SkipReason::InvalidPath,
                detail: Some("path is not valid Unicode (shown escaped)".into()),
            });
            continue;
        };
        match entry.file_type() {
            Some(t) if t.is_dir() => out.dirs.push(rel),
            Some(t) if t.is_file() => candidates.push((entry.into_path(), rel)),
            Some(t) if t.is_symlink() => out.skipped.push(SkippedFile {
                path: rel,
                reason: SkipReason::Symlink,
                detail: None,
            }),
            _ => out.skipped.push(SkippedFile {
                path: rel,
                reason: SkipReason::ReadError,
                detail: Some("not a regular file".into()),
            }),
        }
    }
    if let Ok(mut v) = excluded.lock() {
        out.skipped
            .extend(v.drain(..).map(|(path, detail)| SkippedFile {
                path,
                reason: SkipReason::ExcludedDir,
                detail,
            }));
    }
    out.walk_ms = started.elapsed().as_millis();

    let started = std::time::Instant::now();
    if let Some(p) = hooks.progress {
        p.start(candidates.len() as u64);
    }
    let results: Vec<Result<FileRecord, SkippedFile>> = candidates
        .par_iter()
        .map(|(abs, rel)| {
            if cancelled() {
                // The caller discards a cancelled walk; skip the work.
                return Err(SkippedFile {
                    path: rel.clone(),
                    reason: SkipReason::ReadError,
                    detail: Some("cancelled".into()),
                });
            }
            let r = read_file(abs, rel, max_file_bytes, hooks.cache);
            if let Some(p) = hooks.progress {
                p.advance();
            }
            r
        })
        .collect();
    for r in results {
        match r {
            Ok(f) => out.files.push(f),
            Err(s) => out.skipped.push(s),
        }
    }
    out.read_ms = started.elapsed().as_millis();

    out.files.sort_by(|a, b| a.path.cmp(&b.path));
    out.dirs.sort();
    out.skipped.sort();
    out
}

/// Why the folder `dir` (below the root) is a copy of a repository, if it is one:
/// - it holds a `.git` file: the checkout of another repository, an initialized submodule
///   (one gitlink entry in this repository's tree, never its files) or a linked worktree;
/// - it is named `<repo>-worktrees` or `<repo>-worktree-<name>` next to a `<repo>` folder
///   that is a git repository: the conventional home of that repository's linked worktrees
///   (and of copies left over by them), seen when the folder above both is indexed.
fn copy_of_repo(dir: &Path, name: Option<&str>) -> Option<String> {
    if dir.join(".git").is_file() {
        return Some(
            "checkout of another repository (`.git` is a file: a linked worktree or a submodule)"
                .into(),
        );
    }
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

fn read_file(
    abs: &Path,
    rel: &str,
    max_file_bytes: u64,
    cache: Option<&ExtractCache>,
) -> Result<FileRecord, SkippedFile> {
    let skip = |reason, detail: String| SkippedFile {
        path: rel.to_string(),
        reason,
        detail: Some(detail),
    };
    let len = std::fs::metadata(abs)
        .map_err(|e| skip(SkipReason::ReadError, e.to_string()))?
        .len();
    if len > max_file_bytes {
        return Err(skip(
            SkipReason::TooLarge,
            format!("{len} bytes > limit {max_file_bytes}"),
        ));
    }
    let read_error = |e: std::io::Error| {
        tracing::warn!(path = rel, error = %e, "read error");
        skip(SkipReason::ReadError, e.to_string())
    };
    // Read at most limit + 1 bytes: the file may have grown since `metadata`.
    let file = std::fs::File::open(abs).map_err(read_error)?;
    let content = read_capped(file, max_file_bytes)
        .map_err(read_error)?
        .ok_or_else(|| {
            skip(
                SkipReason::TooLarge,
                format!("grew past limit {max_file_bytes} bytes while reading"),
            )
        })?;
    let sniff = &content[..content.len().min(BINARY_SNIFF_BYTES)];
    if sniff.contains(&0) {
        return Err(skip(
            SkipReason::Binary,
            format!("NUL byte in first {} bytes", BINARY_SNIFF_BYTES),
        ));
    }
    let lang = lang::detect(rel);
    let hash = blake3::hash(&content).to_hex().to_string();
    let hit = match (lang, cache) {
        (Some(l), Some(c)) => c.get(&cache_key(l, &hash)).cloned(),
        _ => None,
    };
    let cached = hit.is_some();
    let extract = match hit {
        Some(v) => v,
        None => {
            let parse_started = std::time::Instant::now();
            let x = lang.map_or_else(imports::Extract::default, |l| {
                imports::extract_file(l, &content)
            });
            let parse_ms = parse_started.elapsed().as_millis();
            if parse_ms >= SLOW_PARSE_MS {
                tracing::warn!(path = rel, ms = parse_ms, "slow extraction");
            }
            x
        }
    };
    Ok(FileRecord {
        path: rel.to_string(),
        lang,
        imports: extract.imports,
        code: extract.code,
        cached,
        config: is_resolver_config(rel).then(|| String::from_utf8_lossy(&content).into_owned()),
        loc: count_loc(&content),
        bytes: content.len() as u64,
        hash,
        manifest: manifest::detect(rel).map(|kind| manifest::parse(rel, kind, &content)),
    })
}

/// Non-blank lines. A line holding only whitespace (including a lone `\r`) is blank.
/// Works on bytes, so text that is not valid UTF-8 is still counted.
pub fn count_loc(content: &[u8]) -> u64 {
    content
        .split(|&b| b == b'\n')
        .filter(|line| line.iter().any(|b| !b.is_ascii_whitespace()))
        .count() as u64
}

/// Read everything from `r` unless it holds more than `max` bytes (then `None`). Never
/// buffers more than `max + 1` bytes.
pub fn read_capped<R: Read>(r: R, max: u64) -> std::io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    r.take(max.saturating_add(1)).read_to_end(&mut buf)?;
    Ok((buf.len() as u64 <= max).then_some(buf))
}

/// Repo-relative path with forward slashes, or `None` if a component is not valid Unicode.
pub fn relative(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

/// Repo-relative path with forward slashes that never fails: components that are not valid
/// Unicode are escaped losslessly (`%XX` for stray bytes on Unix, `%uXXXX` for unpaired
/// surrogates on Windows, `%25` for a literal `%` in such a component), so two different
/// invalid names never map to the same string.
pub fn relative_escaped(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_str().map_or_else(|| escape_os(s), str::to_string)),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(windows)]
fn escape_os(s: &OsStr) -> String {
    use std::os::windows::ffi::OsStrExt;
    let mut out = String::new();
    for unit in char::decode_utf16(s.encode_wide()) {
        match unit {
            Ok('%') => out.push_str("%25"),
            Ok(c) => out.push(c),
            Err(e) => out.push_str(&format!("%u{:04X}", e.unpaired_surrogate())),
        }
    }
    out
}

#[cfg(unix)]
fn escape_os(s: &OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for chunk in s.as_bytes().utf8_chunks() {
        out.push_str(&chunk.valid().replace('%', "%25"));
        for b in chunk.invalid() {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(not(any(windows, unix)))]
fn escape_os(s: &OsStr) -> String {
    s.to_string_lossy().into_owned()
}

fn error_path(err: &ignore::Error) -> Option<&Path> {
    match err {
        ignore::Error::WithPath { path, .. } => Some(path),
        ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
            error_path(err)
        }
        ignore::Error::Partial(errs) => errs.iter().find_map(error_path),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loc_ignores_blank_and_whitespace_lines() {
        assert_eq!(count_loc(b""), 0);
        assert_eq!(count_loc(b"a\n\n  \nb"), 2);
        assert_eq!(count_loc(b"a\r\n\r\n\t\r\nb\r\n"), 2);
        assert_eq!(count_loc(b"\xff\xfe x\n"), 1);
    }

    #[test]
    fn reads_are_capped_even_if_the_file_grew() {
        let data = vec![b'x'; 100];
        assert_eq!(read_capped(&data[..], 100).unwrap(), Some(data.clone()));
        assert_eq!(read_capped(&data[..], 99).unwrap(), None);
    }

    #[test]
    fn relative_uses_forward_slashes() {
        let root = Path::new("repo");
        let p = root.join("src").join("módulo extra").join("útil.py");
        assert_eq!(
            relative(root, &p).as_deref(),
            Some("src/módulo extra/útil.py")
        );
    }

    #[test]
    fn copies_of_a_repository_are_skipped_with_a_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let write = |rel: &str, text: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        // A folder of repositories: `app` and the home of its linked worktrees.
        std::fs::create_dir_all(root.join("app/.git")).unwrap();
        write("app/src/a.ts", "export const a = 1;\n");
        write(
            "app-worktrees/feat/.git",
            "gitdir: ../../app/.git/worktrees/feat\n",
        );
        write("app-worktrees/feat/src/a.ts", "export const a = 2;\n");
        write("app-worktrees/left-over.log", "x\n");
        write("app-worktree-old/src/a.ts", "export const a = 3;\n");
        // A nested linked worktree inside a repo.
        write("app/wt/.git", "gitdir: x\n");
        write("app/wt/b.ts", "export const b = 1;\n");
        // No `lib` repository next to it: an ordinary folder.
        write("lib-worktrees/c.ts", "export const c = 1;\n");
        let out = walk(root, DEFAULT_MAX_FILE_BYTES, &[], WalkHooks::default());
        let files: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(files, vec!["app/src/a.ts", "lib-worktrees/c.ts"]);
        let reason = |p: &str| {
            out.skipped
                .iter()
                .find(|s| s.path == p)
                .and_then(|s| s.detail.clone())
                .unwrap_or_default()
        };
        assert!(reason("app-worktrees").contains("worktrees of the sibling repository `app`"));
        assert!(reason("app-worktree-old").contains("`app`"));
        assert!(reason("app/wt").contains("checkout of another repository"));
    }
}
