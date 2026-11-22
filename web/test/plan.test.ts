import ELK from "elkjs/lib/elk.bundled.js";
import { describe, expect, it } from "vitest";
import { saasFixture } from "../mock/fixtures/saas";
import { MockIndex } from "../mock/model";
import type { GraphResponse } from "../src/api/types";
import { buildHierarchy } from "../src/model/hierarchy";
import { buildPlan, center, flyTarget, layoutPlan, type Plan, type PlanLayout, type Rect } from "../src/model/plan";
import { atLevel, graphQuery, openDirs, openFolder, TOP_LEVEL, type ViewSpec } from "../src/model/view";

const elk = new ELK();

/** A /api/graph body from [src, dst, weight] triples. Node parents come from the paths. */
function graph(edges: [string, string, number][], extraNodes: string[] = []): GraphResponse {
  const ids = [...new Set([...edges.flatMap(([s, d]) => [s, d]), ...extraNodes])].map((p) => `dir:${p}`);
  return {
    generation: 1,
    nodes: ids.map((id) => {
      const path = id.slice(4);
      const i = path.lastIndexOf("/");
      return {
        id,
        kind: "dir" as const,
        name: path.slice(i + 1),
        path,
        parent: i < 0 ? "repo:" : `dir:${path.slice(0, i)}`,
        loc: 100,
        files: 3,
        langs: { typescript: 3 },
        fan_in: 0,
        fan_out: 0,
        unresolved: 0,
        expandable: true,
      };
    }),
    edges: edges.map(([s, d, w]) => ({ src: `dir:${s}`, dst: `dir:${d}`, weight: w, confidence: { exact: 0, resolved: w, heuristic: 0 } })),
    externals: [],
    ignored: [],
  };
}

// A small web app with src open, in the shape /api/graph returns.
const webApp = graph(
  [
    ["db", "src/lib", 3],
    ["scripts", "src/domain", 3],
    ["scripts", "src/lib", 2],
    ["src/adapters", "src/lib", 6],
    ["src/app", "src/adapters", 5],
    ["src/app", "src/components", 8],
    ["src/app", "src/domain", 4],
    ["src/app", "src/lib", 129],
    ["src/components", "src/lib", 1],
    ["src/domain", "src/adapters", 1],
    ["src/domain", "src/lib", 9],
    ["tests", "src/adapters", 3],
    ["worker", "src/domain", 1],
    ["worker", "src/lib", 4],
  ],
  [".github", "docker", "docs", "public", "specs"],
);

// A tangle inside the open folder, plus outside folders that depend on it both ways.
const tangle = graph([
  ["lib/core/entry", "lib/core/beta", 5],
  ["lib/core/entry", "lib/core/alpha", 8],
  ["lib/core/beta", "lib/core/alpha", 25],
  ["lib/core/alpha", "lib/core/beta", 15],
  ["lib/core/alpha", "lib/core/gamma", 10],
  ["lib/core/gamma", "lib/core/delta", 4],
  ["lib/core/delta", "lib/core/beta", 2],
  ["lib/core/alpha", "lib/core/epsilon", 3],
  ["lib/core/epsilon", "lib/core/alpha", 2],
  ["lib/core/alpha", "lib/common", 6],
  ["lib/common", "lib/core/gamma", 1],
  ["lib/view", "lib/core/gamma", 3],
  ["lib/core/delta", "lib/view", 1],
  ["lib/checks", "lib/core/alpha", 40],
]);


const rectOf = (layout: PlanLayout, id: string): Rect => (layout.cards.get(id) ?? layout.boxes.get(id))!;

/** Every drawn edge points down (or sideways), except the magenta ones, which point up. */
function checkDirections(plan: Plan, layout: PlanLayout) {
  let up = 0;
  for (const e of plan.edges) {
    const s = center(rectOf(layout, e.src));
    const d = center(rectOf(layout, e.dst));
    if (e.upstream) {
      up += 1;
      expect(s.y, `${e.src} → ${e.dst} should point up`).toBeGreaterThan(d.y);
    } else {
      expect(s.y, `${e.src} → ${e.dst} should point down or sideways`).toBeLessThanOrEqual(d.y + 0.5);
    }
  }
  return up;
}

const inRect = (r: Rect, outer: Rect) => r.x >= outer.x - 0.5 && r.y >= outer.y - 0.5 && r.x + r.w <= outer.x + outer.w + 0.5 && r.y + r.h <= outer.y + outer.h + 0.5;

/** Every card and box sits inside the box of its parent folder. */
function checkNesting(plan: Plan, layout: PlanLayout) {
  for (const n of plan.nodes) if (n.box) expect(inRect(layout.cards.get(n.id)!, layout.boxes.get(n.box)!), n.id).toBe(true);
  for (const b of plan.boxes) if (b.box) expect(inRect(layout.boxes.get(b.id)!, layout.boxes.get(b.box)!), b.id).toBe(true);
}

const index = new MockIndex(saasFixture());
const h = buildHierarchy(index.tree());
const viewGraph = (v: ViewSpec): GraphResponse => {
  const q = graphQuery(v);
  return { generation: 1, ...index.graph(q.expanded, { depth: q.depth, collapsed: q.collapsed }) };
};

describe("plan layout", () => {
  it("draws magenta edges pointing up and every other edge pointing down or sideways", async () => {
    const plan = buildPlan(tangle, { showAllEdges: true });
    expect(plan.boxes.map((b) => b.id)).toEqual(["dir:lib", "dir:lib/core"]);
    const layout = await layoutPlan(elk, plan);
    expect(checkDirections(plan, layout)).toBeGreaterThan(0);
    checkNesting(plan, layout);
  });

  it("keeps the rule and the nesting at every level of the mock repo, card to card and nested", async () => {
    const views: ViewSpec[] = [TOP_LEVEL, atLevel(2), atLevel(3), atLevel("all"), openFolder(h, atLevel(2), "dir:src/lib/db")];
    for (const id of openDirs(h, atLevel(3)).slice(0, 8)) views.push(openFolder(h, TOP_LEVEL, id));
    let up = 0;
    const modes = new Set<string>();
    for (const v of views) {
      for (const cardsModeMax of [40, 0]) {
        const plan = buildPlan(viewGraph(v), { showAllEdges: true, cardsModeMax });
        modes.add(plan.mode);
        const layout = await layoutPlan(elk, plan);
        up += checkDirections(plan, layout);
        checkNesting(plan, layout);
      }
    }
    expect(modes).toEqual(new Set(["cards", "nested"]));
    expect(up).toBeGreaterThan(0);
  }, 120_000);

  it("is deterministic and ignores the order of the API lists", async () => {
    const a = buildPlan(tangle);
    const b = buildPlan({ ...tangle, nodes: [...tangle.nodes].reverse(), edges: [...tangle.edges].reverse() });
    expect(b.key).toBe(a.key);
    const la = await layoutPlan(elk, a);
    const lb = await layoutPlan(elk, b);
    expect([...lb.cards.entries()].sort()).toEqual([...la.cards.entries()].sort());
  });

  it("keeps the layout key when only weights change, so a reindex doesn't move the plan", () => {
    const before = buildPlan(webApp);
    const heavier = { ...webApp, edges: webApp.edges.map((e) => ({ ...e, weight: e.weight + 1 })) };
    expect(buildPlan(heavier).key).toBe(before.key);
    const fewer = { ...webApp, edges: webApp.edges.slice(1) };
    expect(buildPlan(fewer).key).not.toBe(before.key);
  });

  it("changes the layout key when a reindex flips an import, even if the drawn orientation is the same", () => {
    // X: a→b closes the cycle a→b→c→a and is drawn upstream (b above a). Y: b→a is a plain
    // edge with the same orientation, but routes are stored by edge key, so X's layout can't serve Y.
    const x = buildPlan(graph([["a", "b", 1], ["b", "c", 5], ["c", "a", 5]]));
    const y = buildPlan(graph([["b", "a", 1], ["b", "c", 5], ["c", "a", 5]]));
    expect(x.edges.find((e) => e.src === "dir:a")?.upstream).toBe(true);
    expect(y.edges.every((e) => !e.upstream)).toBe(true);
    expect(y.key).not.toBe(x.key);
  });

  it("finds where to fly for a card, an open box, a hidden node in a box, or nothing", async () => {
    const g = graph(webApp.edges.map((e) => [e.src.slice(4), e.dst.slice(4), e.weight]), [".github", "src/unused"]);
    const plan = buildPlan(g);
    const layout = await layoutPlan(elk, plan);
    expect(flyTarget(layout, "dir:src/lib")).toEqual(layout.cards.get("dir:src/lib"));
    expect(flyTarget(layout, "dir:src")).toEqual(layout.boxes.get("dir:src"));
    // Unconnected, so not a card: its container is the open box.
    expect(plan.isolated.map((n) => n.id)).toContain("dir:src/unused");
    expect(flyTarget(layout, "dir:src/unused")).toEqual(layout.boxes.get("dir:src"));
    expect(flyTarget(layout, "file:src/lib/db.ts")).toEqual(layout.cards.get("dir:src/lib"));
    // Not a card, not in a box: nothing to fly to.
    expect(flyTarget(layout, "dir:.github")).toBeNull();
    expect(flyTarget(layout, "repo:")).toBeNull();
  });

  it("draws each open folder as a box around its children", async () => {
    const plan = buildPlan(webApp);
    const layout = await layoutPlan(elk, plan);
    expect(plan.boxes.map((b) => b.id)).toEqual(["dir:src"]);
    const box = layout.boxes.get("dir:src")!;
    for (const n of plan.nodes) expect(inRect(layout.cards.get(n.id)!, box), n.id).toBe(n.box === "dir:src");
  });
});

describe("plan modes", () => {
  it("draws card to card up to the limit, and between the children of each box above it", () => {
    const cards = buildPlan(tangle);
    expect(cards.mode).toBe("cards");
    expect(cards.edges.some((e) => e.src === "dir:lib/checks" && e.dst === "dir:lib/core/alpha")).toBe(true);
    const nested = buildPlan(tangle, { cardsModeMax: 3 });
    expect(nested.mode).toBe("nested");
    // lib/checks → lib/core/alpha is drawn from the card to the lib/core box, inside the lib box.
    const lifted = nested.edges.find((e) => e.src === "dir:lib/checks");
    expect(lifted).toMatchObject({ dst: "dir:lib/core", container: "dir:lib", weight: 40 });
    // Every drawn edge joins two children of the box that routes it.
    const parent = (id: string) => nested.nodes.find((n) => n.id === id)?.box ?? nested.boxes.find((b) => b.id === id)?.box ?? null;
    for (const e of nested.edges) {
      expect(parent(e.src), e.key).toBe(e.container);
      expect(parent(e.dst), e.key).toBe(e.container);
    }
    // The same edges point up in both modes.
    const upCards = new Set(cards.allEdges.filter((e) => e.upstream).map((e) => e.key));
    expect(new Set(nested.allEdges.filter((e) => e.upstream).map((e) => e.key))).toEqual(upCards);
    expect(nested.key).not.toBe(cards.key);
  });

  it("keeps a whole repository opened to its files a few screens big, not dozens", async () => {
    const plan = buildPlan(viewGraph(atLevel("all")));
    expect(plan.mode).toBe("nested");
    const layout = await layoutPlan(elk, plan);
    const cards = plan.nodes.length;
    expect(cards).toBeGreaterThan(80);
    // Under ~30 times the area of the cards themselves.
    expect((layout.width * layout.height) / (cards * plan.card.w * plan.card.h)).toBeLessThan(30);
  }, 60_000);
});

describe("plan density", () => {
  it("hides nothing on a sparse view (the small web app with src open)", () => {
    const plan = buildPlan(webApp);
    expect(plan.hidden).toBe(0);
    const drawn = new Set(plan.edges.map((e) => `${e.src}>${e.dst}`));
    for (const k of ["dir:src/components>dir:src/lib", "dir:src/domain>dir:src/adapters", "dir:worker>dir:src/domain"]) expect(drawn.has(k), k).toBe(true);
    expect(plan.nodes.map((n) => n.id)).toContain("dir:tests");
    expect(plan.isolated.map((n) => n.name).sort()).toEqual([".github", "docker", "docs", "public", "specs"]);
    expect(plan.isolatedCount).toBe(5);
  });

  it("on a dense view hides the lightest edges, never a magenta one, and can show them all", () => {
    const n = 30;
    const edges: [string, string, number][] = [];
    for (let i = 0; i < n; i++) for (let j = i + 1; j < Math.min(n, i + 4); j++) edges.push([`m/n${i}`, `m/n${j}`, 1 + ((i * 7 + j) % 9)]);
    edges.push([`m/n${n - 1}`, "m/n0", 1]); // closes a long cycle with a light edge
    const g = graph(edges);
    const plan = buildPlan(g);
    expect(plan.allEdges.length).toBeGreaterThan(30);
    expect(plan.hidden).toBeGreaterThan(0);
    expect(plan.edges.length + plan.hidden).toBe(plan.allEdges.length);
    expect(plan.edges.length).toBeLessThanOrEqual(30);
    for (const e of plan.allEdges.filter((x) => !plan.edges.includes(x))) {
      expect(e.upstream).toBe(false);
      expect(e.weight).toBeLessThan(plan.minWeight);
    }
    for (const e of plan.edges) expect(e.upstream || e.weight >= plan.minWeight).toBe(true);
    expect(plan.allEdges.some((e) => e.upstream && e.weight === 1)).toBe(true);
    expect(plan.edges.some((e) => e.upstream && e.weight === 1)).toBe(true);
    expect(buildPlan(g, { showAllEdges: true }).hidden).toBe(0);
    // Nested: each box keeps its own budget, never hiding a magenta edge either.
    const nested = buildPlan(g, { cardsModeMax: 0 });
    expect(nested.hidden).toBeGreaterThan(0);
    expect(nested.edges.some((e) => e.upstream && e.weight === 1)).toBe(true);
  });

  it("packs unconnected cards in a grid under the plan, not in one long row", async () => {
    const extra = Array.from({ length: 24 }, (_, i) => `loose${String(i).padStart(2, "0")}`);
    const plan = buildPlan(graph([["a", "b", 3], ["b", "c", 2]], extra), { showIsolated: true });
    const layout = await layoutPlan(elk, plan);
    expect(layout.loose).not.toBeNull();
    const core = ["dir:a", "dir:b", "dir:c"].map((id) => layout.cards.get(id)!);
    const grid = extra.map((p) => layout.cards.get(`dir:${p}`)!);
    for (const r of grid) for (const c of core) expect(r.y).toBeGreaterThan(c.y + c.h);
    const rows = new Set(grid.map((r) => r.y)).size;
    expect(rows).toBeGreaterThan(2);
    expect(layout.width / layout.height).toBeLessThan(3);
    expect(plan.nodes.filter((n) => n.isolated)).toHaveLength(24);
  });

  it("keeps the direction rule when light edges are hidden", async () => {
    const edges: [string, string, number][] = [];
    for (let i = 0; i < 24; i++) for (let j = i + 1; j < Math.min(24, i + 4); j++) edges.push([`m/n${i}`, `m/n${j}`, 1 + ((i * 5 + j) % 7)]);
    edges.push(["m/n20", "m/n3", 2], ["m/n15", "m/n1", 1], ["out", "m/n5", 4], ["m/n9", "out", 1]);
    const plan = buildPlan(graph(edges));
    expect(plan.hidden).toBeGreaterThan(0);
    expect(checkDirections(plan, await layoutPlan(elk, plan))).toBeGreaterThan(0);
  }, 30_000);

  it("never hides a folder because of its name, and counts unconnected ones behind a toggle", () => {
    const g = graph([
      ["test", "src", 40],
      ["tests", "src", 2],
      ["src", "lib", 3],
    ], ["fixtures"]);
    const plan = buildPlan(g);
    expect(plan.nodes.map((n) => n.name).sort()).toEqual(["lib", "src", "test", "tests"]);
    expect(plan.isolated.map((n) => n.name)).toEqual(["fixtures"]);
    const all = buildPlan(g, { showIsolated: true });
    expect(all.nodes.map((n) => n.name)).toContain("fixtures");
    expect(all.nodes.find((n) => n.name === "fixtures")?.isolated).toBe(true);
    expect(all.isolated).toHaveLength(0);
    expect(all.isolatedCount).toBe(1);
  });
});

describe("libraries", () => {
  const withExt: GraphResponse = {
    ...webApp,
    externals: [
      ...["react", "next", "zod", "pg", "kysely", "jose", "postgres", "lucide-react"].map((name, i) => ({ src: "dir:src/app", ext: `ext:npm:${name}`, weight: 10 - i })),
      { src: "dir:src/lib", ext: "ext:npm:kysely", weight: 6 },
      { src: "dir:worker", ext: "ext:node:crypto", weight: 2 },
      { src: "dir:docs", ext: "ext:npm:react", weight: 1 },
    ],
  };

  it("are off by default", () => {
    const plan = buildPlan(withExt);
    expect(plan.nodes.some((n) => n.kind === "ext")).toBe(false);
    expect(plan.edges.some((e) => e.external)).toBe(false);
  });

  it("draw the most used per ecosystem plus one card for the rest, in the bottom layer, with edges from what uses them", async () => {
    const plan = buildPlan(withExt, { showExternals: true });
    const ext = plan.nodes.filter((n) => n.kind === "ext");
    expect(ext.map((n) => n.id).sort()).toEqual(["ext:node:crypto", "ext:npm:kysely", "ext:npm:next", "ext:npm:react", "ext:npm:zod", "extmore:npm"].sort());
    expect(ext.find((n) => n.id === "extmore:npm")).toMatchObject({ name: "otras 4", others: ["pg", "jose", "postgres", "lucide-react"] });
    expect(plan.edges.find((e) => e.src === "dir:src/lib" && e.dst === "ext:npm:kysely")).toMatchObject({ external: true, weight: 6 });
    // docs only uses a library: with libraries on it is connected.
    expect(plan.nodes.some((n) => n.id === "dir:docs")).toBe(true);
    const layout = await layoutPlan(elk, plan);
    const lowest = Math.max(...plan.nodes.filter((n) => n.kind !== "ext").map((n) => layout.cards.get(n.id)!.y));
    for (const n of ext) expect(layout.cards.get(n.id)!.y, n.id).toBeGreaterThan(lowest);
    // Nested: package edges start at the top-level card or box.
    const nested = buildPlan(withExt, { showExternals: true, cardsModeMax: 0 });
    expect(nested.edges.filter((e) => e.external).every((e) => !e.src.includes("/"))).toBe(true);
    expect(nested.edges.find((e) => e.src === "dir:src" && e.dst === "ext:npm:kysely")?.weight).toBe(12);
  });
});
