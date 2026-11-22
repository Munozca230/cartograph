import { afterEach, describe, expect, it } from "vitest";
import type { EntriesResponse, FlowNodeData, FlowResponse, SymbolResponse } from "../src/api/types";
import { layoutFlow } from "../src/model/callflow";
import { FlowsController, type FlowsApi } from "../src/state/flows";
import { initialState, store } from "../src/state/store";
import { closeTopmost } from "../src/ui/DetailPanel";
import { cameraStep, FLOW_K_MIN, FLOW_K_OPEN, fitFlow, keepView, minimapCover, visibleCards, zoomAt } from "../src/ui/flowCamera";

function node(id: string, extra: Partial<FlowNodeData> = {}): FlowNodeData {
  return { id: `sym:src/a.rs#${id}`, name: id, kind: "function", file: "src/a.rs", line: 1, end_line: 2, callers: 1, depth: 0, callees: 0, more: 0, lane: "src", ...extra };
}

function flowOf(nodes: FlowNodeData[], edges: [string, string][], generation = 1): FlowResponse {
  return {
    generation,
    indexed: true,
    root: nodes[0]!.id,
    depth: 4,
    max_nodes: 60,
    nodes,
    edges: edges.map(([s, d]) => ({ src: `sym:src/a.rs#${s}`, dst: `sym:src/a.rs#${d}`, confidence: "resolved", line: 1, calls: 1 })),
    lanes: [{ id: "src", label: "src" }],
    truncated: nodes.some((n) => n.more > 0),
  };
}

const entries: EntriesResponse = {
  generation: 1,
  indexed: true,
  total: 1,
  hidden: 0,
  truncated: false,
  entries: [{ kind: "cli", name: "index", file: "src/main.rs", line: 3, target: null, root: "sym:src/a.rs#main" }],
};

/** A fake API that records its calls; the flow grows when `run` is expanded. */
function fakeApi() {
  const calls: string[] = [];
  let generation = 1;
  const api: FlowsApi = {
    entries: async () => {
      calls.push("entries");
      return { ...entries, generation };
    },
    flow: async (from: string, expand: readonly string[] = []) => {
      calls.push(`flow ${from} [${expand.join(",")}]`);
      const open = expand.includes("sym:src/a.rs#run");
      return flowOf(
        [node("main"), node("run", { more: open ? 0 : 2, depth: 1 }), ...(open ? [node("x", { depth: 2 }), node("y", { depth: 2 })] : [])],
        [["main", "run"], ...(open ? ([["run", "x"], ["run", "y"]] as [string, string][]) : [])],
        generation,
      );
    },
    symbol: async (id: string) => {
      calls.push(`symbol ${id}`);
      return { generation, indexed: true, symbol: { id } } as unknown as SymbolResponse;
    },
    fileFlows: async (id: string) => {
      calls.push(`file-flows ${id}`);
      return { generation, indexed: true, file: id, functions: [], functions_total: 0, entries: [], partial: false };
    },
    searchFunctions: async () => ({ generation, results: [] }),
  } as unknown as FlowsApi;
  return { api, calls, bump: () => (generation += 1) };
}

const settle = () => new Promise((r) => setTimeout(r, 0));

afterEach(() => store.set(initialState));

describe("flows controller", () => {
  it("opens the first entry on the first visit to Flujos", async () => {
    const { api, calls } = fakeApi();
    const f = new FlowsController(api);
    f.setMode("flows");
    await settle();
    await settle();
    const s = store.get();
    expect(s.mode).toBe("flows");
    expect(s.flowFrom).toEqual({ id: "sym:src/a.rs#main", entry: entries.entries[0] });
    expect(s.flowSelected).toBe("sym:src/a.rs#main");
    expect(s.flow?.nodes.map((n) => n.name)).toEqual(["main", "run"]);
    expect(calls).toContain("symbol sym:src/a.rs#main");
  });

  it("a click on a cut card expands it; the view is asked to keep it in place", async () => {
    const { api, calls } = fakeApi();
    const f = new FlowsController(api);
    await f.open("sym:src/a.rs#main");
    f.select("sym:src/a.rs#run");
    await settle();
    await settle();
    expect(store.get().flowExpand).toEqual(["sym:src/a.rs#run"]);
    expect(calls).toContain("flow sym:src/a.rs#main [sym:src/a.rs#run]");
    expect(store.get().flow?.nodes.map((n) => n.name)).toEqual(["main", "run", "x", "y"]);
    expect(f.takePendingAnchor()).toBe("sym:src/a.rs#run");
    expect(f.takePendingAnchor()).toBeNull();
    // Selecting it again does not fetch the same expansion twice.
    const before = calls.length;
    f.expand("sym:src/a.rs#run");
    expect(calls.length).toBe(before);
  });

  it("opening another flow drops the anchor of a card expanded in the last one", async () => {
    const { api } = fakeApi();
    const f = new FlowsController(api);
    await f.open("sym:src/a.rs#main");
    f.expand("sym:src/a.rs#run");
    await f.open("sym:src/a.rs#run");
    expect(f.takePendingAnchor()).toBeNull();
  });

  it("a flow rooted at a file (a scene, a script) loads that file's card", async () => {
    const { api, calls } = fakeApi();
    const scene = node("main.tscn", { id: "file:game/main.tscn", kind: "file", file: "game/main.tscn" });
    api.flow = async () => ({ ...flowOf([scene], []), root: scene.id });
    const f = new FlowsController(api);
    await f.open("file:game/main.tscn");
    await settle();
    expect(calls).toContain("symbol file:game/main.tscn");
    expect(store.get().flowCard?.error).toBeNull();
    expect(store.get().flowCard?.data?.symbol.id).toBe("file:game/main.tscn");
  });

  it("a new generation refetches the flow with the same expansions, the card and the entries", async () => {
    const { api, calls, bump } = fakeApi();
    const f = new FlowsController(api);
    store.set({ graph: { generation: 1 } as never });
    f.watch();
    await f.loadEntries();
    await f.open("sym:src/a.rs#main");
    f.expand("sym:src/a.rs#run");
    await settle();
    calls.length = 0;
    bump();
    store.set({ graph: { generation: 2 } as never });
    await settle();
    await settle();
    expect(calls).toEqual(expect.arrayContaining(["entries", "flow sym:src/a.rs#main [sym:src/a.rs#run]", "symbol sym:src/a.rs#main"]));
    expect(store.get().flow?.generation).toBe(2);
    expect(store.get().flowExpand).toEqual(["sym:src/a.rs#run"]);
  });

  it("drops a selection the new flow no longer has", async () => {
    const { api } = fakeApi();
    const f = new FlowsController(api);
    await f.open("sym:src/a.rs#main");
    store.set({ flowSelected: "sym:src/a.rs#gone" });
    await f.loadFlow();
    expect(store.get().flowSelected).toBeNull();
    expect(store.get().flowCard).toBeNull();
  });

  it("Esc clears the selection in Flujos, and only that", async () => {
    const { api } = fakeApi();
    const f = new FlowsController(api);
    await f.open("sym:src/a.rs#main");
    store.set({ selected: "file:src/a.rs" });
    // The singleton controller is the one Esc talks to: give it the same state.
    store.set({ flowSelected: "sym:src/a.rs#main" });
    expect(closeTopmost()).toBe(true);
    expect(store.get().flowSelected).toBeNull();
    expect(store.get().selected).toBe("file:src/a.rs");
    expect(closeTopmost()).toBe(false);
  });

  it("Esc closes the explorer drawer first in Flujos too", async () => {
    const { api } = fakeApi();
    const f = new FlowsController(api);
    await f.open("sym:src/a.rs#main");
    store.set({ flowSelected: "sym:src/a.rs#main", explorerOpen: true });
    expect(closeTopmost()).toBe(true);
    expect(store.get().explorerOpen).toBe(false);
    expect(store.get().flowSelected).toBe("sym:src/a.rs#main");
    // Then the selection, then nothing.
    expect(closeTopmost()).toBe(true);
    expect(store.get().flowSelected).toBeNull();
    expect(closeTopmost()).toBe(false);
  });

  it("loads a file's flows for the plan and forgets them", async () => {
    const { api, calls } = fakeApi();
    const f = new FlowsController(api);
    await f.loadFileFlows("file:src/a.rs");
    expect(store.get().fileFlows?.data?.file).toBe("file:src/a.rs");
    expect(calls).toEqual(["file-flows file:src/a.rs"]);
    f.closeFileFlows();
    expect(store.get().fileFlows).toBeNull();
  });
});

describe("flow camera", () => {
  /** A chain of `n` functions, each calling the next and `fan` leaves of its own. */
  function chainFlow(n: number, fan: number) {
    const nodes = [node("main")];
    const edges: [string, string][] = [];
    for (let i = 1; i < n; i++) {
      nodes.push(node(`f${i}`));
      edges.push([i === 1 ? "main" : `f${i - 1}`, `f${i}`]);
      for (let j = 0; j < fan; j++) {
        nodes.push(node(`f${i}_leaf${j}`));
        edges.push([`f${i}`, `f${i}_leaf${j}`]);
      }
    }
    return flowOf(nodes, edges);
  }

  it("opens a flow that fits at 12 px names whole, centered, never past 1:1", () => {
    const small = layoutFlow(flowOf([node("main"), node("run")], [["main", "run"]]));
    const v = fitFlow(small, 1000, 600);
    expect(v.k).toBe(1);
    expect(v.tx).toBeCloseTo((1000 - small.width) / 2);
    expect(visibleCards(small, v, 1000, 600)).toBe(2);
  });

  it("opens a bigger flow at 12 px names, on the window with the most whole cards, the start among them", () => {
    const f = chainFlow(9, 4);
    const l = layoutFlow(f);
    const v = fitFlow(l, 966, 619, f.root);
    expect(v.k).toBe(FLOW_K_OPEN);
    const cover = minimapCover(l, 966, 619);
    const best = visibleCards(l, v, 966, 619, cover);
    // The start is whole on the stage, and no shift of the window shows more whole cards.
    const start = { ...l, cards: new Map([[f.root, l.cards.get(f.root)!]]) };
    expect(visibleCards(start, v, 966, 619, cover)).toBe(1);
    for (const dx of [-300, -100, -20, 20, 100, 300])
      for (const dy of [-200, -20, 0, 20, 200]) {
        const u = { ...v, tx: v.tx + dx, ty: v.ty + dy };
        if (visibleCards(start, u, 966, 619, cover) === 1) expect(visibleCards(l, u, 966, 619, cover)).toBeLessThanOrEqual(best);
      }
    // "Encuadrar" goes down to FLOW_K_MIN and shows it all when it fits there.
    const e = fitFlow(l, 966, 619, f.root, FLOW_K_MIN);
    expect(e.k).toBeGreaterThanOrEqual(FLOW_K_MIN);
    expect(e.k).toBeLessThan(FLOW_K_OPEN);
    expect(visibleCards(l, e, 966, 619)).toBe(l.cards.size);
    expect(zoomAt(v, 0.1, 0, 0).k).toBe(FLOW_K_MIN);
  });

  it("ends the window in the gap before a card rather than across it", () => {
    const f = chainFlow(14, 0);
    const l = layoutFlow(f);
    const w = 900;
    const h = 400;
    const v = fitFlow(l, w, h, f.root);
    for (const r of l.cards.values()) {
      const x0 = r.x * v.k + v.tx;
      const x1 = x0 + r.w * v.k;
      // Each card is wholly on the stage or wholly off it.
      expect(x1 <= 0 || x0 >= w || (x0 >= -0.5 && x1 <= w + 0.5)).toBe(true);
    }
  });

  it("frames a flow of 300 cards quickly", () => {
    // Every 15th card calls the next one and 14 leaves, over four lanes; then each card is
    // nudged off the grid, so no two share a left or top edge (the most windows to score).
    const lanes = ["src", "api", "db", "ui"];
    const nodes = Array.from({ length: 300 }, (_, i) => node(i === 0 ? "main" : `f${i}`, { lane: lanes[i % 4]! }));
    const flow = flowOf(nodes, []);
    flow.lanes = lanes.map((id) => ({ id, label: id }));
    flow.edges = nodes.slice(1).map((n, i) => ({ src: nodes[i - (i % 15)]!.id, dst: n.id, confidence: "resolved", line: 1, calls: 1 }));
    const grid = layoutFlow(flow);
    const cards = new Map([...grid.cards].map(([id, r], i) => [id, { ...r, x: r.x + i * 0.37, y: r.y + i * 0.21 }]));
    const l = { ...grid, cards };
    expect(cards.size).toBe(300);
    const run = () => fitFlow(l, 1280, 720, flow.root);
    expect(run().k).toBe(FLOW_K_OPEN);
    let best = Infinity;
    for (let i = 0; i < 5; i++) {
      const t = performance.now();
      run();
      best = Math.min(best, performance.now() - t);
    }
    expect(best).toBeLessThan(50);
  });

  it("only a new layout of the same start keeps a card in place; a resize leaves the view", () => {
    const a = layoutFlow(flowOf([node("main"), node("run")], [["main", "run"]]));
    const b = layoutFlow(flowOf([node("main"), node("run"), node("x")], [["main", "run"], ["run", "x"]]));
    expect(cameraStep(null, { root: "m", layout: a })).toBe("fit");
    expect(cameraStep({ root: "m", layout: a }, { root: "other", layout: b })).toBe("fit");
    expect(cameraStep({ root: "m", layout: a }, { root: "m", layout: b })).toBe("keep");
    expect(cameraStep({ root: "m", layout: a }, { root: "m", layout: a })).toBe("stay");
  });

  it("keeps the expanded card where it was on screen when the flow grows", () => {
    const before = layoutFlow(flowOf([node("main"), node("run", { more: 2 })], [["main", "run"]]));
    const after = layoutFlow(flowOf([node("main"), node("run"), node("x"), node("y"), node("z")], [["main", "run"], ["run", "x"], ["run", "y"], ["run", "z"]]));
    const v = { k: 0.9, tx: 130, ty: 40 };
    const next = keepView(before, after, v, 1000, 600, "sym:src/a.rs#run", null);
    const a = before.cards.get("sym:src/a.rs#run")!;
    const b = after.cards.get("sym:src/a.rs#run")!;
    expect(next.k).toBe(0.9);
    expect(a.x * v.k + v.tx).toBeCloseTo(b.x * next.k + next.tx);
    expect(a.y * v.k + v.ty).toBeCloseTo(b.y * next.k + next.ty);
  });
});
