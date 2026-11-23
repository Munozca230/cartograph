//! The files a `package.json` entry names (`main`, `bin`, `exports`, the scripts' files),
//! shared by the indexer (flow entries) and the query graph (entry-point files).

/// Extensions Node and bundlers try for a `package.json` path written without one.
pub const ENTRY_EXTS: &[&str] = &["js", "mjs", "cjs", "ts", "tsx", "jsx"];

/// `dir/rel` (repo-relative, forward slashes) with empty and `.` segments dropped and `..`
/// applied; `None` when `..` goes above the repo root.
pub fn join(dir: &str, rel: &str) -> Option<String> {
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
    Some(parts.join("/"))
}

/// The files a `package.json` entry (`rel`, relative to the package folder `dir`; `""` is
/// the repo root) names, as Node resolves them: the path itself,
/// `<p>.{js,mjs,cjs,ts,tsx,jsx}`, `<p>/index.*`. A path that names a folder (empty, `.`, a
/// trailing `/` or `/.`) only tries `<p>/index.*`. When none is a file and the entry is a
/// build output, the source it is compiled from ([`output_source`]). `is_file` says whether
/// a repo-relative path is an indexed file.
pub fn entry_files(dir: &str, rel: &str, is_file: impl Fn(&str) -> bool) -> Vec<String> {
    let rel = rel.replace('\\', "/");
    let folder = rel.is_empty() || rel == "." || rel.ends_with('/') || rel.ends_with("/.");
    let Some(p) = join(dir, &rel) else {
        return Vec::new();
    };
    let index = |e: &&str| {
        if p.is_empty() {
            format!("index.{e}")
        } else {
            format!("{p}/index.{e}")
        }
    };
    let at_dir = p == join(dir, "").unwrap_or_default();
    let found: Vec<String> = if folder || at_dir {
        ENTRY_EXTS
            .iter()
            .map(index)
            .filter(|c| is_file(c))
            .collect()
    } else if is_file(&p) {
        vec![p.clone()]
    } else {
        ENTRY_EXTS
            .iter()
            .map(|e| format!("{p}.{e}"))
            .chain(ENTRY_EXTS.iter().map(index))
            .filter(|c| is_file(c))
            .collect()
    };
    if !found.is_empty() || folder || at_dir {
        return found;
    }
    output_source(dir, &rel, &is_file).into_iter().collect()
}

/// The source of a build output that is not indexed (an ignored output, or a compile in
/// place): the output's own path first (`electron/main.js` → `electron/main.ts`, then
/// `src/electron/main.ts`), then with the first folder (an output folder) dropped, then each
/// further leading folder, `src/` first (`dist-electron/electron/main.js` →
/// `src/electron/main.ts`), with the extensions a compiler turns into the output's (`.ts`,
/// `.tsx`, `.js`, `.jsx` for `.js`; `.mts` for `.mjs`; `.cts` for `.cjs`). The first that is
/// a file, or `None`.
fn output_source(dir: &str, rel: &str, is_file: &impl Fn(&str) -> bool) -> Option<String> {
    let parts: Vec<&str> = rel
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if parts.contains(&"..") {
        return None;
    }
    let (last, folders) = parts.split_last()?;
    let (stem, ext) = last.rsplit_once('.')?;
    let exts: &[&str] = match ext {
        "js" => &["ts", "tsx", "js", "jsx"],
        "mjs" => &["mts", "ts", "mjs"],
        "cjs" => &["cts", "ts", "cjs"],
        _ => return None,
    };
    for k in 0..=folders.len() {
        let tail: Vec<&str> = folders[k..].iter().copied().chain([stem]).collect();
        let tail = tail.join("/");
        let bases = if k == 0 { ["", "src/"] } else { ["src/", ""] };
        for base in bases {
            for e in exts {
                let c = join(dir, &format!("{base}{tail}.{e}"))?;
                if is_file(&c) {
                    return Some(c);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files<'a>(list: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
        move |p| list.contains(&p)
    }

    #[test]
    fn node_resolution_first() {
        let f = files(&[
            "app/lib/server.js",
            "app/cli/index.ts",
            "index.js",
            "app/main.ts",
        ]);
        assert_eq!(
            entry_files("app", "lib/server", &f),
            vec!["app/lib/server.js"]
        );
        assert_eq!(entry_files("app", "./cli", &f), vec!["app/cli/index.ts"]);
        assert_eq!(entry_files("app", "cli/", &f), vec!["app/cli/index.ts"]);
        assert_eq!(entry_files("", ".", &f), vec!["index.js"]);
        assert_eq!(entry_files("app", "main.ts", &f), vec!["app/main.ts"]);
        assert!(entry_files("app", "../../x.js", &f).is_empty());
    }

    #[test]
    fn a_build_output_maps_to_its_source() {
        let f = files(&[
            "src/electron/main.ts",
            "pkg/src/index.ts",
            "tool/cli.mts",
            "electron/main.ts",
        ]);
        // The output folder dropped, `src/` tried first.
        assert_eq!(
            entry_files("", "dist-electron/electron/main.js", &f),
            vec!["src/electron/main.ts"]
        );
        // Within the package folder only.
        assert_eq!(
            entry_files("pkg", "dist/index.js", &f),
            vec!["pkg/src/index.ts"]
        );
        assert_eq!(entry_files("tool", "out/cli.mjs", &f), vec!["tool/cli.mts"]);
        // Compiled in place (`tsc` without `outDir`): the output's own folder first, never
        // an unrelated `src/main.ts`.
        let g = files(&["electron/main.ts", "src/main.ts", "main.ts"]);
        assert_eq!(
            entry_files("", "electron/main.js", &g),
            vec!["electron/main.ts"]
        );
        assert_eq!(entry_files("", "main.js", &g), vec!["main.ts"]);
        // Not a JS output, or nothing fits.
        assert!(entry_files("", "dist/main.css", &f).is_empty());
        assert!(entry_files("", "dist/other.js", &f).is_empty());
    }
}
