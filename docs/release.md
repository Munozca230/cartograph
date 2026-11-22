# Releasing cartograph

Publishing is a manual decision. Nothing in this repository pushes a tag, uploads an
archive or publishes a release on its own: the release workflow stops at a **draft**.
Publishing that draft by hand also publishes to npm, and an npm version cannot be
replaced (see [npm](#npm)).

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
5. Review the draft on GitHub and publish it by hand. That starts
   `.github/workflows/npm.yml`, which publishes the same archives to npm (see [npm](#npm)).

Actions are pinned by commit SHA, with the version in a comment (checked on GitHub on
2026-11-21; `npm.yml` uses the same pins): `actions/checkout` v7.0.1, `actions/setup-node` v7.1.0,
`actions/upload-artifact` v7.0.2, `actions/download-artifact` v8.0.2,
`Swatinem/rust-cache` v2.9.3, `softprops/action-gh-release` v3.0.4. Rust comes from the
runner's `rustup` (stable).

## npm

The npm distribution follows the esbuild pattern: one package per platform with the
binary, and a main package with a launcher that depends on all of them as optional
dependencies. npm installs only the platform package whose `os`, `cpu` (and `libc`)
match.

| Package | Content |
|---|---|
| `@munozca230/cartograph` | `bin/cartograph.js` (the `cartograph` command), `README.md`, license files |
| `@munozca230/cartograph-win32-x64` | `bin/cartograph.exe` from the `x86_64-pc-windows-msvc` archive |
| `@munozca230/cartograph-linux-x64` | `bin/cartograph` from the `x86_64-unknown-linux-gnu` archive (`libc: glibc`) |
| `@munozca230/cartograph-darwin-arm64` | `bin/cartograph` from the `aarch64-apple-darwin` archive |
| `@munozca230/cartograph-darwin-x64` | `bin/cartograph` from the `x86_64-apple-darwin` archive |

Each platform package also carries `LICENSE-MIT`, `LICENSE-APACHE` and
`THIRD_PARTY_NOTICES.md` from its archive.

- `npm/cartograph/` holds the main package as committed: `package.json`, `README.md` (the
  page on npmjs.com) and the launcher `bin/cartograph.js`. The launcher is plain CommonJS
  with no dependencies. It resolves `@munozca230/cartograph-<platform>-<arch>/bin/cartograph[.exe]`
  with `require.resolve` and runs it with `spawnSync` and `stdio: "inherit"`, so stdin and
  stdout (the MCP protocol) pass through untouched. It passes on the exit code and a
  terminating signal. On another platform, or when the platform package is missing, it
  prints the supported platforms and the other ways to install, and exits 1.
- The platform packages are not in the repository: `scripts/npm-packages.mjs` generates
  them from the release archives.

### Versions

**The tag is the only version to set.** `scripts/npm-packages.mjs --version` writes it into
all five `package.json` files and into the main package's `optionalDependencies` (exact
versions). `npm/cartograph/package.json` keeps the placeholder `0.0.0` and is never bumped.
The tag already has to match `Cargo.toml` (`release.yml`), and the script fails unless the
archives are named after the version and the binary of the machine running it prints
`cartograph <version>`, so the npm version, the archives and the binary agree.

A prerelease tag (`v1.2.0-rc.1`) is published under the npm dist-tag `next`, so
`npx @munozca230/cartograph` keeps resolving to the last stable version.

### The workflow

`.github/workflows/npm.yml` runs when a release is published (`release: published`), or by
hand from the Actions tab (`workflow_dispatch` with the `tag` input: to publish a release
that was published before the workflow existed, or to finish a failed run). On
`ubuntu-24.04` it:

1. fails at once if the secret `NPM_TOKEN` is not set;
2. checks that the tag reads `v<major>.<minor>.<patch>[-<pre>]` and that the release is
   not a draft;
3. downloads the archives and `.sha256` files of the release (`gh release download`);
4. runs `node scripts/npm-packages.mjs --version <tag version> --assets assets --out npm-out`,
   which checks every archive against its `.sha256`, extracts it, writes the five packages
   (the unix binaries keep mode 755) and runs the Linux binary's `--version`;
5. publishes with `npm publish --access public --provenance`, the four platform packages
   first and the main package last. A `package@version` that `npm view` already finds is
   skipped, so running the workflow again only publishes what is missing.

`--provenance` links each version on npmjs.com to the workflow run that built it (it needs
the `id-token: write` permission and a public repository).

### One-time setup

npm only issues granular access tokens (classic and automation tokens were removed in
late 2025).

1. On npmjs.com, signed in as `munozca230`, open the account's access tokens page and
   generate a new granular access token.
2. Fill in the form:
   - name: `cartograph GitHub Actions`;
   - **Bypass two-factor authentication**: checked. Without it a publish from CI asks for
     a one-time password and fails. It is off by default, and it covers package publishing
     only.
   - **Packages and scopes**: permission **Read and write (publish and stage)**, not "stage
     only" (that one stages versions for review and never publishes them). Select **All
     Packages** for the first publish: the five packages do not exist yet, so they cannot
     be picked one by one.
   - **Expiration**: npm caps tokens that can write at 90 days. Note the date: once it
     passes, the workflow fails at the publish step until the secret is replaced.
3. On GitHub, in the repository: **Settings**, **Secrets and variables**, **Actions**, **New
   repository secret**. Name `NPM_TOKEN`, and the token as the value.

npm announces that granular tokens will stop publishing directly in January 2027. The way
forward is trusted publishing (OIDC). Once the five packages exist, add a trusted publisher
in each package's settings on npmjs.com:

- provider GitHub Actions, user `Munozca230`, repository `cartograph`, workflow filename
  `npm.yml`, no environment;
- under the allowed actions, enable direct `npm publish`, since the workflow does not stage;
- a new configuration expires if no publish uses it within 2 days, so set it up shortly
  before a release.

The npm CLI (11.5.1 or newer; Node.js 24 ships it) tries OIDC before the token, and
provenance is then automatic. Once every package has its trusted publisher, remove the
`NPM_TOKEN` step at the start of `npm.yml` (it would fail without the secret), then the
secret and the token.

### Publishing a version

1. Release as above (tag, workflow, review the draft).
2. Publish the draft on GitHub. `npm.yml` starts; follow it in the Actions tab.
3. Check: `npx -y @munozca230/cartograph@<version> --version` prints `cartograph <version>`.

If the run fails halfway (a network error, an expired token), fix the cause and run the
workflow again from the Actions tab with the same tag: what was published is skipped.

For 0.1.0, which was tagged before the workflow existed, publishing the release starts
nothing (a release runs the workflows of its tagged commit). Run it by hand from `main` with
`tag` = `v0.1.0` once the release is published. The script and the workflow then come from
`main`, and the packages from the 0.1.0 archives.

### Testing the packages locally

`--local` builds this platform's package from a local binary (license files and notices
from the repository root), plus the main package:

```
node scripts/npm-packages.mjs --local target/release/cartograph.exe --out <dir>
```

The version defaults to `Cargo.toml`'s; `--version` overrides it (with a warning when the
binary reports another version). Then, in `<dir>`:
`npm pack` in `cartograph-win32-x64` and in `cartograph`, `npm install` the two tarballs into
an empty project, and run `npx cartograph --version`. `npm install` reports nothing for the
three platform packages that are not on the registry: optional dependencies that cannot be
fetched are skipped. `npm publish --dry-run` in each package directory lists what would be
published.

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
