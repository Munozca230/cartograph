import { afterEach, describe, expect, it, vi } from "vitest";
import type { IncomingMessage, ServerResponse } from "node:http";
import { MockServer } from "../mock/server";

/** One GET against the mock API: status and JSON body. */
function get(server: MockServer, url: string): { status: number; body: Record<string, unknown> } {
  const res = { statusCode: 0, body: "", setHeader() {}, end(b: string) { this.body = b; } };
  server.handle({ url, method: "GET" } as IncomingMessage, res as unknown as ServerResponse);
  return { status: res.statusCode, body: JSON.parse(res.body) as Record<string, unknown> };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("mock server", () => {
  it("merges the reasons and changed paths of re-indexes queued behind a running one", () => {
    vi.useFakeTimers();
    const events: { event: string; data: { reason?: string; changed?: string[] } }[] = [];
    const server = new MockServer({ fixture: "saas", watchMs: 0, startupMs: 500, onEvent: (event, data) => events.push({ event, data: data as never }) });
    vi.advanceTimersByTime(1000);
    server.reindex("watch", ["a.ts"], 500);
    server.reindex("watch", ["c.ts"], 500);
    server.reindex("manual", ["b.ts"], 500);
    vi.advanceTimersByTime(3000);
    server.dispose();
    const started = events.filter((e) => e.event === "index-started").map((e) => e.data);
    const finished = events.filter((e) => e.event === "index-finished").map((e) => e.data);
    expect(started.map((s) => s.reason)).toEqual(["startup", "watch", "manual"]);
    expect(started[2]!.changed).toEqual(["b.ts", "c.ts"]);
    expect(finished[2]!.changed).toEqual(["b.ts", "c.ts"]);
  });

  it("parses /api/graph like the Rust server: plain-digit depths, each ignored id once", () => {
    vi.useFakeTimers();
    const server = new MockServer({ fixture: "saas", watchMs: 0, startupMs: 10 });
    vi.advanceTimersByTime(2000);
    const g = get(server, "/api/graph?expanded=dir:nope,dir:nope&collapsed=dir:nope&collapsed=repo:&collapsed=repo:");
    expect(g.status).toBe(200);
    expect(g.body.ignored).toEqual(["dir:nope", "repo:"]);
    const all = get(server, "/api/graph?depth=all").body.nodes;
    expect(get(server, "/api/graph?depth=99999999999999999999999").body.nodes).toEqual(all);
    for (const bad of ["0", "00", "-1", "+5", "%2B5", "%205", "5%20", "1.0", "abc", ""]) {
      const r = get(server, `/api/graph?depth=${bad}`);
      expect(r.status, `depth=${bad}`).toBe(400);
      expect((r.body.error as { code: string }).code).toBe("bad_request");
    }
    server.dispose();
  });
});
