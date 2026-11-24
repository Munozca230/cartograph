#!/usr/bin/env node
// Performance gate: a change against its base, measured on the same machine.
//
// Both binaries index the same corpora into fresh data dirs (outside the corpora), after
// one untimed run that warms the disk cache, runs interleaved so a drift of the machine
// hits both alike. Per corpus and binary:
// - cold index: wall time, the median of --runs runs, each into an empty data dir;
// - peak memory: the median of the peak RSS of those runs (`/usr/bin/time -v` on Linux,
//   `/usr/bin/time -l` on macOS, the process's peak working set on Windows);
// - DB size: graph.db (and its -wal, if any) after a cold index;
// - warm index: a re-index of an unchanged corpus, the median of --runs runs;
// - queries: the sum of the median time of each --query.
// A metric fails when the head exceeds the base by more than its limit (a time also by
// more than --min-time-delta seconds, so a fast corpus does not fail on jitter). The
// table is printed either way, and appended to $GITHUB_STEP_SUMMARY when set.
//
// Usage:
//   node scripts/perf-gate.mjs --base <bin> --head <bin> --corpus <name>=<path> [...]
//     [--query "<name>:<query args>"]... [--runs 3] [--work <dir>]
//     [--max-time 25] [--max-size 10] [--max-rss 20] [--max-warm 0] [--max-query 0]
//     [--min-time-delta 0.25] [--json <file>]
//   A limit is a percentage; 0 reports the metric without gating it.
//   --query "self:find Store" runs `<bin> query -C <corpus> --data-dir <dir> find Store`.
// Exit: 0 within the limits, 1 past one, 2 on a usage or run error.

import { spawnSync } from "node:child_process";
import {
  appendFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

function usage(msg) {
  console.error(`perf-gate: ${msg}`);
  console.error(
    "usage: node scripts/perf-gate.mjs --base <bin> --head <bin> --corpus <name>=<path> " +
      '[--query "<name>:<args>"] [--runs N] [--work DIR] [--max-time P] [--max-size P] ' +
      "[--max-rss P] [--max-warm P] [--max-query P] [--min-time-delta S] [--json FILE]",
  );
  process.exit(2);
}

function parseArgs(argv) {
  const o = {
    corpora: [],
    queries: [],
    runs: 3,
    limits: { cold: 25, size: 10, rss: 20, warm: 0, query: 0 },
    minTimeDelta: 0.25,
  };
  const limitOf = { "--max-time": "cold", "--max-size": "size", "--max-rss": "rss" };
  Object.assign(limitOf, { "--max-warm": "warm", "--max-query": "query" });
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const v = () => {
      if (i + 1 >= argv.length) usage(`${a} needs a value`);
      return argv[++i];
    };
    if (a === "--base") o.base = resolve(v());
    else if (a === "--head") o.head = resolve(v());
    else if (a === "--corpus") {
      const s = v();
      const at = s.indexOf("=");
      if (at < 1) usage(`--corpus wants <name>=<path>, got ${s}`);
      o.corpora.push({ name: s.slice(0, at), path: resolve(s.slice(at + 1)) });
    } else if (a === "--query") {
      const s = v();
      const at = s.indexOf(":");
      if (at < 1) usage(`--query wants <name>:<args>, got ${s}`);
      const args = s.slice(at + 1).trim().split(/\s+/).filter(Boolean);
      o.queries.push({ corpus: s.slice(0, at), args });
    } else if (a === "--runs") o.runs = Number(v());
    else if (a === "--work") o.work = resolve(v());
    else if (a === "--json") o.json = resolve(v());
    else if (a === "--min-time-delta") o.minTimeDelta = Number(v());
    else if (a in limitOf) o.limits[limitOf[a]] = Number(v());
    else usage(`unknown argument ${a}`);
  }
  if (!o.base || !o.head) usage("--base and --head are required");
  if (!o.corpora.length) usage("at least one --corpus is required");
  if (!(o.runs >= 1)) usage("--runs must be at least 1");
  for (const b of [o.base, o.head]) if (!existsSync(b)) usage(`no binary at ${b}`);
  for (const c of o.corpora) if (!existsSync(c.path)) usage(`no corpus at ${c.path}`);
  for (const q of o.queries)
    if (!o.corpora.some((c) => c.name === q.corpus)) usage(`--query names no corpus: ${q.corpus}`);
  return o;
}

// One run of `bin args`: wall seconds, peak RSS in bytes (null when not measurable), stdout.
// A non-zero exit stops the gate: a binary that cannot index has no numbers to compare.
const WIN_PEAK = `
Add-Type -TypeDefinition @"
using System; using System.Runtime.InteropServices;
public static class PerfGatePeak {
  [StructLayout(LayoutKind.Sequential)] public struct C {
    public uint cb; public uint PageFaultCount; public UIntPtr PeakWorkingSetSize;
    public UIntPtr WorkingSetSize; public UIntPtr a; public UIntPtr b; public UIntPtr c;
    public UIntPtr d; public UIntPtr PagefileUsage; public UIntPtr PeakPagefileUsage; }
  [DllImport("psapi.dll")] public static extern bool GetProcessMemoryInfo(IntPtr h, out C c, uint cb);
  public static ulong Peak(IntPtr h) { C c; c.cb = (uint)Marshal.SizeOf(typeof(C));
    return GetProcessMemoryInfo(h, out c, c.cb) ? (ulong)c.PeakWorkingSetSize : 0; }
}
"@
$psi = New-Object System.Diagnostics.ProcessStartInfo $env:PERF_GATE_EXE
$psi.Arguments = $env:PERF_GATE_ARGS
$psi.UseShellExecute = $false
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$p = [System.Diagnostics.Process]::Start($psi)
$h = $p.Handle
$err = $p.StandardError.ReadToEndAsync()
$out = $p.StandardOutput.ReadToEnd()
$p.WaitForExit()
$ms = $sw.Elapsed.TotalMilliseconds
[Console]::Error.Write($err.Result)
[Console]::Out.Write($out)
"@@perf-gate ms=$ms peak=$([PerfGatePeak]::Peak($h)) exit=$($p.ExitCode)"
`;

let winScript;
function runOnce(bin, args, work) {
  if (process.platform === "win32") {
    if (!winScript) {
      winScript = join(work, "peak.ps1");
      writeFileSync(winScript, WIN_PEAK);
    }
    const quoted = args.map((a) => `"${a.replace(/"/g, '\\"')}"`).join(" ");
    const r = spawnSync(
      "powershell",
      ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", winScript],
      {
        env: { ...process.env, PERF_GATE_EXE: bin, PERF_GATE_ARGS: quoted },
        encoding: "utf8",
        maxBuffer: 256 * 1024 * 1024,
      },
    );
    const m = /@@perf-gate ms=([\d.,]+) peak=(\d+) exit=(-?\d+)/.exec(r.stdout ?? "");
    if (!m) fail(bin, args, r);
    if (m[3] !== "0") fail(bin, args, { ...r, status: Number(m[3]) });
    const peak = Number(m[2]);
    return {
      secs: Number(m[1].replace(",", ".")) / 1000,
      rss: peak > 0 ? peak : null,
      stdout: r.stdout.slice(0, m.index),
    };
  }
  const time = "/usr/bin/time";
  const timed = existsSync(time) && (process.platform === "linux" || process.platform === "darwin");
  const report = join(work, "time.txt");
  const [cmd, full] = timed
    ? [time, [process.platform === "linux" ? "-v" : "-l", "-o", report, bin, ...args]]
    : [bin, args];
  const t0 = process.hrtime.bigint();
  const r = spawnSync(cmd, full, { encoding: "utf8", maxBuffer: 256 * 1024 * 1024 });
  const secs = Number(process.hrtime.bigint() - t0) / 1e9;
  if (r.status !== 0) fail(bin, args, r);
  let rss = null;
  if (timed) {
    const text = readFileSync(report, "utf8");
    const linux = /Maximum resident set size \(kbytes\):\s*(\d+)/.exec(text);
    const mac = /(\d+)\s+maximum resident set size/.exec(text);
    if (linux) rss = Number(linux[1]) * 1024;
    else if (mac) rss = Number(mac[1]);
  }
  return { secs, rss, stdout: r.stdout };
}

function fail(bin, args, r) {
  console.error(`perf-gate: ${bin} ${args.join(" ")} failed (exit ${r.status ?? r.error})`);
  console.error((r.stdout ?? "").slice(-4000));
  console.error((r.stderr ?? "").slice(-4000));
  process.exit(2);
}

const median = (xs) => {
  const v = xs.filter((x) => x != null).sort((a, b) => a - b);
  if (!v.length) return null;
  const m = v.length >> 1;
  return v.length % 2 ? v[m] : (v[m - 1] + v[m]) / 2;
};

function dbBytes(dir) {
  let n = 0;
  for (const f of ["graph.db", "graph.db-wal"]) {
    const p = join(dir, f);
    if (existsSync(p)) n += statSync(p).size;
  }
  return n;
}

function measure(o) {
  const work = o.work ?? mkdtempSync(join(tmpdir(), "cartograph-perf-"));
  mkdirSync(work, { recursive: true });
  const bins = { base: o.base, head: o.head };
  const results = [];
  for (const c of o.corpora) {
    const r = { corpus: c.name, base: {}, head: {}, files: "" };
    const dir = (side) => join(work, `${c.name}-${side}`);
    const index = (side) => ["index", c.path, "--data-dir", dir(side)];
    const cold = { base: [], head: [] };
    const rss = { base: [], head: [] };
    // One untimed run first, so the first timed one does not read a cold disk alone.
    rmSync(dir("base"), { recursive: true, force: true });
    runOnce(bins.base, index("base"), work);
    for (let i = 0; i < o.runs; i++) {
      // Base first on odd runs, head first on even ones.
      for (const side of i % 2 ? ["head", "base"] : ["base", "head"]) {
        rmSync(dir(side), { recursive: true, force: true });
        const x = runOnce(bins[side], index(side), work);
        cold[side].push(x.secs);
        rss[side].push(x.rss);
        if (side === "head") r.files = x.stdout.split("\n")[0].replace(/ in \d+ ms.*$/, "");
        process.stderr.write(`${c.name} cold ${side} ${x.secs.toFixed(2)} s\n`);
      }
    }
    const warm = { base: [], head: [] };
    for (let i = 0; i < o.runs; i++) {
      for (const side of i % 2 ? ["head", "base"] : ["base", "head"]) {
        warm[side].push(runOnce(bins[side], index(side), work).secs);
      }
    }
    const queries = o.queries.filter((q) => q.corpus === c.name);
    for (const side of ["base", "head"]) {
      r[side].cold = median(cold[side]);
      r[side].rss = median(rss[side]);
      r[side].size = dbBytes(dir(side));
      r[side].warm = median(warm[side]);
      r[side].query = queries.length ? 0 : null;
    }
    for (const q of queries) {
      const t = { base: [], head: [] };
      for (let i = 0; i < o.runs; i++) {
        for (const side of i % 2 ? ["head", "base"] : ["base", "head"]) {
          const args = ["query", "-C", c.path, "--data-dir", dir(side), ...q.args];
          t[side].push(runOnce(bins[side], args, work).secs);
        }
      }
      for (const side of ["base", "head"]) r[side].query += median(t[side]);
    }
    r.queries = queries.map((q) => q.args.join(" "));
    results.push(r);
  }
  if (!o.work) rmSync(work, { recursive: true, force: true });
  return results;
}

const METRICS = [
  { key: "cold", label: "cold index (median)", time: true },
  { key: "rss", label: "peak RSS (median)" },
  { key: "size", label: "DB size" },
  { key: "warm", label: "warm index (median)", time: true },
  { key: "query", label: "queries (sum of medians)", time: true },
];

const fmtSecs = (s) => `${s.toFixed(2)} s`;
const fmtBytes = (b) => `${(b / 1048576).toFixed(1)} MiB`;

function judge(o, results) {
  const rows = [];
  let failed = false;
  for (const r of results) {
    for (const m of METRICS) {
      const b = r.base[m.key];
      const h = r.head[m.key];
      if (b == null || h == null) continue;
      const fmt = m.time ? fmtSecs : fmtBytes;
      const change = b > 0 ? ((h - b) / b) * 100 : 0;
      const limit = o.limits[m.key];
      let verdict = "info";
      if (limit > 0) {
        const over = change > limit && (!m.time || h - b > o.minTimeDelta);
        verdict = over ? "FAIL" : "ok";
        failed ||= over;
      }
      rows.push({
        corpus: r.corpus,
        metric: m.label,
        base: fmt(b),
        head: fmt(h),
        change: `${change >= 0 ? "+" : ""}${change.toFixed(1)} %`,
        limit: limit > 0 ? `+${limit} %` : "-",
        verdict,
      });
    }
  }
  return { rows, failed };
}

function table(rows) {
  const head = ["corpus", "metric", "base", "head", "change", "limit", "result"];
  const cells = rows.map((r) => [r.corpus, r.metric, r.base, r.head, r.change, r.limit, r.verdict]);
  const w = head.map((h, i) => Math.max(h.length, ...cells.map((c) => c[i].length)));
  const right = [false, false, true, true, true, true, false];
  const line = (c) =>
    `| ${c.map((x, i) => (right[i] ? x.padStart(w[i]) : x.padEnd(w[i]))).join(" | ")} |`;
  const rule = `|${w.map((n, i) => (right[i] ? `${"-".repeat(n + 1)}:` : "-".repeat(n + 2))).join("|")}|`;
  return [line(head), rule, ...cells.map(line)].join("\n");
}

const o = parseArgs(process.argv.slice(2));
const results = measure(o);
const { rows, failed } = judge(o, results);
const notes = results.map(
  (r) =>
    `- ${r.corpus}: ${r.files || "indexed"}` +
    (r.queries.length ? `; queries: ${r.queries.map((q) => `\`${q}\``).join(", ")}` : ""),
);
const text =
  `${table(rows)}\n\n${notes.join("\n")}\n\n` +
  `${o.runs} runs per binary; a time fails only past its limit and ${o.minTimeDelta} s. ` +
  (failed ? "**Over budget.**" : "Within budget.") +
  "\n";
console.log(text);
if (process.env.GITHUB_STEP_SUMMARY) {
  appendFileSync(process.env.GITHUB_STEP_SUMMARY, `## Performance budget\n\n${text}`);
}
if (o.json) writeFileSync(o.json, JSON.stringify({ results, rows, failed }, null, 2));
process.exit(failed ? 1 : 0);
