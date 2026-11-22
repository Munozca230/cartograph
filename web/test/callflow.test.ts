import { describe, expect, it } from "vitest";
import type { FlowEntry, FlowNodeData, FlowResponse } from "../src/api/types";
import {
  alertCount,
  cardMeta,
  cardSizes,
  edgeAlert,
  entryLabel,
  entryTag,
  entryWhere,
  fadeFocus,
  fileLabels,
  FLOW_CARD,
  flowKey,
  flowTitle,
  groupEntries,
  LANE_PAD,
  laneLabel,
  laneOrder,
  laneTitleW,
  layoutFlow,
  metaLine,
  moreCapped,
  moreText,
  ROUGH,
  shortFile,
  splitName,
  SWIMLANES,
  type FlowLayout,
} from "../src/model/callflow";
import { edgeKey } from "../src/model/flow";
import { swimlanes } from "../src/model/swimlanes";
import type { Point, Rect } from "../src/model/plan";
import { fitFlow, FLOW_K_OPEN, visibleCards } from "../src/ui/flowCamera";


/** A node: `lane/file.rs#name`. */
function node(id: string, lane: string, extra: Partial<FlowNodeData> = {}): FlowNodeData {
  const [file, name] = id.split("#") as [string, string];
  return { id: `sym:${id}`, name, kind: "function", file, line: 10, end_line: 20, callers: 1, depth: 0, callees: 0, more: 0, lane, ...extra };
}

function flowOf(nodes: FlowNodeData[], edges: [string, string][], heuristic: string[] = []): FlowResponse {
  const lanes: string[] = [];
  for (const n of nodes) if (!lanes.includes(n.lane)) lanes.push(n.lane);
  return {
    generation: 1,
    indexed: true,
    root: nodes[0]!.id,
    depth: 4,
    max_nodes: 60,
    nodes,
    edges: edges.map(([s, d]) => ({ src: `sym:${s}`, dst: `sym:${d}`, confidence: heuristic.includes(d) ? "heuristic" : "resolved", line: 3, calls: 1 })),
    lanes: lanes.map((id) => ({ id, label: id })),
    truncated: false,
  };
}

const overlap = (a: Rect, b: Rect) => a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;

/** An orthogonal polyline runs through the inside of `r`. */
function crosses(pts: Point[], r: Rect): boolean {
  for (let i = 1; i < pts.length; i++) {
    const a = pts[i - 1]!;
    const b = pts[i]!;
    const x0 = Math.min(a.x, b.x);
    const x1 = Math.max(a.x, b.x);
    const y0 = Math.min(a.y, b.y);
    const y1 = Math.max(a.y, b.y);
    if (x0 < r.x + r.w - 0.5 && x1 > r.x + 0.5 && y0 < r.y + r.h - 0.5 && y1 > r.y + 0.5) return true;
  }
  return false;
}

/** Three crates, with a call back into the first one. */
const sample = flowOf(
  [
    node("crates/cli/src/main.rs#cmd_index", "crates/cli"),
    node("crates/index/src/lib.rs#index_to_store", "crates/index", { callers: 33 }),
    node("crates/index/src/walk.rs#walk", "crates/index"),
    node("crates/index/src/lib.rs#read_files", "crates/index"),
    node("crates/core/src/store.rs#Store.write_graph", "crates/core", { finding: "warning" }),
    node("crates/core/src/model.rs#Graph.default", "crates/core", { more: 3 }),
    node("crates/cli/src/log.rs#log_progress", "crates/cli"),
  ],
  [
    ["crates/cli/src/main.rs#cmd_index", "crates/index/src/lib.rs#index_to_store"],
    ["crates/index/src/lib.rs#index_to_store", "crates/index/src/walk.rs#walk"],
    ["crates/index/src/lib.rs#index_to_store", "crates/index/src/lib.rs#read_files"],
    ["crates/index/src/lib.rs#index_to_store", "crates/core/src/store.rs#Store.write_graph"],
    ["crates/index/src/lib.rs#read_files", "crates/core/src/model.rs#Graph.default"],
    // Back into the first lane.
    ["crates/index/src/walk.rs#walk", "crates/cli/src/log.rs#log_progress"],
  ],
  ["crates/core/src/model.rs#Graph.default"],
);

describe("flow layout", () => {
  let layout: FlowLayout;

  it("lays out every card, lane and edge", () => {
    layout = layoutFlow(sample);
    expect(layout.cards.size).toBe(7);
    expect([...layout.lanes.keys()]).toEqual(["crates/cli", "crates/index", "crates/core"]);
    for (const e of sample.edges) expect(layout.routes.get(edgeKey(e.src, e.dst))?.length ?? 0).toBeGreaterThanOrEqual(2);
  });

  it("stacks each lane in a band of its own, top to bottom in flow order, without overlaps", () => {
    const lanes = [...layout.lanes.values()];
    for (let i = 0; i < lanes.length; i++) for (let j = i + 1; j < lanes.length; j++) expect(overlap(lanes[i]!, lanes[j]!)).toBe(false);
    expect(lanes[0]!.y + lanes[0]!.h).toBeLessThanOrEqual(lanes[1]!.y);
    expect(lanes[1]!.y + lanes[1]!.h).toBeLessThanOrEqual(lanes[2]!.y);
    // Every card lies inside its lane, under the lane's title; every lane fits its title.
    for (const n of sample.nodes) {
      const r = layout.cards.get(n.id)!;
      const l = layout.lanes.get(n.lane)!;
      expect(r.x).toBeGreaterThanOrEqual(l.x);
      expect(r.x + r.w).toBeLessThanOrEqual(l.x + l.w);
      expect(r.y).toBeGreaterThanOrEqual(l.y + LANE_PAD.top);
      expect(r.y + r.h).toBeLessThanOrEqual(l.y + l.h);
    }
    for (const [lane, r] of layout.lanes) expect(r.w).toBeGreaterThanOrEqual(laneTitleW(lane));
  });

  it("shares its columns between lanes: a call into another lane goes to the next column", () => {
    const start = layout.cards.get("sym:crates/cli/src/main.rs#cmd_index")!;
    const callee = layout.cards.get("sym:crates/index/src/lib.rs#index_to_store")!;
    const next = layout.cards.get("sym:crates/index/src/walk.rs#walk")!;
    expect(callee.x).toBeGreaterThan(start.x + start.w);
    expect(callee.x).toBeLessThan(next.x);
    // A lane is as wide as its cards, not the whole drawing.
    const cli = layout.lanes.get("crates/cli")!;
    expect(cli.w).toBeLessThan(layout.width);
  });

  it("routes every call from the right side of its caller to the left side of its callee, around the cards", () => {
    for (const e of sample.edges) {
      const pts = layout.routes.get(edgeKey(e.src, e.dst))!;
      const s = layout.cards.get(e.src)!;
      const d = layout.cards.get(e.dst)!;
      const a = pts[0]!;
      const b = pts[pts.length - 1]!;
      expect(a.x).toBeCloseTo(s.x + s.w);
      expect(a.y).toBeGreaterThanOrEqual(s.y);
      expect(a.y).toBeLessThanOrEqual(s.y + s.h);
      expect(b.x).toBeCloseTo(d.x);
      expect(b.y).toBeGreaterThanOrEqual(d.y);
      expect(b.y).toBeLessThanOrEqual(d.y + d.h);
      for (let i = 1; i < pts.length; i++) expect(pts[i]!.x === pts[i - 1]!.x || pts[i]!.y === pts[i - 1]!.y).toBe(true);
      for (const [id, r] of layout.cards) if (id !== e.src && id !== e.dst) expect(crosses(pts, r)).toBe(false);
    }
  });

  it("never overlaps two cards", () => {
    const rs = [...layout.cards.values()];
    for (let i = 0; i < rs.length; i++) for (let j = i + 1; j < rs.length; j++) expect(overlap(rs[i]!, rs[j]!)).toBe(false);
  });

  it("keeps every card under its lane's title when passing edges are re-sorted", () => {
    // Re-sorting the passing edges of a column can make it taller than the lane's tallest
    // column was: it grows the lane, it never climbs above it.
    const hs = [58, 30, 58, 44, 58, 44, 44, 30, 44, 44, 58, 58];
    const nodes = hs.map((h, i) => ({ id: `n${i}`, lane: "l0", w: 120, h }));
    const calls = "0-1 1-2 1-3 8-5 0-4 5-6 0-5 7-3 4-3 6-7 4-8 9-7 1-9 9-3 5-10 2-10 7-11".split(" ");
    const edges = calls.map((c) => {
      const [a, b] = c.split("-");
      return { id: c, src: `n${a}`, dst: `n${b}` };
    });
    const out = swimlanes(nodes, edges, "n0", ["l0"], SWIMLANES);
    const lane = out.lanes.get("l0")!;
    for (const n of nodes) {
      const r = out.cards.get(n.id)!;
      expect(r.y, n.id).toBeGreaterThanOrEqual(lane.y + SWIMLANES.lanePad.top);
      expect(r.y + r.h, n.id).toBeLessThanOrEqual(lane.y + lane.h);
    }
  });

  it("draws the start on the left", () => {
    const start = layout.cards.get("sym:crates/cli/src/main.rs#cmd_index")!;
    for (const [id, r] of layout.cards) if (id !== "sym:crates/cli/src/main.rs#cmd_index" && !id.includes("log_progress")) expect(r.x).toBeGreaterThan(start.x);
  });

  it("is deterministic and keyed by cards, widths and edges", () => {
    const again = layoutFlow(sample);
    expect(again.key).toBe(layout.key);
    expect([...again.cards.entries()]).toEqual([...layout.cards.entries()]);
    const renamed = { ...sample, nodes: sample.nodes.map((n, i) => (i === 1 ? { ...n, callers: 34 } : n)) };
    // One more caller does not change the width here: same key.
    expect(flowKey(renamed, cardSizes(renamed))).toBe(layout.key);
  });

  it("an empty flow has an empty layout", () => {
    const empty = { ...sample, nodes: [], edges: [], lanes: [] };
    const l = layoutFlow(empty);
    expect(l.cards.size).toBe(0);
  });
});

/** The shape of a real 30-function command flow over three lanes (names replaced). */
const SHAPE_NODES: [string, number][] = [["a",9],["b",20],["b",14],["c",10],["b",6],["a",8],["b",12],["b",8],["b",10],["b",9],["b",11],["b",18],["b",17],["b",9],["b",4],["b",12],["b",12],["b",6],["b",11],["b",4],["b",6],["b",15],["c",10],["c",10],["b",19],["b",4],["b",8],["b",9],["b",12],["b",11]];
const SHAPE_EDGES: [number, number][] = [[0,1],[0,2],[0,3],[0,4],[0,5],[2,6],[2,7],[2,3],[2,8],[2,9],[2,10],[2,11],[2,12],[2,13],[3,22],[3,23],[4,14],[4,15],[4,16],[4,17],[4,18],[4,19],[4,20],[4,21],[8,12],[8,27],[9,13],[10,6],[10,24],[10,11],[10,25],[10,26],[10,27],[10,28],[10,29],[15,17],[25,11]];

/** How much longer an orthogonal route is than the straight L between its ends. */
function detour(pts: { x: number; y: number }[]): number {
  let len = 0;
  for (let i = 1; i < pts.length; i++) len += Math.abs(pts[i]!.x - pts[i - 1]!.x) + Math.abs(pts[i]!.y - pts[i - 1]!.y);
  const a = pts[0]!;
  const b = pts[pts.length - 1]!;
  return len - Math.abs(a.x - b.x) - Math.abs(a.y - b.y);
}

/** The real-shaped command flow: 30 functions over three lanes. */
function shapeFlow(): FlowResponse {
  const id = (i: number) => `${SHAPE_NODES[i]![0]}/src/m.rs#f${i}`;
  return flowOf(
    SHAPE_NODES.map(([lane, len], i) => node(`${lane}/src/m.rs#${`f${i}`.padEnd(len, "x")}`, lane)).map((n, i) => ({ ...n, id: `sym:${id(i)}` })),
    SHAPE_EDGES.map(([a, b]) => [id(a), id(b)] as [string, string]),
  );
}

describe("edge routing", () => {
  it("routes calls short: no edge detours around the flow", () => {
    const l = layoutFlow(shapeFlow());
    const worst = Math.max(...[...l.routes.values()].map(detour));
    // Before: the call from the start to the last lane ran along the whole bottom (+366 px).
    expect(worst).toBeLessThan(120);
  });

  it("draws a call back into a caller (a cycle) from its card back to the one it calls", () => {
    const f = flowOf(
      [node("src/a.rs#main", "src"), node("src/a.rs#walk", "src"), node("src/a.rs#visit", "src")],
      [
        ["src/a.rs#main", "src/a.rs#walk"],
        ["src/a.rs#walk", "src/a.rs#visit"],
        ["src/a.rs#visit", "src/a.rs#walk"],
      ],
    );
    const l = layoutFlow(f);
    const pts = l.routes.get(edgeKey("sym:src/a.rs#visit", "sym:src/a.rs#walk"))!;
    const walk = l.cards.get("sym:src/a.rs#walk")!;
    const visit = l.cards.get("sym:src/a.rs#visit")!;
    expect(visit.x).toBeGreaterThan(walk.x);
    // It leaves `visit` on its left and arrives on the right of `walk`, beside its own calls.
    expect(pts[0]!.x).toBeCloseTo(visit.x);
    expect(pts[pts.length - 1]!.x).toBeCloseTo(walk.x + walk.w);
  });
});

describe("compact flows", () => {
  it("a 30-function command flow opens whole on a laptop's stage with 12 px names", () => {
    const f = shapeFlow();
    const l = layoutFlow(f);
    const v = fitFlow(l, 966, 619, f.root);
    expect(v.k).toBeGreaterThanOrEqual(FLOW_K_OPEN);
    expect(visibleCards(l, v, 966, 619)).toBe(30);
  });

  it("moves leaves out of a column taller than the stage's shape allows, keeping every call left to right", () => {
    // One function calling 16 leaves, and a chain beside it that makes room further right.
    const nodes = [node("src/a.rs#main", "src"), ...Array.from({ length: 16 }, (_, i) => node(`src/a.rs#leaf${i}`, "src"))];
    const edges: [string, string][] = Array.from({ length: 16 }, (_, i) => ["src/a.rs#main", `src/a.rs#leaf${i}`]);
    for (const c of ["c1", "c2", "c3"]) nodes.push(node(`src/a.rs#${c}`, "src"));
    edges.push(["src/a.rs#main", "src/a.rs#c1"], ["src/a.rs#c1", "src/a.rs#c2"], ["src/a.rs#c2", "src/a.rs#c3"]);
    const f = flowOf(nodes, edges);
    const l = layoutFlow(f);
    const columns = new Set([...l.cards.values()].map((r) => r.x));
    expect(columns.size).toBe(4);
    const tallest = Math.max(...[...columns].map((x) => [...l.cards.values()].filter((r) => r.x === x).length));
    expect(tallest).toBeLessThan(10);
    for (const e of f.edges) expect(l.cards.get(e.dst)!.x).toBeGreaterThan(l.cards.get(e.src)!.x);
  });

  /** 300 functions over seven lanes, with calls across them and past several columns. */
  function bigFlow(): FlowResponse {
    const nodes = [node("src/a.rs#main", "src")];
    const edges: [string, string][] = [];
    for (let i = 1; i < 300; i++) {
      nodes.push(node(`p${i % 7}/a.rs#f${i}`, `p${i % 7}`));
      const parent = i < 8 ? "src/a.rs#main" : `p${Math.floor(i / 6) % 7}/a.rs#f${Math.floor(i / 6)}`;
      edges.push([parent, `p${i % 7}/a.rs#f${i}`]);
      if (i % 5 === 0 && i > 20) edges.push([`p${(i - 13) % 7}/a.rs#f${i - 13}`, `p${i % 7}/a.rs#f${i}`]);
    }
    return flowOf(nodes, edges);
  }

  it("lays out 300 functions quickly, without overlaps", () => {
    const f = bigFlow();
    const t0 = performance.now();
    const l = layoutFlow(f);
    fitFlow(l, 966, 619, f.root);
    expect(performance.now() - t0).toBeLessThan(1500);
    expect(l.cards.size).toBe(300);
    const rs = [...l.cards.values()];
    for (let i = 0; i < rs.length; i++) for (let j = i + 1; j < rs.length; j++) expect(overlap(rs[i]!, rs[j]!)).toBe(false);
  });

  it("never runs two unrelated calls along each other, nor a call through a card", () => {
    for (const f of [shapeFlow(), bigFlow(), sample]) {
      const l = layoutFlow(f);
      // Level stretches of two calls that share neither end would read as one call.
      const level: { s: string; d: string; y: number; x0: number; x1: number }[] = [];
      for (const e of f.edges) {
        const pts = l.routes.get(edgeKey(e.src, e.dst))!;
        for (let i = 1; i < pts.length; i++)
          if (pts[i]!.y === pts[i - 1]!.y) level.push({ s: e.src, d: e.dst, y: pts[i]!.y, x0: Math.min(pts[i]!.x, pts[i - 1]!.x), x1: Math.max(pts[i]!.x, pts[i - 1]!.x) });
        for (const [id, r] of l.cards) if (id !== e.src && id !== e.dst) expect(crosses(pts, r)).toBe(false);
      }
      for (let i = 0; i < level.length; i++)
        for (let j = i + 1; j < level.length; j++) {
          const a = level[i]!;
          const b = level[j]!;
          if (a.s === b.s || a.d === b.d || Math.abs(a.y - b.y) > 1) continue;
          expect(Math.min(a.x1, b.x1) - Math.max(a.x0, b.x0)).toBeLessThanOrEqual(3);
        }
    }
  });
});

describe("card sizes", () => {
  it("fit the whole name and the meta line with its pill: names are never cut", () => {
    const long = node("src/a.rs#a_really_long_function_name_that_needs_room_to_breathe", "src", { more: 12 });
    const f = flowOf([long], []);
    const w = cardSizes(f).get(long.id)!.w;
    expect(w).toBeGreaterThanOrEqual(ROUGH.name(long.name) + 28);
    expect(w).toBeGreaterThanOrEqual(ROUGH.meta(metaLine(long)) + ROUGH.meta("+12") + 28);
  });

  it("are at least the minimum width, however short the name", () => {
    const n = node("crates/a-very-long-crate-name-for-a-lane/src/x.rs#f", "crates/a-very-long-crate-name-for-a-lane");
    expect(cardSizes(flowOf([n], [])).get(n.id)!.w).toBeGreaterThanOrEqual(FLOW_CARD.minW);
  });

  it("put a long qualified name on two lines, whole, when the card gets much narrower", () => {
    const n = node("src/Backend.ts#BackendServiceManager.resolveEngineWorkingDirectory", "src", { callers: 3 });
    const one = ROUGH.name(n.name) + 28;
    const size = cardSizes(flowOf([n], [])).get(n.id)!;
    expect(size.h).toBe(FLOW_CARD.h + FLOW_CARD.line);
    expect(size.w).toBeLessThan(one * 0.8);
    expect(splitName(n.name)).toEqual(["BackendServiceManager.", "resolveEngineWorkingDirectory"]);
    expect(splitName("plain_name")).toBeNull();
    expect(splitName(".hidden")).toBeNull();
    // Short ones stay on one line.
    const short = node("src/a.rs#Store.open", "src");
    expect(cardSizes(flowOf([short], [])).get(short.id)!.h).toBe(FLOW_CARD.h);
  });

  it("name each file by the shortest end of its path that tells it apart in its lane", () => {
    const f = flowOf(
      [
        node("crates/i/src/git/mod.rs#update", "crates/i"),
        node("crates/i/src/imports/mod.rs#extract", "crates/i"),
        node("crates/i/src/lib.rs#index", "crates/i"),
        node("crates/c/src/lib.rs#open", "crates/c"),
      ],
      [],
    );
    const labels = fileLabels(f);
    expect(labels.get("crates/i/src/git/mod.rs")).toBe("git/mod.rs");
    expect(labels.get("crates/i/src/imports/mod.rs")).toBe("imports/mod.rs");
    // `lib.rs` is in both lanes, but alone in each: the lane's title tells them apart.
    expect(labels.get("crates/i/src/lib.rs")).toBe("lib.rs");
    expect(labels.get("crates/c/src/lib.rs")).toBe("lib.rs");
    expect(cardMeta(f.nodes[0]!, labels)).toBe("git/mod.rs:10 · ←1");
  });
});

describe("fading", () => {
  it("fades around a selected function, never around the start every flow opens with", () => {
    expect(fadeFocus(sample, null)).toBeNull();
    expect(fadeFocus(sample, sample.root)).toBeNull();
    expect(fadeFocus(sample, "sym:crates/index/src/walk.rs#walk")).toBe("sym:crates/index/src/walk.rs#walk");
  });
});

describe("flow helpers", () => {
  it("names lanes and files the way the cards show them", () => {
    expect(laneLabel("crates/cartograph-cli")).toBe("cartograph-cli");
    expect(laneLabel("src/ui")).toBe("src/ui");
    expect(laneLabel("")).toBe("raíz");
    expect(shortFile("crates/cli/src/main.rs", "crates/cli")).toBe("src/main.rs");
    expect(shortFile("main.rs", "")).toBe("main.rs");
    expect(metaLine(node("crates/cli/src/main.rs#f", "crates/cli", { line: 262, callers: 1 }))).toBe("src/main.rs:262 · ←1");
    expect(metaLine(node("a.py#f", "", { callers: 0, kind: "file" }))).toBe("a.py");
    expect(moreText(node("a#f", "", { more: 3 }))).toBe("+3");
    expect(moreText(node("a#f", ""))).toBeNull();
    // Opened and still cut: the cap, not a cut a click opens.
    expect(moreCapped(node("a#f", "", { more: 3 }))).toBe(false);
    expect(moreCapped(node("a#f", "", { more: 3, open: true }))).toBe(true);
    expect(moreCapped(node("a#f", "", { more: 0, open: true }))).toBe(false);
  });

  it("orders lanes as the flow reaches them", () => {
    expect(laneOrder(sample)).toEqual(["crates/cli", "crates/index", "crates/core"]);
  });

  it("marks calls into files with findings", () => {
    const into = sample.edges.find((e) => e.dst.includes("write_graph"))!;
    expect(edgeAlert(sample, into)).toBe(true);
    expect(edgeAlert(sample, sample.edges[0]!)).toBe(false);
    expect(alertCount(sample)).toBe(1);
  });

  const entry = (kind: FlowEntry["kind"], name: string, file = "src/main.rs", line = 37): FlowEntry => ({ kind, name, file, line, target: null, root: "sym:x#y" });

  it("groups entries by kind and filters them", () => {
    const list = [entry("http", "GET /api/tree", "src/api.rs"), entry("cli", "index"), entry("main", "main"), entry("ipc", "doc:save", "src/main.ts"), entry("godot_main_scene", "main.tscn", "main.tscn", 0)];
    expect(groupEntries(list).map((g) => g.title)).toEqual(["Comandos", "HTTP", "main", "IPC", "Godot"]);
    expect(groupEntries(list, "api").map((g) => [g.title, g.items.length])).toEqual([["HTTP", 1]]);
    expect(groupEntries(list, "nothing")).toEqual([]);
  });

  it("labels entries like the mockup", () => {
    expect([entryTag(entry("http", "POST /x")), entryLabel(entry("http", "POST /x"))]).toEqual(["POST", "/x"]);
    expect([entryTag(entry("cli", "index")), entryLabel(entry("cli", "index"))]).toEqual(["CLI", "index"]);
    expect(entryLabel(entry("main", "main", "crates/cli/src/main.rs"))).toBe("src/main.rs");
    expect(entryTag(entry("godot_main_scene", "main.tscn"))).toBe("escena");
    expect([entryTag(entry("ipc", "doc:save")), entryLabel(entry("ipc", "doc:save"))]).toEqual(["IPC", "doc:save"]);
    expect(entryWhere(entry("cli", "index", "crates/cli/src/main.rs", 38))).toBe("main.rs:38");
    expect(entryWhere(entry("main", "x", "a/b.tscn", 0))).toBe("b.tscn");
    expect(flowTitle(entry("cli", "index"), "cartograph", "f")).toBe("cartograph index");
    expect(flowTitle(entry("http", "GET /x"), "r", "f")).toBe("GET /x");
    expect(flowTitle(null, "r", "f")).toBe("f");
  });
});
