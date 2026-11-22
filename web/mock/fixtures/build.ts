// Turns a compact fixture description (paths + import specifiers) into a RawRepo,
// resolving specifiers the way a TS indexer would: `@/` alias, relative paths,
// extension and index probing. Bare specifiers become external packages.
import type { PackageInfo } from "../../src/api/types";
import type { RawFile, RawImport, RawRepo } from "../model";

export interface FileSpec {
  path: string;
  loc: number;
  imports: string[];
  lang?: string;
}

const EXT_LANG: Record<string, string> = {
  ts: "typescript",
  tsx: "tsx",
  js: "javascript",
  mjs: "javascript",
  jsx: "javascript",
  py: "python",
  rs: "rust",
  go: "go",
  java: "java",
  cs: "csharp",
  cpp: "cpp",
  c: "c",
  gd: "gdscript",
};

export function langOf(path: string): string {
  const ext = path.slice(path.lastIndexOf(".") + 1);
  return EXT_LANG[ext] ?? "other";
}

function normalize(parts: string[]): string {
  const out: string[] = [];
  for (const p of parts) {
    if (p === "" || p === ".") continue;
    if (p === "..") out.pop();
    else out.push(p);
  }
  return out.join("/");
}

function packageName(spec: string): string {
  if (spec.startsWith("node:")) return spec;
  const parts = spec.split("/");
  return spec.startsWith("@") ? parts.slice(0, 2).join("/") : parts[0]!;
}

export interface BuildOptions {
  name: string;
  root: string;
  alias?: Record<string, string>;
  packages?: PackageInfo[];
  skipped?: RawRepo["skipped"];
  diagnostics?: RawRepo["diagnostics"];
}

export function buildRepo(specs: FileSpec[], opts: BuildOptions): RawRepo {
  const known = new Set(specs.map((s) => s.path));
  const probe = (base: string): string | null => {
    const candidates = [
      base,
      `${base}.ts`,
      `${base}.tsx`,
      `${base}.js`,
      `${base}.mjs`,
      `${base}/index.ts`,
      `${base}/index.tsx`,
    ];
    return candidates.find((c) => known.has(c)) ?? null;
  };
  const files: RawFile[] = specs.map((spec) => {
    const dir = spec.path.includes("/") ? spec.path.slice(0, spec.path.lastIndexOf("/")) : "";
    const imports: RawImport[] = spec.imports.map((raw, i) => {
      const line = 1 + i;
      // `!reason:detail:specifier` forces an unresolved import with a given reason.
      if (raw.startsWith("!")) {
        const [reason, detail, specifier] = raw.slice(1).split("|") as [string, string, string];
        return { line, specifier, to: { kind: "unresolved", reason, detail } };
      }
      const heuristic = raw.startsWith("~");
      const specifier = heuristic ? raw.slice(1) : raw;
      let base: string | null = null;
      for (const [prefix, target] of Object.entries(opts.alias ?? {})) {
        if (specifier.startsWith(prefix)) base = normalize([target, specifier.slice(prefix.length)].join("/").split("/"));
      }
      if (base === null && (specifier.startsWith("./") || specifier.startsWith("../"))) {
        base = normalize([...dir.split("/"), ...specifier.split("/")]);
      }
      if (base !== null) {
        const hit = probe(base);
        if (hit) {
          return {
            line,
            specifier,
            to: { kind: "file", path: hit, confidence: heuristic ? "heuristic" : specifier.startsWith("@") ? "resolved" : "exact" },
          };
        }
        return {
          line,
          specifier,
          to: { kind: "unresolved", reason: "not_found", detail: `ningún archivo coincide con ${base}{.ts,.tsx,/index.ts}` },
        };
      }
      const pkg = packageName(specifier);
      const ext = pkg.startsWith("node:") ? pkg : `npm:${pkg}`;
      return { line, specifier, to: { kind: "ext", ext } };
    });
    return { path: spec.path, lang: spec.lang ?? langOf(spec.path), loc: spec.loc, imports };
  });
  return {
    name: opts.name,
    root: opts.root,
    files,
    skipped: opts.skipped ?? [],
    diagnostics: opts.diagnostics ?? [],
    packages: opts.packages ?? [],
  };
}

/** Small deterministic PRNG (mulberry32). */
export function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
