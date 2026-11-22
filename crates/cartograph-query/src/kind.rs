//! What a file is (source code, tests, scripts, styles, or config/data/docs), from its path
//! alone. This is the Rust port of `web/src/model/kind.ts` and must follow the same rules:
//! the plan colours cards with that file and `impact` picks the tests to run with this one.
//! Generic rules only: no repository is special-cased.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Code,
    Test,
    Script,
    Style,
    Data,
}

impl FileKind {
    /// In legend order; also the tie-break order of a folder's dominant kind.
    pub const ALL: [FileKind; 5] = [
        FileKind::Code,
        FileKind::Test,
        FileKind::Script,
        FileKind::Style,
        FileKind::Data,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Code => "code",
            FileKind::Test => "test",
            FileKind::Script => "script",
            FileKind::Style => "style",
            FileKind::Data => "data",
        }
    }
}

const CODE_EXT: &[&str] = &[
    "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "vue", "svelte", "astro", "py", "pyi",
    "rs", "go", "java", "kt", "kts", "scala", "groovy", "c", "h", "cpp", "cc", "cxx", "hpp", "hh",
    "hxx", "cs", "fs", "vb", "gd", "tscn", "gdshader", "shader", "glsl", "hlsl", "wgsl", "rb",
    "php", "swift", "m", "mm", "dart", "lua", "ex", "exs", "erl", "hs", "ml", "clj", "elm", "r",
    "jl", "zig", "nim", "html", "htm",
];
const STYLE_EXT: &[&str] = &["css", "scss", "sass", "less", "styl", "pcss"];
const SCRIPT_EXT: &[&str] = &["sh", "bash", "zsh", "fish", "ps1", "psm1", "bat", "cmd"];
/// Folders whose files are tests, wherever they are.
const TEST_DIRS: &[&str] = &[
    "test",
    "tests",
    "__tests__",
    "__test__",
    "e2e",
    "__mocks__",
    "__fixtures__",
    "__snapshots__",
];
/// Folders whose code files are tests (RSpec, Jasmine).
const SPEC_DIRS: &[&str] = &["spec", "specs"];
const SPEC_SUPPORT: &[&str] = &["fixtures", "support", "mocks", "factories", "helpers"];
/// Words that end in IT: `OrderAUDIT` is an audit, not an integration test.
const IT_WORDS: &[&str] = &[
    "AUDIT", "BIT", "EDIT", "UNIT", "LIMIT", "SUBMIT", "COMMIT", "CREDIT", "DEBIT", "EXIT", "INIT",
    "SPLIT", "KIT", "HIT", "FIT", "DIGIT", "DEPOSIT", "PROFIT", "VISIT", "PERMIT", "TRANSIT",
    "OMIT", "EMIT", "REMIT", "QUIT", "SUIT", "WIT",
];
/// Godot code: never a "script" for being in `scripts/`.
const GODOT_EXT: &[&str] = &["gd", "tscn", "gdshader"];
const SCRIPT_DIRS: &[&str] = &["scripts", "script"];
/// Script folders only at the top level (inside a source tree they are usually code).
const TOP_SCRIPT_DIRS: &[&str] = &["bin", "tools"];
const SCRIPT_NAMES: &[&str] = &["makefile", "justfile", "rakefile", "gnumakefile"];
const CONFIG_CODE_EXT: &[&str] = &["js", "cjs", "mjs", "ts", "cts", "mts"];

fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[i + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// `vite.config.ts`, `jest.setup.js`, `.eslintrc.cjs`…
fn config_code(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let Some((stem, ext)) = lower.rsplit_once('.') else {
        return false;
    };
    if !CONFIG_CODE_EXT.contains(&ext) {
        return false;
    }
    if [".config", ".conf", ".setup"]
        .iter()
        .any(|s| stem.ends_with(s))
    {
        return true;
    }
    stem.len() >= 3
        && stem.starts_with('.')
        && stem.ends_with("rc")
        && stem[1..]
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// `a.test.ts`, `test_a.py`, `a_test.go`, `ATest.java`, `a_spec.rb`…
fn test_file(name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() >= 3 {
        let marker = parts[parts.len() - 2];
        if ["test", "spec", "e2e", "bench"].contains(&marker) && !parts[parts.len() - 1].is_empty()
        {
            return true;
        }
    }
    if let Some(stem) = name.strip_suffix(".py")
        && stem.len() > 5
        && stem.starts_with("test_")
    {
        return true;
    }
    if name.ends_with("_test.py") || name.ends_with("_test.go") || name.ends_with("_spec.rb") {
        return true;
    }
    for ext in [".java", ".kt", ".cs"] {
        if let Some(stem) = name.strip_suffix(ext)
            && (stem.ends_with("Test") || stem.ends_with("Tests"))
        {
            return true;
        }
    }
    false
}

/// A Java/Kotlin/C# integration test: `UserServiceIT`, `S3IT`, not `AUDIT` or `OrderAUDIT`.
fn integration_test(name: &str) -> bool {
    let Some(stem) = [".java", ".kt", ".cs"]
        .iter()
        .find_map(|e| name.strip_suffix(e))
    else {
        return false;
    };
    let Some(before) = stem.strip_suffix("IT") else {
        return false;
    };
    // `caps` = the run of capitals right before IT; `head` = the rest.
    let caps_start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_uppercase())
        .last()
        .map_or(before.len(), |(i, _)| i);
    let (head, caps) = before.split_at(caps_start);
    let has_lower_or_digit = head
        .chars()
        .chain(caps.chars())
        .any(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if !has_lower_or_digit {
        return false;
    }
    caps.is_empty() || !IT_WORDS.contains(&format!("{caps}IT").as_str())
}

/// The kind of a file from its repo-relative path (forward slashes).
pub fn file_kind(path: &str) -> FileKind {
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let name = parts.last().copied().unwrap_or("");
    let dirs: Vec<String> = parts[..parts.len().saturating_sub(1)]
        .iter()
        .map(|d| d.to_ascii_lowercase())
        .collect();
    let ext = ext_of(name);
    if test_file(name)
        || integration_test(name)
        || dirs.iter().any(|d| TEST_DIRS.contains(&d.as_str()))
    {
        return FileKind::Test;
    }
    if let Some(spec) = dirs.iter().position(|d| SPEC_DIRS.contains(&d.as_str())) {
        if dirs[spec + 1..]
            .iter()
            .any(|d| SPEC_SUPPORT.contains(&d.as_str()))
        {
            return FileKind::Test;
        }
        if CODE_EXT.contains(&ext.as_str()) && !config_code(name) {
            return FileKind::Test;
        }
    }
    // Godot keeps its game code in `scripts/`: GDScript and scenes there are code.
    if (!GODOT_EXT.contains(&ext.as_str())
        && dirs.iter().any(|d| SCRIPT_DIRS.contains(&d.as_str())))
        || dirs
            .first()
            .is_some_and(|d| TOP_SCRIPT_DIRS.contains(&d.as_str()))
    {
        return FileKind::Script;
    }
    if SCRIPT_EXT.contains(&ext.as_str())
        || SCRIPT_NAMES.contains(&name.to_ascii_lowercase().as_str())
    {
        return FileKind::Script;
    }
    if STYLE_EXT.contains(&ext.as_str()) {
        return FileKind::Style;
    }
    if config_code(name) {
        return FileKind::Data;
    }
    if dirs.is_empty() && (ext == "mjs" || ext == "cjs") {
        return FileKind::Script;
    }
    if CODE_EXT.contains(&ext.as_str()) {
        return FileKind::Code;
    }
    FileKind::Data
}

/// Whether a path is a test file (what `impact` lists under "tests to run").
pub fn is_test(path: &str) -> bool {
    file_kind(path) == FileKind::Test
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same cases as `web/test/kind.test.ts`, so both sides agree.
    #[test]
    fn matches_the_web_rules() {
        let cases = [
            ("src/a.ts", FileKind::Code),
            ("src/a.test.ts", FileKind::Test),
            ("src/a.spec.tsx", FileKind::Test),
            ("src/__tests__/a.ts", FileKind::Test),
            ("tests/util.py", FileKind::Test),
            ("pkg/test_util.py", FileKind::Test),
            ("pkg/util_test.go", FileKind::Test),
            ("src/main/java/x/UserServiceTest.java", FileKind::Test),
            ("src/main/java/x/UserServiceIT.java", FileKind::Test),
            ("src/main/java/x/S3IT.java", FileKind::Test),
            ("src/main/java/x/OrderAUDIT.java", FileKind::Code),
            ("src/main/java/x/AUDIT.java", FileKind::Code),
            ("src/main/java/x/IT.java", FileKind::Code),
            ("spec/models/user_spec.rb", FileKind::Test),
            ("spec/openapi.yaml", FileKind::Data),
            ("spec/support/x.yaml", FileKind::Test),
            ("scripts/build.ts", FileKind::Script),
            ("bin/run.js", FileKind::Script),
            ("src/bin/main.rs", FileKind::Code),
            ("deploy.sh", FileKind::Script),
            ("Makefile", FileKind::Script),
            ("src/app.css", FileKind::Style),
            ("vite.config.ts", FileKind::Data),
            (".eslintrc.cjs", FileKind::Data),
            ("jest.setup.js", FileKind::Data),
            ("build.mjs", FileKind::Script),
            ("src/x.mjs", FileKind::Code),
            ("package.json", FileKind::Data),
            ("README.md", FileKind::Data),
            ("scenes/main.tscn", FileKind::Code),
            ("scripts/view/dig_hud.gd", FileKind::Code),
            ("tools/capture.gd", FileKind::Script),
        ];
        for (path, want) in cases {
            assert_eq!(file_kind(path), want, "{path}");
        }
    }

    #[test]
    fn odd_names_do_not_panic() {
        for p in [
            "",
            "/",
            ".",
            "..",
            "a.",
            ".test.ts",
            "IT.java",
            "é.test.ts",
            "x/ñIT.java",
        ] {
            let _ = file_kind(p);
        }
    }
}
