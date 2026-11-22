import { describe, expect, it } from "vitest";
import { saasFixture, SAAS_FILE_COUNT } from "../mock/fixtures/saas";
import { syntheticFixture } from "../mock/fixtures/synthetic";
import { MockIndex } from "../mock/model";
import { buildHierarchy, frontier, idForIssuePath, normalizeExpanded } from "../src/model/hierarchy";
import { representativeOf } from "../src/state/controller";

const index = new MockIndex(saasFixture());
const h = buildHierarchy(index.tree());

describe("fixture A", () => {
  it("is a ~170-file Next.js SaaS whose core is the db layer", () => {
    expect(SAAS_FILE_COUNT).toBeGreaterThanOrEqual(160);
    expect(SAAS_FILE_COUNT).toBeLessThanOrEqual(185);
    const files = [...h.nodes.values()].filter((n) => n.kind === "file").sort((a, b) => b.fanIn - a.fanIn);
    const top = files.slice(0, 3).map((f) => f.path);
    expect(top).toContain("src/lib/db/schema.ts");
    expect(top).toContain("src/lib/db/with-tenant.ts");
  });
});

describe("mock rollup (contract semantics)", () => {
  const cases: string[][] = [
    [],
    ["dir:src"],
    ["dir:src", "dir:src/lib"],
    ["dir:src", "dir:src/lib", "dir:src/lib/db", "dir:src/app", "dir:src/app/(app)"],
  ];
  it.each(cases)("sum of edge weights equals crossing file pairs (%j)", (...expanded) => {
    const g = index.graph(expanded);
    const sum = g.edges.reduce((s, e) => s + e.weight, 0);
    expect(sum).toBe(index.crossingPairs(expanded));
    for (const e of g.edges) {
      expect(e.confidence.exact + e.confidence.resolved + e.confidence.heuristic).toBe(e.weight);
      expect(e.src).not.toBe(e.dst);
    }
  });

  it("ignores dirs whose parent is not expanded, and accepts the dir: alias", () => {
    const g = index.graph(["dir:src/lib/db", "dir:", "dir:nope"]);
    // Same order as the server: unknown ids first, then the ones that did not open.
    expect(g.ignored).toEqual(["dir:nope", "dir:src/lib/db"]);
    expect(g.nodes.some((n) => n.id === "dir:src")).toBe(true);
  });

  it("weighted degrees match the edges", () => {
    const g = index.graph(["dir:src", "dir:src/lib"]);
    const lib = g.nodes.filter((n) => n.parent === "dir:src/lib");
    // db is the most imported region of lib, then auth.
    const ranked = [...lib].sort((a, b) => b.fan_in - a.fan_in).map((n) => n.id);
    expect(ranked.slice(0, 2)).toEqual(["dir:src/lib/db", "dir:src/lib/auth"]);
    for (const n of lib) {
      expect(n.fan_in).toBe(g.edges.filter((e) => e.dst === n.id).reduce((s, e) => s + e.weight, 0));
    }
  });
});

describe("client frontier mirrors the server", () => {
  it.each([[[]], [["dir:src"]], [["dir:src", "dir:src/lib", "dir:src/lib/db"]]])("for %j", (expanded) => {
    const server = index.graph(expanded).nodes.map((n) => n.id);
    expect(frontier(h, expanded)).toEqual(server);
  });

  it("finds the visible representative of a hidden path, even one the index skipped", () => {
    const set = new Set(frontier(h, ["dir:src", "dir:src/lib"]));
    expect(representativeOf(set, "src/lib/db/schema.ts")).toBe("dir:src/lib/db");
    expect(representativeOf(set, "src/lib/auth")).toBe("dir:src/lib/auth");
    expect(representativeOf(set, "src/lib/db/vendor/huge.min.js")).toBe("dir:src/lib/db");
    expect(representativeOf(set, "nowhere/x.ts")).toBeNull();
  });

  it("maps issue paths and store-style ids to node ids", () => {
    expect(idForIssuePath(h, "src/lib/db/schema.ts")).toBe("file:src/lib/db/schema.ts");
    expect(idForIssuePath(h, "src/lib/db")).toBe("dir:src/lib/db");
    expect(idForIssuePath(h, "package:package.json")).toBe("pkg:package.json");
    expect(idForIssuePath(h, "external:npm:react")).toBe("ext:npm:react");
    expect(idForIssuePath(h, "file:src/env.ts")).toBe("file:src/env.ts");
  });

  it("drops orphans when normalizing", () => {
    expect(normalizeExpanded(h, ["dir:src/lib", "file:src/env.ts", "repo:"])).toEqual([]);
  });
});

describe("fixture B", () => {
  it("has about 5k files and a rollup that keeps the invariant", () => {
    const big = new MockIndex(syntheticFixture());
    expect(big.files.size).toBeGreaterThan(4500);
    expect(big.files.size).toBeLessThan(6000);
    const expanded = ["dir:packages", "dir:packages/web", "dir:packages/core"];
    const g = big.graph(expanded);
    expect(g.edges.reduce((s, e) => s + e.weight, 0)).toBe(big.crossingPairs(expanded));
  });
});
