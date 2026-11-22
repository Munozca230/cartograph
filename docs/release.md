# Releasing cartograph

Publishing is a manual decision. Nothing in this repository pushes a tag, uploads an
archive or publishes a release on its own: the release workflow stops at a **draft**.

## What a release contains

One archive per platform with the single `cartograph` binary, which carries the web map
(`web/dist`, embedded at compile time by `crates/cartograph-server/build.rs`), next to
`README.md`, `LICENSE-MIT`, `LICENSE-APACHE` and `THIRD_PARTY_NOTICES.md`; plus a SHA-256
checksum per archive and a `SHA256SUMS` file:

| Target | Runner | Archive |
|---|---|---|
| `x86_64-pc-windows-msvc` | `windows-2025` | `cartograph-<version>-x86_64-pc-windows-msvc.zip` |
| `x86_64-unknown-linux-gnu` | `ubuntu-24.04` | `cartograph-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| `aarch64-apple-darwin` | `macos-15` | `cartograph-<version>-aarch64-apple-darwin.tar.gz` |
| `x86_64-apple-darwin` | `macos-15-intel` | `cartograph-<version>-x86_64-apple-darwin.tar.gz` |

The Windows binary links the C runtime statically (`.cargo/config.toml`), so it runs
without the VC++ Redistributable. Linux is glibc (`gnu`): musl needs a musl C toolchain
for the bundled SQLite and the tree-sitter grammars, which was not set up.

## Steps

1. Set the version in the root `Cargo.toml` (`[workspace.package] version`), regenerate
   the third-party notices if a dependency changed (see below), and commit.
2. Optional, locally: `powershell -ExecutionPolicy Bypass -File scripts/package.ps1`
   builds the Windows archive into `dist/` (see below).
3. Tag and push: `git tag v<version>` then `git push origin v<version>`.
4. `.github/workflows/release.yml` runs on the tag. Per platform it:
   - checks that the tag matches the Cargo version (it fails otherwise);
   - builds `web/dist` (`npm ci && npm run build`), because the binary embeds it;
   - checks that `THIRD_PARTY_NOTICES.md` is up to date (`node scripts/third-party.mjs --check`);
   - runs `cargo test --workspace --release`;
   - builds `cartograph` in release mode and checks that `cartograph doctor` reports
     `web ui  embedded in this binary`;
   - packages the archive (binary, README, license files, notices) and its `.sha256`.

   Then one job collects the archives, writes `SHA256SUMS` and creates a draft release
   with generated notes.
5. Review the draft on GitHub and publish it by hand.

Actions are pinned by commit SHA, with the version in a comment (checked on GitHub on
2026-11-21): `actions/checkout` v7.0.1, `actions/setup-node` v7.1.0,
`actions/upload-artifact` v7.0.2, `actions/download-artifact` v8.0.2,
`Swatinem/rust-cache` v2.9.3, `softprops/action-gh-release` v3.0.4. Rust comes from the
runner's `rustup` (stable).

## Local package (Windows)

```
powershell -ExecutionPolicy Bypass -File scripts/package.ps1 [-SkipWeb] [-Test]
```

It builds the web frontend, sets `CARTOGRAPH_WEB_DIST` (so `build.rs` picks up a
`web/dist` that did not exist at the previous build), builds
`target/x86_64-pc-windows-msvc/release/cartograph.exe`, fails unless `doctor` reports the
web UI as embedded, and writes `dist/cartograph-<version>-x86_64-pc-windows-msvc.zip` and
`dist/…zip.sha256` (`sha256sum` format). The zip holds the binary, `README.md`, the two
license files and `THIRD_PARTY_NOTICES.md`. `-Test` runs the tests first; `-SkipWeb` reuses
an existing `web/dist`. `dist/` is ignored by git.

Measured for 0.1.0 (2026-11-21): the zip is 10.3 MB (10,831,034 bytes), the binary inside it 39.9 MB (41,805,312 bytes).

## CI

`.github/workflows/ci.yml` runs on pull requests and pushes to `main`:

- `rust` on Ubuntu and Windows: `cargo fmt --check`, `cargo clippy -D warnings`,
  `cargo test --workspace`;
- `web`: `npm test`, `tsc --noEmit`, `npm run lint`, `npm run build`;
- `evals`: `cartograph eval` (see [mcp.md](mcp.md#evals)). It may fail without failing the
  workflow (`continue-on-error`): a failing case is a defect of a tool to fix, not a
  broken build.

## License and third-party notices

cartograph is licensed under `MIT OR Apache-2.0` (`LICENSE-MIT`, `LICENSE-APACHE`; the
workspace and `web/package.json` say the same). `THIRD_PARTY_NOTICES.md` lists every Rust
crate the binary links and every npm package bundled into `web/dist`, with their licenses,
and reproduces the texts that must travel with the binary: elkjs (EPL-2.0; its
package.json says `EPL-2.0 OR GPL-3.0-or-later`) and the two fonts (OFL-1.1). Regenerate it
after any dependency change:

```
npm --prefix web ci
node scripts/third-party.mjs          # writes THIRD_PARTY_NOTICES.md
node scripts/third-party.mjs --check  # exit 1 if it is out of date
```

## Before the first public release

- **Platforms not yet run.** The test suite has only run on Windows. The first tag (or a
  manual run of CI) is the first run on Linux and macOS.
- **Signing.** Binaries are not code-signed or notarized: macOS Gatekeeper and Windows
  SmartScreen will warn on first run.
