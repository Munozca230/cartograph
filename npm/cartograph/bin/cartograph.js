#!/usr/bin/env node
// Launcher of the @munozca230/cartograph npm package.
//
// npm installs exactly one platform package as an optional dependency (its `os`, `cpu` and
// `libc` fields select it); this script finds the native binary in it and runs it with the
// same arguments. stdin, stdout and stderr are inherited untouched: `cartograph mcp` speaks
// MCP over stdio, so this script never reads stdin or writes to stdout. The exit code and a
// terminating signal of the binary are passed on.
//
// Plain CommonJS with no dependencies, so it runs on every supported Node.js as is.

"use strict";

const { spawnSync } = require("child_process");
const fs = require("fs");

const SCOPE = "@munozca230";
const REPO = "https://github.com/Munozca230/cartograph";

// `${process.platform}-${process.arch}` -> what the platform package covers.
const PLATFORMS = {
  "win32-x64": "Windows x86_64",
  "linux-x64": "Linux x86_64 (glibc)",
  "darwin-arm64": "macOS arm64 (Apple silicon)",
  "darwin-x64": "macOS x86_64 (Intel)",
};

function isMusl() {
  if (process.platform !== "linux") return false;
  try {
    const report = process.report && process.report.getReport();
    return !(report && report.header && report.header.glibcVersionRuntime);
  } catch (_) {
    return false;
  }
}

function fail(reason) {
  const lines = [`cartograph: ${reason}`, ""];
  if (isMusl()) {
    lines.push(
      "This system uses musl libc (Alpine or similar); the prebuilt Linux binary needs glibc.",
      "",
    );
  }
  lines.push("The npm package ships prebuilt binaries for:");
  for (const [key, label] of Object.entries(PLATFORMS)) {
    lines.push(`  - ${label}: ${SCOPE}/cartograph-${key}`);
  }
  lines.push(
    "",
    "Other ways to install cartograph:",
    `  - a release archive: ${REPO}/releases (put the cartograph binary on your PATH)`,
    "  - from source (Rust 1.90+ and a C compiler; without the web UI unless web/dist is built,",
    "    see the README):",
    `      cargo install --locked --git ${REPO} cartograph-cli`,
    "",
  );
  process.stderr.write(lines.join("\n"));
  process.exit(1);
}

function binaryPath() {
  const key = `${process.platform}-${process.arch}`;
  if (!Object.prototype.hasOwnProperty.call(PLATFORMS, key)) {
    fail(`there is no prebuilt binary for ${process.platform} ${process.arch}.`);
  }
  const pkg = `${SCOPE}/cartograph-${key}`;
  const exe = process.platform === "win32" ? "cartograph.exe" : "cartograph";
  try {
    return require.resolve(`${pkg}/bin/${exe}`);
  } catch (_) {
    fail(
      `the package ${pkg}, which holds the binary for this platform, is not installed.\n` +
        "npm installs it as an optional dependency of @munozca230/cartograph. It is missing when\n" +
        "optional dependencies were omitted (--omit=optional, --no-optional), when the platform\n" +
        "package was skipped (unsupported libc), or when node_modules came from another platform.\n" +
        "Reinstall without omitting optional dependencies.",
    );
  }
}

function run(bin) {
  return spawnSync(bin, process.argv.slice(2), { stdio: "inherit", windowsHide: true });
}

const bin = binaryPath();
let result = run(bin);

// A package unpacked without its mode bits: make the binary executable once and retry.
if (result.error && result.error.code === "EACCES" && process.platform !== "win32") {
  try {
    fs.chmodSync(bin, 0o755);
    result = run(bin);
  } catch (_) {
    // Reported below with the original error.
  }
}

if (result.error) {
  fail(`could not run ${bin}: ${result.error.message}`);
}

if (result.signal) {
  // Die of the same signal, so the parent sees what the binary saw.
  process.kill(process.pid, result.signal);
}
process.exit(typeof result.status === "number" ? result.status : 1);
