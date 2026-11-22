import { describe, expect, it } from "vitest";
import { ApiClient, ApiError } from "../src/api/client";
import { createSearch, groupResults, matchRanges, toPaletteResult } from "../src/api/search";

function jsonResponse(status: number, body: unknown) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

describe("search adapter", () => {
  it("highlights a contiguous match, else a subsequence", () => {
    expect(matchRanges("with-tenant.ts", "tenant")).toEqual([[5, 11]]);
    expect(matchRanges("with-tenant.ts", "wtt")).toEqual([[0, 1], [2, 3], [5, 6]]);
    expect(matchRanges("schema.ts", "xyz")).toEqual([]);
  });

  it("maps results to palette items with context", () => {
    const file = toPaletteResult({ id: "file:src/lib/db/schema.ts", kind: "file", path: "src/lib/db/schema.ts", name: "schema.ts", score: 9 }, "sch");
    expect(file).toMatchObject({ value: "file:src/lib/db/schema.ts", label: "schema.ts", context: "src/lib/db", group: "Archivos" });
    const ext = toPaletteResult({ id: "ext:npm:drizzle-orm", kind: "ext", path: "npm:drizzle-orm", name: "drizzle-orm", score: 3 }, "dri");
    expect(ext).toMatchObject({ context: "paquete npm", group: "Paquetes externos" });
    const top = toPaletteResult({ id: "file:next.config.mjs", kind: "file", path: "next.config.mjs", name: "next.config.mjs", score: 1 }, "next");
    expect(top.context).toBe("raíz del repositorio");
  });

  it("groups by kind keeping the server order", () => {
    const items = [
      toPaletteResult({ id: "file:a.ts", kind: "file", path: "a.ts", name: "a.ts", score: 5 }, "a"),
      toPaletteResult({ id: "dir:a", kind: "dir", path: "a", name: "a", score: 4 }, "a"),
      toPaletteResult({ id: "file:b/a.ts", kind: "file", path: "b/a.ts", name: "a.ts", score: 3 }, "a"),
    ];
    const groups = groupResults(items);
    expect(groups.map((g) => g.group)).toEqual(["Carpetas", "Archivos"]);
    expect(groups[1]!.items.map((i) => i.value)).toEqual(["file:a.ts", "file:b/a.ts"]);
  });

  it("reads the wrapped {generation, results} body, encodes the query and dedupes", async () => {
    const urls: string[] = [];
    const client = new ApiClient("http://x/api/", async (input) => {
      urls.push(input);
      return jsonResponse(200, {
        generation: 2,
        results: [
          { id: "dir:src/app/(app)", kind: "dir", path: "src/app/(app)", name: "(app)", score: 3 },
          { id: "dir:src/app/(app)", kind: "dir", path: "src/app/(app)", name: "(app)", score: 3 },
        ],
      });
    });
    const search = createSearch(client, 20);
    const out = await search(" (app) ", { signal: new AbortController().signal });
    expect(out).toHaveLength(1);
    expect(urls[0]).toBe("http://x/api/search?q=(app)&limit=20".replace("(app)", encodeURIComponent("(app)")));
    expect(await search("   ", { signal: new AbortController().signal })).toEqual([]);
    expect(urls).toHaveLength(1);
  });

  it("sends expanded ids as repeated params, so commas in folder names survive", () => {
    const client = new ApiClient("http://x/api/", async () => jsonResponse(200, {}));
    const url = new URL(client.url("graph", { expanded: ["dir:src", "dir:src/a,b"] }));
    expect(url.searchParams.getAll("expanded")).toEqual(["dir:src", "dir:src/a,b"]);
  });

  it("reports a failed reindex request as an error", async () => {
    const client = new ApiClient("/api/", async () =>
      jsonResponse(500, { generation: 3, error: { code: "internal", message: "boom" } }),
    );
    const err = await client.reindex().catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(500);
    expect((err as ApiError).message).toBe("boom");
  });

  it("surfaces 503 indexing errors with progress", async () => {
    const client = new ApiClient("/api/", async () =>
      jsonResponse(503, { generation: 0, error: { code: "indexing", message: "indexando" }, progress: { done: 4, total: 10 } }),
    );
    const err = await client.search("x").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).indexing).toBe(true);
    expect((err as ApiError).progress).toEqual({ done: 4, total: 10 });
  });
});
