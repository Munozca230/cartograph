#!/usr/bin/env node
// Builds the npm packages of cartograph from the release archives (the esbuild pattern):
//
// - `@munozca230/cartograph`: the launcher `bin/cartograph.js`, from npm/cartograph/, with
//   one optional dependency per platform package;
// - `@munozca230/cartograph-<platform>-<arch>`: one per release target, with the binary in
//   bin/, the license files and THIRD_PARTY_NOTICES.md, restricted by `os` and `cpu`.
//
// Every version (the five packages and the optional dependencies) is set from --version, so
// nothing is bumped by hand: npm/cartograph/package.json keeps the placeholder 0.0.0.
//
// Usage:
//   node scripts/npm-packages.mjs --version <v> --assets <dir> --out <dir>
//       <dir> holds the four release archives cartograph-<v>-<target>.{zip,tar.gz} and their
//       .sha256 files. Each archive is checked against its checksum, then extracted.
//   node scripts/npm-packages.mjs --local <path to cartograph[.exe]> --out <dir> [--version <v>]
//       Builds only this platform's package from a local binary (licenses and notices from
//       the repository root), plus the main package. For testing. The version defaults to
//       the workspace version in Cargo.toml.
//
// Writes <out>/cartograph and <out>/cartograph-<platform>-<arch>, replacing those
// directories if they exist; nothing else in <out> is touched. When the binary of the
// machine running the script is among them, its `--version` must print the version.
//
// No dependencies. Extraction uses `tar` for .tar.gz, and for .zip PowerShell
// `Expand-Archive` on Windows, `unzip` (or `tar`) elsewhere.

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const SCOPE = "@munozca230";
const MAIN = `${SCOPE}/cartograph`;
const LICENSES = ["LICENSE-MIT", "LICENSE-APACHE"];
const NOTICES = "THIRD_PARTY_NOTICES.md";

/** Release targets, as release.yml builds them. `key` is `${process.platform}-${process.arch}`. */
const PLATFORMS = [
  { key: "win32-x64", os: "win32", cpu: "x64", target: "x86_64-pc-windows-msvc", archive: "zip", label: "Windows x86_64" },
  { key: "linux-x64", os: "linux", cpu: "x64", target: "x86_64-unknown-linux-gnu", archive: "tar.gz", libc: "glibc", label: "Linux x86_64 (glibc)" },
  { key: "darwin-arm64", os: "darwin", cpu: "arm64", target: "aarch64-apple-darwin", archive: "tar.gz", label: "macOS arm64" },
  { key: "darwin-x64", os: "darwin", cpu: "x64", target: "x86_64-apple-darwin", archive: "tar.gz", label: "macOS x86_64" },
];

const exeName = (p) => (p.os === "win32" ? "cartograph.exe" : "cartograph");

function die(msg) {
  console.error(`npm-packages: ${msg}`);
  process.exit(1);
}

function parseCli() {
  let values;
  try {
    ({ values } = parseArgs({
      options: {
        version: { type: "string" },
        assets: { type: "string" },
        out: { type: "string" },
        local: { type: "string" },
        help: { type: "boolean", short: "h" },
      },
      strict: true,
    }));
  } catch (e) {
    die(e.message);
  }
  if (values.help) {
    console.log(
      "usage: node scripts/npm-packages.mjs --version <v> --assets <dir> --out <dir>\n" +
        "       node scripts/npm-packages.mjs --local <cartograph binary> --out <dir> [--version <v>]",
    );
    process.exit(0);
  }
  if (!values.out) die("--out <dir> is required");
  if (values.local && values.assets) die("--local and --assets exclude each other");
  if (!values.local && !values.assets) die("pass --assets <dir> (release archives) or --local <binary>");
  if (values.assets && !values.version) die("--version <v> is required with --assets");
  return values;
}

function normalizeVersion(v) {
  const version = v.replace(/^v/, "");
  if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$/.test(version)) {
    die(`not a semver version: ${v}`);
  }
  return version;
}

function cargoVersion() {
  const toml = readFileSync(join(root, "Cargo.toml"), "utf8");
  const m = toml.match(/^\[workspace\.package\][^[]*?^version\s*=\s*"([^"]+)"/m);
  if (!m) die("no [workspace.package] version in Cargo.toml");
  return m[1];
}

const mainTemplate = () => JSON.parse(readFileSync(join(root, "npm", "cartograph", "package.json"), "utf8"));

function writeJson(file, value) {
  writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function freshDir(dir) {
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });
}

// --- release archives -------------------------------------------------------------------

function sha256(file) {
  return createHash("sha256").update(readFileSync(file)).digest("hex");
}

function verifyChecksum(archive) {
  const sumFile = `${archive}.sha256`;
  if (!existsSync(sumFile)) die(`missing checksum file ${basename(sumFile)}`);
  // `<hash>  <file>` (sha256sum, shasum) or `<hash> *<file>` (binary mode).
  const m = readFileSync(sumFile, "utf8").match(/^([0-9a-fA-F]{64})\s+\*?(.+?)\s*$/m);
  if (!m) die(`${basename(sumFile)} has no SHA-256 line`);
  if (m[2] !== basename(archive)) die(`${basename(sumFile)} names ${m[2]}, not ${basename(archive)}`);
  const actual = sha256(archive);
  if (actual !== m[1].toLowerCase()) {
    die(`checksum mismatch for ${basename(archive)}: expected ${m[1].toLowerCase()}, got ${actual}`);
  }
}

function tarCommand() {
  // On Windows use the system bsdtar: a GNU tar earlier on PATH (Git for Windows) reads
  // `C:\...` as a remote host.
  if (process.platform === "win32") {
    const sys = join(process.env.SystemRoot || "C:\\Windows", "System32", "tar.exe");
    if (existsSync(sys)) return sys;
  }
  return "tar";
}

function run(cmd, args, opts = {}) {
  execFileSync(cmd, args, { stdio: ["ignore", "ignore", "inherit"], ...opts });
}

function extract(archive, dest) {
  freshDir(dest);
  // tar runs inside dest with a relative archive path (see tarCommand).
  const rel = relative(dest, archive);
  if (archive.endsWith(".tar.gz")) {
    run(tarCommand(), ["-xzf", rel, "-C", "."], { cwd: dest });
  } else if (process.platform === "win32") {
    run(
      "powershell.exe",
      [
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "$ErrorActionPreference = 'Stop'; Expand-Archive -LiteralPath $env:NPM_PKG_ARCHIVE -DestinationPath $env:NPM_PKG_DEST -Force",
      ],
      { env: { ...process.env, NPM_PKG_ARCHIVE: archive, NPM_PKG_DEST: dest } },
    );
  } else {
    try {
      run("unzip", ["-q", "-o", archive, "-d", dest]);
    } catch (e) {
      // unzip exits 1 on warnings only (such as backslash separators), having extracted
      // everything; the files are checked afterwards.
      if (e.status === 1) return;
      if (e.code !== "ENOENT") throw e;
      run(tarCommand(), ["-xf", rel, "-C", "."], { cwd: dest });
    }
  }
}

// --- packages ---------------------------------------------------------------------------

function platformPackage(p, version, files, out) {
  const name = `${SCOPE}/cartograph-${p.key}`;
  const dir = join(out, `cartograph-${p.key}`);
  freshDir(join(dir, "bin"));

  const bin = join(dir, "bin", exeName(p));
  copyFileSync(files.binary, bin);
  if (p.os !== "win32") chmodSync(bin, 0o755);
  for (const f of [...LICENSES, NOTICES]) copyFileSync(files[f], join(dir, f));

  const main = mainTemplate();
  const pkg = {
    name,
    version,
    description: `The cartograph binary for ${p.label}. Install ${MAIN}, which picks the right one.`,
    license: main.license,
    repository: main.repository,
    homepage: main.homepage,
    os: [p.os],
    cpu: [p.cpu],
    ...(p.libc ? { libc: [p.libc] } : {}),
    files: ["bin", ...LICENSES, NOTICES],
    preferUnplugged: true,
  };
  writeJson(join(dir, "package.json"), pkg);
  writeFileSync(
    join(dir, "README.md"),
    `# ${name}\n\nThe \`cartograph\` binary for ${p.label}. Do not install this package directly: ` +
      `install [\`${MAIN}\`](https://www.npmjs.com/package/${MAIN}), which depends on it ` +
      `as an optional dependency and runs it.\n\n` +
      `Source and documentation: ${main.homepage.replace(/#.*$/, "")}\n`,
  );
  return { name, dir, bin, platform: p };
}

function mainPackage(version, out) {
  const src = join(root, "npm", "cartograph");
  const dir = join(out, "cartograph");
  freshDir(join(dir, "bin"));

  const pkg = mainTemplate();
  const expected = PLATFORMS.map((p) => `${SCOPE}/cartograph-${p.key}`).sort();
  const listed = Object.keys(pkg.optionalDependencies || {}).sort();
  if (JSON.stringify(expected) !== JSON.stringify(listed)) {
    die(`npm/cartograph/package.json optionalDependencies must be exactly: ${expected.join(", ")}`);
  }
  pkg.version = version;
  pkg.optionalDependencies = Object.fromEntries(expected.map((name) => [name, version]));
  writeJson(join(dir, "package.json"), pkg);

  copyFileSync(join(src, "bin", "cartograph.js"), join(dir, "bin", "cartograph.js"));
  chmodSync(join(dir, "bin", "cartograph.js"), 0o755);
  copyFileSync(join(src, "README.md"), join(dir, "README.md"));
  for (const f of LICENSES) copyFileSync(join(root, f), join(dir, f));
  return { name: MAIN, dir };
}

/** The binary built for this machine must report the version it is published as. */
function checkHostBinary(built, version) {
  const host = built.find((b) => b.platform.key === `${process.platform}-${process.arch}`);
  if (!host) return;
  if (process.platform !== "win32" && (statSync(host.bin).mode & 0o111) === 0) {
    die(`${host.bin} is not executable`);
  }
  const out = execFileSync(host.bin, ["--version"], { encoding: "utf8" }).trim();
  if (out !== `cartograph ${version}`) {
    die(`${host.name}: the binary reports "${out}", expected "cartograph ${version}"`);
  }
  console.log(`checked ${host.name}: ${out}`);
}

function sizeOf(dir) {
  let total = 0;
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    total += e.isDirectory() ? sizeOf(p) : statSync(p).size;
  }
  return total;
}

function main() {
  const args = parseCli();
  const out = resolve(args.out);
  mkdirSync(out, { recursive: true });
  const built = [];
  let version;

  if (args.local) {
    version = normalizeVersion(args.version || cargoVersion());
    const key = `${process.platform}-${process.arch}`;
    const p = PLATFORMS.find((x) => x.key === key);
    if (!p) die(`no npm platform package for ${key}`);
    const binary = resolve(args.local);
    if (!existsSync(binary)) die(`no such file: ${binary}`);
    const files = { binary };
    for (const f of [...LICENSES, NOTICES]) files[f] = join(root, f);
    built.push(platformPackage(p, version, files, out));
  } else {
    version = normalizeVersion(args.version);
    const assets = resolve(args.assets);
    const staging = join(out, ".staging");
    freshDir(staging);
    for (const p of PLATFORMS) {
      const stem = `cartograph-${version}-${p.target}`;
      const archive = join(assets, `${stem}.${p.archive}`);
      if (!existsSync(archive)) die(`missing release archive ${basename(archive)} in ${assets}`);
      verifyChecksum(archive);
      const dest = join(staging, p.key);
      try {
        extract(archive, dest);
      } catch (e) {
        die(`could not extract ${basename(archive)}: ${e.message.split("\n")[0]}`);
      }
      const inner = join(dest, stem);
      const files = { binary: join(inner, exeName(p)) };
      for (const f of [...LICENSES, NOTICES]) files[f] = join(inner, f);
      for (const f of Object.values(files)) {
        if (!existsSync(f)) die(`${basename(archive)} has no ${relative(dest, f).replaceAll("\\", "/")}`);
      }
      built.push(platformPackage(p, version, files, out));
      console.log(`verified and extracted ${basename(archive)}`);
    }
    rmSync(staging, { recursive: true, force: true });
  }

  checkHostBinary(built, version);
  built.push(mainPackage(version, out));
  for (const b of built) {
    console.log(`${b.name}@${version}  ${b.dir}  ${(sizeOf(b.dir) / 1e6).toFixed(1)} MB`);
  }
}

main();
