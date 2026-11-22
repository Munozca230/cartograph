// @vitest-environment jsdom
import ELK from "elkjs/lib/elk.bundled.js";
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeAll, describe, expect, it, onTestFinished, vi } from "vitest";
import { saasFixture } from "../mock/fixtures/saas";
import { MockIndex } from "../mock/model";
import type { GraphResponse } from "../src/api/types";
import { buildPlan, flyTarget, layoutPlan, type Plan, type PlanLayout, type Rect } from "../src/model/plan";
import { atLevel, graphQuery, openFolder, TOP_LEVEL, type ViewSpec } from "../src/model/view";
import { anchorOf } from "../src/ui/camera";
import { K_MIN } from "../src/ui/planLabels";
import { buildHierarchy } from "../src/model/hierarchy";
import { controller } from "../src/state/controller";
import { initialState, store } from "../src/state/store";

/** Layouts the test hands out by hand, in the order PlanView asks for them. */
const pending: { plan: Plan; resolve: (l: PlanLayout) => void }[] = [];

vi.mock("../src/state/plan", async (importOriginal) => {
  const real = await importOriginal<typeof import("../src/state/plan")>();
  return {
    ...real,
    layoutFor: (plan: Plan) => new Promise<PlanLayout>((resolve) => pending.push({ plan, resolve })),
  };
});

const { PlanView, fitView, kMinFor } = await import("../src/ui/PlanView");

const elk = new ELK();
const index = new MockIndex(saasFixture());
const h = buildHierarchy(index.tree());
const viewGraph = (v: ViewSpec): GraphResponse => {
  const q = graphQuery(v);
  return { generation: 1, ...index.graph(q.expanded, { depth: q.depth, collapsed: q.collapsed }) };
};

beforeAll(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

afterEach(() => {
  pending.length = 0;
  store.set(initialState);
});

const busy = (el: HTMLElement) => el.querySelector(".stage-busy") !== null;

describe("zoom limits", () => {
  const huge = { key: "h", cards: new Map(), boxes: new Map(), width: 20000, height: 12000 } as unknown as PlanLayout;
  const small = { key: "s", cards: new Map(), boxes: new Map(), width: 900, height: 500 } as unknown as PlanLayout;

  it("fits a plan too big for the stage at K_MIN, its top-left corner in view", () => {
    const v = fitView(huge, 1200, 700);
    expect(v.k).toBe(K_MIN);
    // The plan's corner sits a small margin in from the stage's corner.
    expect(v.tx).toBeGreaterThan(0);
    expect(v.tx).toBeLessThan(40);
    expect(v.ty).toBeGreaterThan(0);
    expect(v.ty).toBeLessThan(40);
  });

  it("fits a plan whole when it fits at K_MIN or more", () => {
    const v = fitView({ ...huge, width: 3000, height: 1500 } as PlanLayout, 1200, 700);
    expect(v.k).toBeGreaterThanOrEqual(K_MIN);
    expect((3000 + 44) * v.k).toBeLessThanOrEqual(1200);
  });

  it("never fits under the zoom limit of the same stage, so − after Encuadrar never zooms in", () => {
    for (const plan of [huge, small, { ...small, width: 2600, height: 1400 } as PlanLayout])
      for (const w of [300, 420, 700, 900, 1200, 1534])
        for (const h of [250, 500, 711]) expect([plan.width, w, h, fitView(plan, w, h).k >= kMinFor(plan, w, h) - 1e-9]).toEqual([plan.width, w, h, true]);
  });

  it("never lets the zoom go under K_MIN, and a small plan stops near its whole view", () => {
    expect(kMinFor(huge, 1200, 700)).toBe(K_MIN);
    expect(kMinFor(small, 1200, 700)).toBeGreaterThan(K_MIN);
  });
});

describe("PlanView", () => {
  it("drops the drawing banner when the plan goes back to the one on screen mid-layout", async () => {
    const a = viewGraph(TOP_LEVEL);
    const b = viewGraph(openFolder(h, TOP_LEVEL, "dir:src"));
    // A re-index that only changes numbers: same layout key as `a`, new plan.
    const a2: GraphResponse = { ...a, generation: 2, edges: a.edges.map((e) => ({ ...e, weight: e.weight + 1 })) };
    expect(buildPlan(a2).key).toBe(buildPlan(a).key);
    expect(buildPlan(b).key).not.toBe(buildPlan(a).key);

    const el = document.createElement("div");
    document.body.append(el);
    const root = createRoot(el);
    await act(async () => {
      store.set({ phase: "ready", graph: a });
      root.render(createElement(PlanView));
    });
    expect(pending).toHaveLength(1);
    const layoutA = await layoutPlan(elk, pending[0]!.plan);
    await act(async () => pending[0]!.resolve(layoutA));
    expect(busy(el)).toBe(false);

    // Another view starts laying out…
    await act(async () => store.set({ graph: b }));
    expect(pending).toHaveLength(2);
    expect(busy(el)).toBe(true);
    // …and the plan goes back to the drawn layout before it ends.
    await act(async () => store.set({ graph: a2 }));
    expect(busy(el)).toBe(false);
    // The dropped layout arriving late changes nothing.
    const layoutB = await layoutPlan(elk, pending[1]!.plan);
    await act(async () => pending[1]!.resolve(layoutB));
    expect(busy(el)).toBe(false);
    await act(async () => root.unmount());
    el.remove();
  });

  it("draws what each card is, and names that stay when zoomed out past the floor", async () => {
    // A small stage: the plan is bigger than it even at K_MIN.
    const cw = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientWidth");
    const ch = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientHeight");
    Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, get: () => 400 });
    Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get: () => 250 });
    onTestFinished(() => {
      if (cw) Object.defineProperty(HTMLElement.prototype, "clientWidth", cw);
      if (ch) Object.defineProperty(HTMLElement.prototype, "clientHeight", ch);
    });
    const g = viewGraph(openFolder(h, TOP_LEVEL, "dir:src"));
    const el = document.createElement("div");
    document.body.append(el);
    const root = createRoot(el);
    await act(async () => {
      store.set({ phase: "ready", graph: g, hierarchy: h });
      root.render(createElement(PlanView));
    });
    const layout = await layoutPlan(elk, pending[0]!.plan);
    await act(async () => pending[0]!.resolve(layout));

    const cards = [...el.querySelectorAll<SVGGElement>(".card:not(.ext)")];
    expect(cards.length).toBeGreaterThan(0);
    for (const c of cards) {
      expect(c.getAttribute("class")).toMatch(/\bk-(code|test|script|style|data)\b/);
      expect(c.querySelector(".stripe")).not.toBeNull();
      expect(c.querySelector(".glyph")).not.toBeNull();
      // Folders keep a folder shape: a tab.
      expect(c.querySelector(".tab") !== null).toBe(c.dataset.id!.startsWith("dir:"));
    }
    expect(el.querySelector(".ctr")!.getAttribute("class")).toMatch(/\bk-\w+/);
    // At 1:1 the plan's own text shows and there are no labels.
    expect(el.querySelector(".labels")).toBeNull();

    // Zoomed out past the floor: the names come back as labels that never overlap. However far
    // one zooms out, the plan stops at K_MIN, where every card keeps its name.
    await act(async () => {
      for (let i = 0; i < 6; i++) controller.zoom(0.2);
    });
    expect(Number(el.querySelector<SVGElement>("svg.dia")!.dataset.k)).toBeGreaterThanOrEqual(K_MIN);
    // Labels are placed in the next animation frame.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 100));
    });
    expect(el.querySelector("svg.dia")!.classList.contains("tiny")).toBe(true);
    const labels = [...el.querySelectorAll<HTMLElement>(".labels .lbl")].map((l) => ({
      text: l.textContent,
      x: parseFloat(l.style.left),
      y: parseFloat(l.style.top),
      w: parseFloat(l.style.width),
      h: 16,
    }));
    expect(labels.length).toBeGreaterThan(0);
    expect(labels.some((l) => l.text === "src")).toBe(true);
    // At the smallest zoom allowed, every card on the stage has its name.
    const [tx, ty, k] = el.querySelector("svg.dia > g")!.getAttribute("transform")!.match(/-?[\d.]+(e-?\d+)?/g)!.map(Number) as [number, number, number];
    expect(k).toBeCloseTo(kMinFor(layout, 400, 250), 6);
    expect(k).toBeGreaterThanOrEqual(K_MIN);
    const named = new Set([...el.querySelectorAll<HTMLElement>(".labels .lbl.card")].map((l) => l.dataset.id));
    let onStage = 0;
    for (const [id, r] of layout.cards) {
      const x = r.x * k + tx;
      const y = r.y * k + ty;
      if (x + r.w * k < 0 || y + r.h * k < 0 || x > 400 || y > 250) continue;
      onStage++;
      expect([id, named.has(id)]).toEqual([id, true]);
    }
    expect(onStage).toBeGreaterThan(3);
    for (let i = 0; i < labels.length; i++)
      for (let j = i + 1; j < labels.length; j++) {
        const a = labels[i]!;
        const b = labels[j]!;
        expect(a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h).toBe(false);
      }
    await act(async () => root.unmount());
    el.remove();
  });

  it("keeps the zoom and the anchor where it is on screen when a folder opens, the level changes or a layer toggles", async () => {
    // jsdom has no layout: give the stage a size, or the camera never runs.
    const W = 1200;
    const H = 700;
    const cw = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientWidth");
    const ch = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientHeight");
    Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, get: () => W });
    Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get: () => H });
    const anchorSpy = vi.spyOn(controller, "takePendingAnchor");
    try {
      const el = document.createElement("div");
      document.body.append(el);
      const root = createRoot(el);
      const readView = () => {
        const g = el.querySelector("svg.dia > g")!.getAttribute("transform")!;
        const [tx, ty, k] = g.match(/-?[\d.]+(e-?\d+)?/g)!.map(Number) as [number, number, number];
        return { k, tx, ty };
      };
      const at = (r: Rect, v: { k: number; tx: number; ty: number }) => [r.x * v.k + v.tx, r.y * v.k + v.ty] as const;
      const expectSame = (a: readonly [number, number], b: readonly [number, number]) => {
        expect(Math.abs(a[0] - b[0])).toBeLessThanOrEqual(1);
        expect(Math.abs(a[1] - b[1])).toBeLessThanOrEqual(1);
      };
      const draw = async (graph: GraphResponse, view: ViewSpec, extra: object = {}) => {
        const n = pending.length;
        await act(async () => store.set({ graph, graphView: view, ...extra }));
        const layout = await layoutPlan(elk, pending[n]!.plan);
        await act(async () => pending[n]!.resolve(layout));
        return layout;
      };

      // First drawing: fitted.
      await act(async () => {
        store.set({ phase: "ready", hierarchy: h });
        root.render(createElement(PlanView));
      });
      const layoutA = await draw(viewGraph(TOP_LEVEL), TOP_LEVEL);
      await act(async () => controller.zoom(0.8));
      const v1 = readView();

      // Open src (the controller names it as the anchor): its card becomes a box in the same spot.
      const viewB = openFolder(h, TOP_LEVEL, "dir:src");
      anchorSpy.mockReturnValueOnce("dir:src");
      const layoutB = await draw(viewGraph(viewB), viewB);
      const v2 = readView();
      expect(v2.k).toBe(v1.k);
      expectSame(at(layoutB.boxes.get("dir:src")!, v2), at(layoutA.cards.get("dir:src")!, v1));
      // The drawer's settle timer doesn't put the fitted view back.
      await act(async () => {
        await new Promise((r) => setTimeout(r, 650));
      });
      expect(readView()).toEqual(v2);

      // Another level: the selected card, on screen, stays put.
      const onScreen = (r: Rect, v: { k: number; tx: number; ty: number }) => {
        const [x, y] = at(r, v);
        return x >= 0 && y >= 0 && x + r.w * v.k <= W && y + r.h * v.k <= H;
      };
      const viewC = atLevel(3);
      const graphC = viewGraph(viewC);
      const sel = [...layoutB.cards.keys()].find((id) => onScreen(layoutB.cards.get(id)!, v2) && graphC.nodes.some((n) => n.id === id));
      expect(sel).toBeDefined();
      const layoutC = await draw(graphC, viewC, { selected: sel });
      const v3 = readView();
      expect(v3.k).toBe(v1.k);
      expectSame(at(layoutC.cards.get(sel!)!, v3), at(layoutB.cards.get(sel!)!, v2));

      // A layer toggle (libraries) with nothing selected: the card nearest the center stays put.
      await act(async () => store.set({ selected: null }));
      const centre = anchorOf(layoutC, v3, W, H, null)!;
      const layoutD = await draw(graphC, viewC, { showExternals: true });
      const v4 = readView();
      expect(layoutD.key).not.toBe(layoutC.key);
      expect(v4.k).toBe(v1.k);
      expectSame(at(flyTarget(layoutD, centre)!, v4), at(flyTarget(layoutC, centre)!, v3));

      // With the drawer open, a selected card panned off the stage stays off it across a
      // redraw: the drawer only slides the plan when the selection or the drawer changes.
      const picked = [...layoutD.cards.keys()].find((id) => id.startsWith("file:") && layoutC.cards.has(id))!;
      await act(async () => store.set({ selected: picked, detail: { id: picked, data: null, error: null, loading: false } }));
      await act(async () => {
        await new Promise((r) => setTimeout(r, 650));
      });
      const stage = el.querySelector<HTMLElement>(".stage")!;
      await act(async () => {
        for (let i = 0; i < 40; i++) stage.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
      });
      const v5 = readView();
      expect(onScreen(layoutD.cards.get(picked)!, v5)).toBe(false);
      await draw(graphC, viewC, { showExternals: false });
      const v6 = readView();
      expect(v6.k).toBe(v1.k);
      await act(async () => {
        await new Promise((r) => setTimeout(r, 650));
      });
      expect(readView()).toEqual(v6);

      await act(async () => root.unmount());
      el.remove();
    } finally {
      anchorSpy.mockRestore();
      if (cw) Object.defineProperty(HTMLElement.prototype, "clientWidth", cw);
      if (ch) Object.defineProperty(HTMLElement.prototype, "clientHeight", ch);
    }
  }, 30_000); // several ELK layouts in jsdom: slow on a loaded machine, never wrong
});
