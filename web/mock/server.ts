// Framework-free mock of `cartograph serve`: routes, SSE and a simulated indexer.
// Used by the Vite dev middleware (./plugin.ts) and by tests.
import type { IncomingMessage, ServerResponse } from "node:http";
import type { Counts, Meta } from "../src/api/types";
import { saasFixture } from "./fixtures/saas.ts";
import { syntheticFixture } from "./fixtures/synthetic.ts";
import { ApiError, MockIndex, type RawRepo } from "./model.ts";

export interface MockOptions {
  fixture: string;
  /** Interval of simulated watcher re-indexes, in ms. 0 disables them. */
  watchMs: number;
  /** Duration of the first (startup) index, in ms. */
  startupMs?: number;
  /** Observes every SSE event the server broadcasts (for tests). */
  onEvent?: (event: string, data: unknown) => void;
}

type Reason = "startup" | "watch" | "manual";

type Client = ServerResponse;

export class MockServer {
  private repo: RawRepo;
  private index: MockIndex | null = null;
  private generation = 0;
  private status: Meta["status"] = "indexing";
  private reindexing = false;
  private progress: { done: number; total: number } | null = null;
  private indexedAt: string | null = null;
  private indexMs: number | null = null;
  private clients = new Set<Client>();
  private tick = 0;
  private timers: NodeJS.Timeout[] = [];
  /** Exactly one re-index waits behind the running one; its reasons and paths accumulate. */
  private queued: { reason: Reason; changed: Set<string> } | null = null;

  constructor(private readonly opts: MockOptions) {
    this.repo = opts.fixture === "perf" || opts.fixture === "synthetic" ? syntheticFixture() : saasFixture();
    this.runIndex("startup", [], opts.startupMs ?? 2200);
    if (opts.watchMs > 0) {
      this.timers.push(setInterval(() => this.watchTick(), opts.watchMs));
    }
    this.timers.push(setInterval(() => this.broadcastComment(), 15000));
  }

  dispose() {
    for (const t of this.timers) clearInterval(t);
    for (const c of this.clients) c.end();
  }

  private broadcast(event: string, data: unknown) {
    this.opts.onEvent?.(event, data);
    const payload = `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
    for (const c of this.clients) c.write(payload);
  }

  private broadcastComment() {
    for (const c of this.clients) c.write(": keepalive\n\n");
  }

  /** Starts a re-index, or queues it behind the running one (contract: one queued run, "manual" wins). */
  reindex(reason: Reason, changed: string[], durationMs: number) {
    this.runIndex(reason, changed, durationMs);
  }

  private runIndex(reason: Reason, changed: string[], durationMs: number) {
    if (this.progress) {
      const q = (this.queued ??= { reason, changed: new Set() });
      if (reason === "manual") q.reason = "manual";
      for (const p of changed) q.changed.add(p);
      return;
    }
    const total = this.repo.files.length;
    const started = Date.now();
    if (this.status !== "indexing") this.reindexing = true;
    this.progress = { done: 0, total };
    this.broadcast("index-started", { reason, changed });
    const steps = Math.max(2, Math.round(durationMs / 250));
    let step = 0;
    const timer = setInterval(() => {
      step += 1;
      const done = Math.min(total, Math.round((total * step) / steps));
      this.progress = { done, total };
      this.broadcast("index-progress", this.progress);
      if (step < steps) return;
      clearInterval(timer);
      try {
        this.index = new MockIndex(this.repo);
      } catch (err) {
        this.progress = null;
        this.reindexing = false;
        this.status = "error";
        this.broadcast("index-error", { message: String(err) });
        return;
      }
      this.generation += 1;
      this.status = "ready";
      this.reindexing = false;
      this.progress = null;
      this.indexedAt = new Date().toISOString().replace(/\.\d+Z$/, "Z");
      this.indexMs = Date.now() - started;
      this.broadcast("index-finished", {
        generation: this.generation,
        ms: this.indexMs,
        changed,
        counts: this.index.counts(),
      });
      if (this.queued) {
        const next = this.queued;
        this.queued = null;
        this.runIndex(next.reason, [...next.changed].sort(), durationMs);
      }
    }, 250);
    this.timers.push(timer);
  }

  /** Simulates a file save: a few files change size, and every third tick a new query module appears. */
  private watchTick() {
    if (this.status !== "ready") return;
    this.tick += 1;
    const changed = mutateRepo(this.repo, this.tick);
    this.runIndex("watch", changed, 1400);
  }

  meta(): Meta {
    return {
      generation: this.generation,
      status: this.status,
      progress: this.progress,
      reindexing: this.reindexing,
      repo: { name: this.repo.name, root: this.repo.root },
      indexed_at: this.indexedAt,
      index_ms: this.indexMs,
      counts: this.index ? (this.index.counts() as Counts) : null,
      languages: this.index ? this.index.languages() : [],
      watching: this.opts.watchMs > 0,
    };
  }

  /** Returns true when the request was handled. */
  handle(req: IncomingMessage, res: ServerResponse): boolean {
    const url = new URL(req.url ?? "/", "http://mock.local");
    if (!url.pathname.startsWith("/api/")) return false;
    const send = (status: number, body: unknown) => {
      res.statusCode = status;
      res.setHeader("content-type", "application/json; charset=utf-8");
      res.setHeader("cache-control", "no-store");
      res.end(JSON.stringify(body));
    };
    const route = url.pathname.slice(5);
    try {
      if (route === "events") {
        res.writeHead(200, {
          "content-type": "text/event-stream",
          "cache-control": "no-cache",
          connection: "keep-alive",
        });
        res.write(`event: hello\ndata: ${JSON.stringify({ generation: this.generation, status: this.status })}\n\n`);
        this.clients.add(res);
        req.on("close", () => this.clients.delete(res));
        return true;
      }
      if (route === "meta") {
        send(200, this.meta());
        return true;
      }
      if (route === "reindex") {
        if (req.method !== "POST") throw new ApiError(405, "bad_request", "usa POST");
        this.runIndex("manual", [], 1200);
        send(202, { accepted: true, generation: this.generation });
        return true;
      }
      const index = this.index;
      if (!index) {
        res.statusCode = 503;
        send(503, {
          generation: this.generation,
          error: { code: "indexing", message: "indexando por primera vez" },
          progress: this.progress,
        });
        return true;
      }
      const g = this.generation;
      const q = url.searchParams;
      switch (route) {
        case "tree": {
          const depth = q.get("depth");
          send(200, { generation: g, root: index.tree(q.get("root") ?? "repo:", depth ? Number(depth) : undefined) });
          return true;
        }
        case "graph": {
          // Contract: repeated params; a value that is exactly a dir id stays whole, else it is split on commas.
          const ids = (key: string) => q.getAll(key).flatMap((v) => (index.hasDir(v) ? [v] : v.split(","))).filter(Boolean);
          const raw = q.get("depth");
          const depth = raw === null ? 1 : raw.toLowerCase() === "all" ? "all" : /^\d+$/.test(raw) && Number(raw) >= 1 ? Number(raw) : null;
          if (depth === null) throw new ApiError(400, "bad_request", `depth must be an integer of at least 1 or "all", got ${JSON.stringify(raw)}`);
          send(200, { generation: g, ...index.graph(ids("expanded"), { depth, collapsed: ids("collapsed") }) });
          return true;
        }
        case "node": {
          const id = q.get("id");
          if (!id) throw new ApiError(400, "bad_request", "falta id");
          send(200, { generation: g, ...index.node(id) });
          return true;
        }
        case "packages":
          send(200, { generation: g, packages: this.repo.packages });
          return true;
        case "findings":
          send(200, { generation: g, findings: [], summary: { total: 0, violations: 0, by_severity: {}, by_kind: {} }, config: { origin: null, error: null }, total: 0, truncated: false });
          return true;
        case "issues":
          send(200, { generation: g, ...index.issues(Number(q.get("limit") ?? 500)) });
          return true;
        case "search":
          send(200, { generation: g, results: index.search(q.get("q") ?? "", Number(q.get("limit") ?? 50)) });
          return true;
        default:
          throw new ApiError(404, "not_found", `ruta desconocida: ${url.pathname}`);
      }
    } catch (err) {
      if (err instanceof ApiError) {
        send(err.status, { generation: this.generation, error: { code: err.code, message: err.message } });
      } else {
        send(500, { generation: this.generation, error: { code: "internal", message: String(err) } });
      }
      return true;
    }
  }
}

export function mutateRepo(repo: RawRepo, tick: number): string[] {
  const changed: string[] = [];
  const n = repo.files.length;
  for (let i = 0; i < 3; i++) {
    const f = repo.files[(tick * 37 + i * 101) % n]!;
    f.loc = Math.max(5, f.loc + ((tick + i) % 2 === 0 ? 23 : -11));
    changed.push(f.path);
  }
  if (tick % 3 === 1 && repo.files.some((f) => f.path === "src/lib/db/schema.ts")) {
    const path = `src/lib/db/queries/report-${tick}.ts`;
    repo.files.push({
      path,
      lang: "typescript",
      loc: 64,
      imports: [
        { line: 1, specifier: "drizzle-orm", to: { kind: "ext", ext: "npm:drizzle-orm" } },
        { line: 2, specifier: "@/lib/db/schema", to: { kind: "file", path: "src/lib/db/schema.ts", confidence: "resolved" } },
        { line: 3, specifier: "@/lib/db/with-tenant", to: { kind: "file", path: "src/lib/db/with-tenant.ts", confidence: "resolved" } },
      ],
    });
    changed.push(path);
  }
  return changed.sort();
}
