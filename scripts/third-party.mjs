#!/usr/bin/env node
// Regenerates THIRD_PARTY_NOTICES.md at the repository root.
//
// - Rust: `cargo metadata` (all platforms), the crates the workspace members reach through
//   normal dependencies (build and dev dependencies are not shipped).
// - npm: the packages of web/package-lock.json that are not dev dependencies, minus
//   type-only packages, with the license of their own package.json in web/node_modules
//   (run `npm ci` in web/ first).
//
// Usage: node scripts/third-party.mjs [--check]
//   --check  exit 1 if THIRD_PARTY_NOTICES.md is not up to date (writes nothing).
//
// The output holds names, versions, SPDX expressions and public repository URLs only:
// no local paths.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const web = join(root, "web");
const out = join(root, "THIRD_PARTY_NOTICES.md");

/** Plain code-unit order: the same on every machine and locale. */
const cmp = (a, b) => (a < b ? -1 : a > b ? 1 : 0);

function rustCrates() {
  const json = execFileSync("cargo", ["metadata", "--format-version", "1", "--locked"], {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 256 * 1024 * 1024,
  });
  const meta = JSON.parse(json);
  const byId = new Map(meta.packages.map((p) => [p.id, p]));
  const nodes = new Map(meta.resolve.nodes.map((n) => [n.id, n]));
  const members = new Set(meta.workspace_members);
  const seen = new Set();
  const stack = [...members];
  while (stack.length) {
    const id = stack.pop();
    if (seen.has(id)) continue;
    seen.add(id);
    for (const d of nodes.get(id)?.deps ?? []) {
      // Normal dependencies only: `kind` is null for them.
      if (d.dep_kinds.some((k) => k.kind === null)) stack.push(d.pkg);
    }
  }
  return [...seen]
    .filter((id) => !members.has(id))
    .map((id) => byId.get(id))
    .map((p) => ({
      name: p.name,
      version: p.version,
      license: p.license ?? (p.license_file ? `see its ${p.license_file.split(/[\\/]/).pop()}` : "unknown"),
      repo: p.repository ?? "",
      dir: dirname(p.manifest_path),
    }))
    .sort((a, b) => cmp(a.name, b.name) || cmp(a.version, b.version));
}

function npmPackages() {
  const lock = JSON.parse(readFileSync(join(web, "package-lock.json"), "utf8"));
  const pkgs = [];
  for (const [path, entry] of Object.entries(lock.packages)) {
    if (!path || entry.dev) continue;
    const dir = join(web, path);
    const manifest = join(dir, "package.json");
    if (!existsSync(manifest)) {
      throw new Error(`${path} is not installed: run \`npm ci\` in web/ first`);
    }
    const pj = JSON.parse(readFileSync(manifest, "utf8"));
    // Type-only packages (no runtime code) never reach web/dist.
    const typeOnly =
      pj.name.startsWith("@types/") || (!pj.main && !pj.module && !pj.exports && !pj.browser && (pj.types || pj.typings));
    if (typeOnly) continue;
    const repo = typeof pj.repository === "string" ? pj.repository : pj.repository?.url ?? "";
    pkgs.push({
      name: pj.name,
      version: pj.version,
      license: pj.license ?? entry.license ?? "unknown",
      repo: normalizeRepo(repo),
      dir,
    });
  }
  return pkgs.sort((a, b) => cmp(a.name, b.name));
}

function normalizeRepo(url) {
  return url
    .replace(/^git\+/, "")
    .replace(/^git:\/\//, "https://")
    .replace(/\.git$/, "")
    .replace(/^github:/, "https://github.com/");
}

function table(rows, head) {
  const esc = (s) => String(s).replace(/\|/g, "\\|");
  const lines = [`| ${head.join(" | ")} |`, `|${head.map(() => "---").join("|")}|`];
  for (const r of rows) lines.push(`| ${r.map(esc).join(" | ")} |`);
  return lines.join("\n");
}

function counts(items) {
  const m = new Map();
  for (const i of items) m.set(i.license, (m.get(i.license) ?? 0) + 1);
  return [...m.entries()].sort((a, b) => b[1] - a[1] || cmp(a[0], b[0]));
}

/** NOTICE files (Apache-2.0 asks redistributors to keep them), by package. */
function notices(items) {
  const found = [];
  for (const i of items) {
    for (const name of ["NOTICE", "NOTICE.txt", "NOTICE.md"]) {
      const p = join(i.dir, name);
      if (existsSync(p)) found.push({ ...i, notice: text(p) });
    }
  }
  return found;
}

function text(path) {
  return readFileSync(path, "utf8").replace(/\r\n/g, "\n").trimEnd();
}

function render() {
  const crates = rustCrates();
  const npm = npmPackages();
  const elk = npm.find((p) => p.name === "elkjs");
  const fonts = npm.filter((p) => p.name.startsWith("@fontsource"));
  const s = [];
  s.push("# Third-party notices", "");
  s.push(
    "The `cartograph` binary links the Rust crates listed below and embeds the web UI",
    "(`web/dist`), which is built from the npm packages listed below. Each one is distributed",
    "under its own license, named here by its SPDX expression. cartograph itself is licensed",
    "under `MIT OR Apache-2.0` (see `LICENSE-MIT` and `LICENSE-APACHE`).",
    "",
    "This file is generated by `node scripts/third-party.mjs` from `cargo metadata` (normal",
    "dependencies of the workspace, every platform) and `web/package-lock.json` (packages",
    "that are not dev dependencies, without type-only ones). Do not edit it by hand.",
    "",
  );
  s.push("## Licenses that need attention", "");
  if (elk) {
    s.push(
      `- **elkjs ${elk.version}** (graph layout, bundled unmodified as a web worker):`,
      `  \`${elk.license}\` in its package.json. Its \`LICENSE.md\` is the Eclipse Public`,
      "  License 2.0, whose Exhibit A names the GNU General Public License v3.0 or later as",
      "  Secondary License. cartograph uses and redistributes it under EPL-2.0. Its source code",
      `  is available at ${elk.repo} (tag \`${elk.version}\`). The EPL-2.0 text is reproduced`,
      "  at the end of this file.",
    );
  }
  // Copyleft licenses without a permissive alternative (file-level ones such as MPL-2.0).
  for (const c of crates.filter((c) => /MPL|EPL|GPL/.test(c.license) && !/OR/.test(c.license))) {
    s.push(
      `- **${c.name} ${c.version}** (Rust crate): \`${c.license}\`. Linked unmodified; its source`,
      `  code is available at ${c.repo || "crates.io"} and on crates.io (version ${c.version}).`,
    );
  }
  for (const f of fonts) {
    const family = f.name.split("/").pop();
    s.push(
      `- **${family}** (${f.name} ${f.version}, a font embedded in the web UI): \`${f.license}\`,`,
      "  SIL Open Font License 1.1. Its copyright notice and license text are reproduced at the",
      "  end of this file.",
    );
  }
  s.push("");
  s.push(`## Rust crates (${crates.length})`, "");
  s.push(table(counts(crates), ["License", "Crates"]), "");
  s.push(table(crates.map((c) => [c.name, c.version, c.license, c.repo]), ["Crate", "Version", "License", "Repository"]), "");
  s.push(`## npm packages bundled into web/dist (${npm.length})`, "");
  s.push(table(npm.map((p) => [p.name, p.version, p.license, p.repo]), ["Package", "Version", "License", "Repository"]), "");
  const ns = notices([...crates, ...npm]);
  if (ns.length) {
    s.push("## NOTICE files", "");
    for (const n of ns) s.push(`### ${n.name} ${n.version}`, "", "```text", n.notice, "```", "");
  }
  s.push("## License texts", "");
  if (elk) {
    s.push(`### elkjs ${elk.version}: Eclipse Public License 2.0`, "", "```text", text(join(elk.dir, "LICENSE.md")), "```", "");
  }
  for (const f of fonts) {
    s.push(`### ${f.name} ${f.version}: SIL Open Font License 1.1`, "", "```text", text(join(f.dir, "LICENSE")), "```", "");
  }
  return s.join("\n").replace(/\n+$/, "\n");
}

const next = render();
if (process.argv.includes("--check")) {
  const now = existsSync(out) ? readFileSync(out, "utf8").replace(/\r\n/g, "\n") : "";
  if (now !== next) {
    console.error("THIRD_PARTY_NOTICES.md is out of date: run `node scripts/third-party.mjs`");
    process.exit(1);
  }
  console.log("THIRD_PARTY_NOTICES.md is up to date");
} else {
  writeFileSync(out, next);
  console.log("wrote THIRD_PARTY_NOTICES.md");
}
