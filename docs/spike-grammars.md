# Spike: tree-sitter grammars

Date: 2026-11-19. Machine: Windows 11 Pro, Rust 1.99.1 (`x86_64-pc-windows-msvc`), 8 cores.

Question: can every grammar we promise (TS/TSX, JS, Python, Rust, C#, Go, Java, C, C++, GDScript) be linked into one binary against a single tree-sitter core, and is there a maintained GDScript grammar?

**Answer: yes.** All 11 grammars build, link and load into `tree-sitter 0.27.2`. Each one parses an idiomatic snippet with no ERROR or MISSING nodes. The code is in `crates/cartograph-index/src/grammars.rs` and the test is `crates/cartograph-index/tests/grammars.rs`.

## Versions

Versions were picked with `cargo add` / `cargo search`, not from memory. The ABI column comes from `Language::abi_version()` at runtime (`cargo test -p cartograph-index --test grammars print_abi_versions -- --nocapture`). Release dates come from the crates.io API.

| Language | Crate | Version | Released | ABI | Status |
|---|---|---|---|---|---|
| (core) | `tree-sitter` | 0.27.2 | 2026-11-07 | accepts 13..=15 | ok |
| TypeScript | `tree-sitter-typescript` (`LANGUAGE_TYPESCRIPT`) | 0.23.2 | 2024-11-11 | 14 | ok, snippet parses clean |
| TSX | `tree-sitter-typescript` (`LANGUAGE_TSX`) | 0.23.2 | 2024-11-11 | 14 | ok, snippet parses clean |
| JavaScript / JSX | `tree-sitter-javascript` | 0.25.0 | 2025-09-01 | 15 | ok, snippet (with JSX) parses clean |
| Python | `tree-sitter-python` | 0.25.0 | 2025-09-11 | 15 | ok, snippet parses clean |
| Rust | `tree-sitter-rust` | 0.24.2 | 2026-03-27 | 15 | ok, snippet parses clean |
| C# | `tree-sitter-c-sharp` | 0.24.0 | 2026-11-18 | 15 | ok, snippet parses clean |
| Go | `tree-sitter-go` | 0.25.0 | 2025-08-29 | 15 | ok, snippet (generics) parses clean |
| Java | `tree-sitter-java` | 0.23.5 | 2024-12-21 | 14 | ok, snippet (records, pattern switch) parses clean |
| C | `tree-sitter-c` | 0.24.2 | 2026-04-22 | 15 | ok, snippet parses clean |
| C++ | `tree-sitter-cpp` | 0.23.4 | 2024-11-11 | 14 | ok, snippet (templates, structured bindings) parses clean |
| GDScript | `tree-sitter-gdscript` | 6.1.0 | 2025-11-03 | 14 | ok, Godot 4 snippet (typed vars, `@export`, `@onready`, property setter, `await`, signals) parses clean |

## ABI and version notes

- No grammar crate depends on `tree-sitter` directly. All of them depend on the `tree-sitter-language` shim (0.1.9) and export a `LanguageFn` constant, so Cargo resolves a single `tree-sitter` core with no version conflict. `cargo tree -d` reports no duplicate tree-sitter crates (the only duplicate in the workspace is `syn` 2 and 3, pulled in by proc macros).
- Core 0.27.2 accepts grammar ABI 13 through 15. The grammars use ABI 14 (TS/TSX, Java, C++, GDScript) or ABI 15 (JS, Python, Rust, C#, Go, C), and both load. `set_language` would fail if a grammar were outside that range, and the test asserts it succeeds for every grammar.
- MSRV: `tree-sitter` 0.27 and `tree-sitter-language` 0.1.9 require Rust 1.90. The workspace sets `rust-version = "1.90"`. With an MSRV of 1.85, the resolver falls back to `tree-sitter` 0.26.13. That combination was resolved but **not built or tested**.
- Risk: three grammars (TypeScript, Java and C++) have not had a release since late 2024, and they still use ABI 14. If a future core drops ABI 14, they will stop loading. The `every_grammar_loads_into_the_same_core` test fails loudly in that case. The fallback is to pin the core, or to vendor the grammar's `src/` and regenerate it with the tree-sitter CLI.
- The test also checks a deliberately broken snippet, so the ERROR/MISSING checker cannot pass vacuously.

## GDScript

- Crate: `tree-sitter-gdscript` 6.1.0 on crates.io (MIT), repository <https://github.com/prestonknopp/tree-sitter-gdscript>. It ships `parser.c` plus an external `scanner.c` (for indentation), compiled with `cc` as C11.
- Maintenance: there were releases in 2025 (4.0.3 on 2025-01-28, 5.0.1 on 2025-05-14, 6.1.0 on 2025-11-03), with about 225k downloads. There has been no release in the last 12 months, so it counts as maintained, but not actively.
- It is the only standalone tree-sitter GDScript grammar crate we found. We checked all 131 crates.io results for `gdscript` and filtered them by "tree"/"sitter" in the name or description. Everything else is a consumer that bundles grammars (`inkjet`, `gry-parse`, `astlens`, `verdant-parsers`), or one of the `gdscript-*` analyzer crates, which use their own cstree parser and are not tree-sitter.
- The same author publishes **`tree-sitter-godot-resource` 0.7.0** (2025-05-14, about 11k downloads, <https://github.com/prestonknopp/tree-sitter-godot-resource>), a grammar for `.tscn`, `.tres` and `project.godot`. It is a candidate for the Godot parser. It is not linked yet, so its ABI is untested.
- Fallback if it goes stale: use a git dependency on the upstream repo, or vendor `src/parser.c` and `src/scanner.c` with a small `build.rs` using `cc`. The `tree-sitter-language` shim makes either option a drop-in replacement.
- `.tscn`, `.tres` and `project.godot` are not GDScript. They get a dedicated parser; the first version only detected them by extension or name, and `project.godot` is parsed as a manifest (name, main scene, autoloads).

## Build notes (Windows)

- **A C compiler is required at build time.** Every grammar, the tree-sitter core and SQLite (`rusqlite` with the `bundled` feature) compile C sources through the `cc` crate.
- Found on this machine: **Visual Studio Build Tools 2026** (MSVC 14.51.36231, `C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools`). `cl.exe` is **not** on `PATH` and `INCLUDE` is unset. `cc` located MSVC on its own, so no Developer Prompt was needed. Presumably it used the VS setup API / vswhere; that is inferred, not traced. All builds in this spike ran from Git Bash. PowerShell builds were not tested.
- MSVC `cl.exe` did the compiling. This is confirmed by MSVC-specific diagnostics in the build-script output (`target/*/build/tree-sitter-*/output`). Those diagnostics are **benign C warnings in the grammar sources**, mostly in the hand-written external scanners: C4244/C4267 (narrowing conversions), C4100 (unused parameter), C4189 and C4456. The counts per crate are typescript 34, javascript 16, c-sharp 8, rust 7, gdscript 6, cpp 3 and python 3. The tree-sitter core, `tree-sitter-c`, `tree-sitter-go`, `tree-sitter-java` and SQLite emitted none. None of these warnings are errors, and Cargo does not surface them.
- Clean release build of the native parts, from the `cargo build --release --timings` run on 8 cores: `libsqlite3-sys` build script 64 s, tree-sitter-typescript 15 s, c-sharp 14 s, javascript 9 s, the tree-sitter core 8 s, rust 8 s, cpp 6 s, java, python and go 3–4 s each, gdscript and c about 2 s each. Total wall time for the CLI release build after cleaning these crates: about 1 min 47 s. A full release rebuild with the static CRT (below) took about 2 min 6 s. The release binary is about 22 MB, with all grammars and SQLite included.

### Install risk (requirement: zero system dependencies at runtime)

- **Runtime:** none. The binary statically contains SQLite and all grammars. It needs no Node, Python, Docker or system SQLite. `cartograph doctor` checks that the grammars load and that SQLite opens in WAL mode.
- **MSVC C runtime:** by default an `x86_64-pc-windows-msvc` build imports `VCRUNTIME140.dll` plus the `api-ms-win-crt-*` UCRT DLLs. We checked this with `grep -a -o -i "vcruntime...dll" target/release/cartograph.exe` on the first release build. A machine without the VC++ Redistributable would then fail to start the binary. **Fixed:** `.cargo/config.toml` sets `-C target-feature=+crt-static` for that target (and `cc` then compiles the C code with `/MT`). After rebuilding, the same grep finds no `vcruntime`, `msvcp` or `api-ms-win-crt` imports, and `cartograph doctor` passes with the static binary.
- **Build from source (`cargo install`):** on Windows this needs the MSVC C/C++ build tools, which the MSVC Rust toolchain needs anyway for linking. On Linux and macOS it needs a C compiler (`cc` / Xcode CLT). Nothing unusual is needed beyond that: no CMake, no tree-sitter CLI, no Node (the grammars ship pre-generated `parser.c`).
- Mitigation for users without build tools: ship prebuilt binaries in releases (later phase).
- Trap seen elsewhere, not hit here: Git Bash's `/usr/bin/link.exe` can shadow the MSVC linker if `cc` or `rustc` fall back to `PATH`. Here, both found MSVC without it being on `PATH` (presumably through the VS setup API; inferred).
