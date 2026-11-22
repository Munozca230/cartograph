Golden fixture for Rust import resolution (see crates/cartograph-index/tests/imports_native_golden.rs).
Expected output: tests/golden/rust-workspace.json. Some imports are broken on purpose.
A workspace with three crates (core-model, app-cli, local-util), nested file and inline
modules, #[path], re-exports, a renamed path dependency, a module no `mod` reaches and a
CRLF file.
