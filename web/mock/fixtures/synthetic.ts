// Fixture B: a synthetic ~5k-file polyglot monorepo for performance work.
// Deterministic: same seed, same repo.
import type { RawFile, RawRepo } from "../model";
import { rng } from "./build.ts";

const SERVICES: { name: string; lang: string; ext: string; modules: number }[] = [
  { name: "core", lang: "typescript", ext: "ts", modules: 14 },
  { name: "api", lang: "typescript", ext: "ts", modules: 16 },
  { name: "web", lang: "tsx", ext: "tsx", modules: 22 },
  { name: "admin", lang: "tsx", ext: "tsx", modules: 12 },
  { name: "worker", lang: "go", ext: "go", modules: 12 },
  { name: "gateway", lang: "go", ext: "go", modules: 8 },
  { name: "ml", lang: "python", ext: "py", modules: 12 },
  { name: "pipelines", lang: "python", ext: "py", modules: 9 },
  { name: "engine", lang: "rust", ext: "rs", modules: 12 },
  { name: "billing", lang: "java", ext: "java", modules: 10 },
  { name: "mobile-shared", lang: "typescript", ext: "ts", modules: 8 },
  { name: "tools", lang: "javascript", ext: "js", modules: 6 },
];

const WORDS = ["account", "audit", "cache", "catalog", "config", "events", "graph", "ingest", "ledger", "media", "orders", "policy", "queue", "report", "search", "session", "storage", "sync", "tenant", "users", "vault", "workflow", "metrics", "notify", "pricing", "router", "schema", "shipping", "geo", "export"];
const SUBS = ["model", "service", "handlers", "util", "adapters", "views"];
const EXTS = ["npm:lodash", "npm:zod", "npm:react", "npm:express", "npm:pino", "pypi:numpy", "pypi:pandas", "pypi:torch", "go:github.com/gin-gonic/gin", "cargo:serde", "cargo:tokio", "maven:org.springframework", "npm:date-fns", "npm:axios", "pypi:requests"];

export function syntheticFixture(seed = 7): RawRepo {
  const rand = rng(seed);
  const pick = <T>(xs: readonly T[]) => xs[Math.floor(rand() * xs.length)]!;
  const files: RawFile[] = [];
  const byModule = new Map<string, string[]>();
  const coreFiles: string[] = [];

  for (const svc of SERVICES) {
    for (let m = 0; m < svc.modules; m++) {
      const mod = `${WORDS[(m * 7 + svc.name.length) % WORDS.length]}${m >= WORDS.length ? m : ""}`;
      const modKey = `packages/${svc.name}/src/${mod}`;
      const list: string[] = [];
      for (const sub of SUBS) {
        const count = 3 + Math.floor(rand() * 7);
        for (let i = 0; i < count; i++) {
          const path = `${modKey}/${sub}/${sub}_${i}_${Math.floor(rand() * 1000)}.${svc.ext}`;
          list.push(path);
          if (svc.name === "core") coreFiles.push(path);
        }
      }
      byModule.set(modKey, list);
    }
  }

  const all = [...byModule.values()].flat();
  const moduleKeys = [...byModule.keys()];
  for (const [modKey, list] of byModule) {
    const svcName = modKey.split("/")[1]!;
    const svc = SERVICES.find((s) => s.name === svcName)!;
    for (const path of list) {
      const imports: RawFile["imports"] = [];
      const n = 2 + Math.floor(rand() * 5);
      for (let i = 0; i < n; i++) {
        const r = rand();
        let target: string;
        if (r < 0.55) target = pick(list);
        else if (r < 0.8) target = pick(byModule.get(pick(moduleKeys.filter((k) => k.startsWith(`packages/${svcName}/`))))!);
        else if (r < 0.93 && coreFiles.length) target = pick(coreFiles.slice(0, 120));
        else target = pick(all);
        if (target === path) continue;
        imports.push({ line: imports.length + 1, specifier: target, to: { kind: "file", path: target, confidence: rand() < 0.85 ? "resolved" : "heuristic" } });
      }
      if (rand() < 0.5) imports.push({ line: imports.length + 1, specifier: "ext", to: { kind: "ext", ext: pick(EXTS) } });
      if (rand() < 0.02) {
        imports.push({ line: imports.length + 1, specifier: "./missing", to: { kind: "unresolved", reason: "not_found", detail: "ningún archivo coincide" } });
      }
      files.push({ path, lang: svc.lang, loc: 20 + Math.floor(rand() * rand() * 600), imports });
    }
  }
  files.push({ path: "README.md", lang: "other", loc: 120, imports: [] });
  return {
    name: "constelacion",
    root: "D:/projects/demo/constelacion",
    files,
    skipped: [],
    diagnostics: [],
    packages: SERVICES.map((s) => ({
      id: `pkg:packages/${s.name}/package.json`,
      name: s.name,
      kind: "npm",
      dir: `dir:packages/${s.name}`,
      manifest: `file:packages/${s.name}/package.json`,
      notes: null,
    })),
  };
}
