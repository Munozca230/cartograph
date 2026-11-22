//! Embeds the web frontend (`web/dist`) into the binary when it exists at build time.
//!
//! Writes `$OUT_DIR/web_assets.rs` with a static table of `(path, bytes)` built from
//! `include_bytes!`. Without `web/dist` (no Node installed, frontend never built) the table
//! is empty and the server serves a fallback page instead: the build never fails because
//! of the frontend. `CARTOGRAPH_WEB_DIST` points the build at another folder.
//!
//! Cargo only rescans `web/dist` when it existed during the previous build. After building
//! the frontend for the first time, touch this file (or `cargo clean -p cartograph-server`).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=CARTOGRAPH_WEB_DIST");
    let dist = std::env::var_os("CARTOGRAPH_WEB_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("web")
                .join("dist")
        });
    let mut files = Vec::new();
    if dist.join("index.html").is_file() {
        println!("cargo::rerun-if-changed={}", dist.display());
        collect(&dist, &dist, &mut files);
        files.sort();
    }
    let mut out = String::new();
    out.push_str("/// Files of the embedded frontend: (path relative to `web/dist`, bytes).\n");
    out.push_str("pub static ASSETS: &[(&str, &[u8])] = &[\n");
    for (rel, abs) in &files {
        let _ = writeln!(out, "    ({rel:?}, include_bytes!({abs:?})),");
    }
    out.push_str("];\n");
    let out_dir = std::env::var_os("OUT_DIR").map(PathBuf::from);
    let Some(out_dir) = out_dir else {
        println!("cargo::error=OUT_DIR is not set");
        return;
    };
    if let Err(e) = std::fs::write(out_dir.join("web_assets.rs"), out) {
        println!("cargo::error=cannot write web_assets.rs: {e}");
    }
    if files.is_empty() {
        println!(
            "cargo::warning=web/dist not found: building without the web frontend (the server will serve a page explaining how to build it)"
        );
    }
}

/// Every file under `dir`, as (forward-slash path relative to `base`, absolute path).
fn collect(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            collect(base, &path, out);
        } else if let (Ok(rel), Some(abs)) = (path.strip_prefix(base), path.to_str()) {
            let rel: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            out.push((rel.join("/"), abs.to_string()));
        }
    }
}
