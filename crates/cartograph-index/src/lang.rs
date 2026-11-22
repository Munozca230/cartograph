//! Language detection by file name and extension.

/// Detect the language of a file from its repo-relative path (forward slashes).
///
/// `.h` is reported as C; telling C and C++ headers apart needs the includers, which
/// arrives with import extraction.
pub fn detect(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name == "project.godot" {
        return Some("godot-project");
    }
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "py" | "pyi" => "python",
        "rs" => "rust",
        "cs" => "csharp",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "gd" => "gdscript",
        "tscn" => "godot-scene",
        "tres" => "godot-resource",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::detect;

    #[test]
    fn detects_by_extension_and_name() {
        let cases = [
            ("a.ts", Some("typescript")),
            ("a.tsx", Some("tsx")),
            ("a.js", Some("javascript")),
            ("a.mjs", Some("javascript")),
            ("a.cjs", Some("javascript")),
            ("a.jsx", Some("jsx")),
            ("a.py", Some("python")),
            ("a.rs", Some("rust")),
            ("a.cs", Some("csharp")),
            ("a.go", Some("go")),
            ("A.java", Some("java")),
            ("a.c", Some("c")),
            ("a.h", Some("c")),
            ("a.cpp", Some("cpp")),
            ("a.cc", Some("cpp")),
            ("a.cxx", Some("cpp")),
            ("a.hpp", Some("cpp")),
            ("a.hh", Some("cpp")),
            ("a.gd", Some("gdscript")),
            ("scenes/main.tscn", Some("godot-scene")),
            ("res/theme.tres", Some("godot-resource")),
            ("game/project.godot", Some("godot-project")),
            ("UPPER.PY", Some("python")),
            ("README.md", None),
            ("Makefile", None),
            (".gitignore", None),
        ];
        for (path, want) in cases {
            assert_eq!(detect(path), want, "{path}");
        }
    }
}
