import { describe, expect, it } from "vitest";
import { BOX_PAD, CARD, CARD_GAP, LOOSE_GAP } from "../src/model/plan";
import { BOX_LABEL_H, CARD_LABEL_H, fitEnd, fitName, keepsExtension, onStage, placeLabels, roughMeasure, stemOf, K_MIN, type LabelCandidate, type PlacedLabel } from "../src/ui/planLabels";

const M = roughMeasure(12);
/** The plan's readability floor (12/14): labels only show below it. */
const FLOOR = 12 / 14;
/** Vertical room between two layers (elk.layered.spacing.nodeNodeBetweenLayers). */
const LAYER_GAP = 46;
const SLOT = CARD.w + CARD_GAP;

const overlap = (a: PlacedLabel, b: PlacedLabel) => a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
const noOverlaps = (out: PlacedLabel[]) => {
  for (let i = 0; i < out.length; i++) for (let j = i + 1; j < out.length; j++) expect([out[i]!.id, out[j]!.id, overlap(out[i]!, out[j]!)]).toEqual([out[i]!.id, out[j]!.id, false]);
};
/** Every box title starts inside its box (or the gap after it), between a line above its top and just under its bottom. */
const attached = (out: PlacedLabel[], cands: LabelCandidate[], k: number) => {
  for (const l of out.filter((x) => x.type === "box")) {
    const b = cands.find((c) => c.type === "box" && c.id === l.id)!.rect;
    const ok = l.x >= b.x * k && l.x <= (b.x + b.w + CARD_GAP) * k && l.y >= b.y * k - BOX_LABEL_H - 3 && l.y <= (b.y + b.h) * k + 2;
    expect([k, l.id, ok]).toEqual([k, l.id, true]);
  }
};
const sweep = (from: number, to: number, step = 0.01) => {
  const ks: number[] = [];
  for (let k = from; k < to; k += step) ks.push(Math.round(k * 1000) / 1000);
  return ks;
};

let prio = 0;
const depthOf = (path: string) => path.split("/").length;
const fileCard = (path: string, x: number, y: number): LabelCandidate => ({
  id: `file:${path}`,
  text: path.slice(path.lastIndexOf("/") + 1),
  type: "card",
  rect: { x, y, w: CARD.w, h: CARD.h },
  priority: 1000 + prio++,
  kind: "code",
  depth: depthOf(path),
  file: true,
});
const dirCard = (path: string, x: number, y: number): LabelCandidate => ({
  id: `dir:${path}`,
  text: path.slice(path.lastIndexOf("/") + 1),
  type: "card",
  rect: { x, y, w: CARD.w, h: CARD.h },
  priority: 1000 + prio++,
  kind: "code",
  depth: depthOf(path),
});

/**
 * An open folder as the plan draws it: a header strip of BOX_PAD.top, then `rows` (each a list
 * of children: "f:name" a file card, "d:name" a closed folder card, or a nested box) laid out
 * like an ELK layer. Returns the candidates and the box's size.
 */
type Child = string | ((x: number, y: number) => { cands: LabelCandidate[]; w: number; h: number });
const openBox =
  (path: string, rows: Child[][]) =>
  (x: number, y: number): { cands: LabelCandidate[]; w: number; h: number } => {
    const cands: LabelCandidate[] = [];
    let cy = y + BOX_PAD.top;
    let w = 0;
    for (const row of rows) {
      let cx = x + BOX_PAD.side;
      let rowH = 0;
      for (const c of row) {
        if (typeof c === "string") {
          const name = c.slice(2);
          cands.push(c.startsWith("d:") ? dirCard(`${path}/${name}`, cx, cy) : fileCard(`${path}/${name}`, cx, cy));
          cx += SLOT;
          rowH = Math.max(rowH, CARD.h);
        } else {
          const b = c(cx, cy);
          cands.push(...b.cands);
          cx += b.w + CARD_GAP;
          rowH = Math.max(rowH, b.h);
        }
      }
      w = Math.max(w, cx - CARD_GAP - x + BOX_PAD.side);
      cy += rowH + LAYER_GAP;
    }
    const h = cy - LAYER_GAP - y + BOX_PAD.bottom;
    cands.push({ id: `dir:${path}`, text: path.slice(path.lastIndexOf("/") + 1), type: "box", rect: { x, y, w, h }, priority: prio++, kind: "test", depth: depthOf(path) });
    return { cands, w, h };
  };

const names = (n: number, prefix: string) => Array.from({ length: n }, (_, i) => `f:${prefix}${["Panel", "Service", "Module", "Overlay", "Controller"][i % 5]}${i}.tsx`);

/** Nested boxes sharing a corner, a long row of sibling files, folder cards and an index.ts/index.js pair. */
const fixture = () =>
  openBox("src", [
    [
      openBox("src/test", [
        [openBox("src/test/unit", [names(8, "ChromeWindow"), ["f:index.ts", "f:index.js", "d:fixtures", "d:helpers"]])],
        [openBox("src/test/e2e", [names(5, "Checkout")]), "d:mocks"],
      ]),
      openBox("src/ui", [["d:components", "d:hooks", "d:pages", "f:App.tsx", "f:main.tsx"]]),
    ],
    names(12, "OrderSummaryView"),
  ])(0, 0).cands;

describe("placeLabels: every card named at every zoom down to K_MIN", () => {
  const cands = fixture();
  const cards = cands.filter((c) => c.type === "card");
  const boxes = cands.filter((c) => c.type === "box");

  it("names every card, in its card and the gap after it, with no overlaps, at every allowed zoom", () => {
    for (const k of sweep(K_MIN, FLOOR)) {
      const out = placeLabels(cands, k, M);
      const byId = new Map(out.map((l) => [`${l.type}|${l.id}`, l]));
      for (const c of cards) {
        const l = byId.get(`card|${c.id}`);
        expect([k, c.id, l !== undefined]).toEqual([k, c.id, true]);
        expect(l!.x).toBeGreaterThanOrEqual(c.rect.x * k);
        // The card's width plus the gap to its neighbour, short of the next name.
        expect(l!.x + l!.w).toBeLessThanOrEqual((c.rect.x + c.rect.w + CARD_GAP) * k);
        expect(M(l!.text) + 8).toBeLessThanOrEqual(l!.w);
        // At least four characters and the ellipsis: still a name.
        expect([k, c.id, l!.text, l!.text.replace("…", "").length >= Math.min(4, stemOf(c.text).length)]).toEqual([k, c.id, l!.text, true]);
      }
      noOverlaps(out);
    }
  });

  it("names every box too from the old tier switch up, and never lets a title cover a card name", () => {
    for (const k of sweep(0.45, FLOOR)) {
      const out = placeLabels(cands, k, M);
      for (const b of boxes) expect([k, b.id, out.some((l) => l.type === "box" && l.id === b.id)]).toEqual([k, b.id, true]);
    }
    for (const k of sweep(K_MIN, FLOOR, 0.05)) {
      const out = placeLabels(cands, k, M);
      const titles = out.filter((l) => l.type === "box");
      const names = out.filter((l) => l.type === "card");
      for (const t of titles) for (const n of names) expect([k, t.id, n.id, overlap(t, n)]).toEqual([k, t.id, n.id, false]);
    }
  });

  it("drops box titles only a whole level at a time, the deepest first", () => {
    const maxDepth = (out: PlacedLabel[]) => Math.max(0, ...out.filter((l) => l.type === "box").map((l) => boxes.find((b) => b.id === l.id)!.depth));
    let last = Infinity;
    for (const k of sweep(K_MIN, FLOOR).reverse()) {
      const out = placeLabels(cands, k, M);
      const depth = maxDepth(out);
      expect(depth).toBeLessThanOrEqual(last);
      last = depth;
      const shown = new Set(out.filter((l) => l.type === "box").map((l) => l.id));
      for (const b of boxes) if (b.depth <= depth) expect([k, b.id, shown.has(b.id)]).toEqual([k, b.id, true]);
    }
    // At K_MIN the nested corners still fit their titles.
    expect(maxDepth(placeLabels(cands, K_MIN, M))).toBe(3);
  });

  it("cuts sibling names in the middle so their ends still tell them apart", () => {
    const out = placeLabels(cands, K_MIN, M);
    const row = out.filter((l) => l.id.startsWith("file:src/OrderSummaryView"));
    expect(row).toHaveLength(12);
    for (const l of row) expect(l.text).toMatch(/^Or.*…/);
    expect(new Set(row.map((l) => l.text)).size).toBe(12);
  });

  it("keeps the extension of index.ts and index.js, whose names would otherwise be the same", () => {
    const pair = (path: string, keepExt: boolean): LabelCandidate => ({ ...fileCard(path, 0, 0), keepExt });
    const a = fitName("index.ts", M("index"), M, { file: true, keepExt: true });
    const b = fitName("index.js", M("index"), M, { file: true, keepExt: true });
    expect(a).not.toBe(b);
    const out = placeLabels([pair("a/index.ts", true), { ...pair("a/index.js", true), id: "x", rect: { x: SLOT, y: 0, w: CARD.w, h: CARD.h } }], K_MIN, M);
    expect(new Set(out.map((l) => l.text)).size).toBe(2);
  });

  it("stacks the titles of nested boxes that share a corner instead of dropping them", () => {
    const out = placeLabels(cands, K_MIN, M).filter((l) => l.type === "box");
    const at = (id: string) => out.find((l) => l.id === id);
    expect(at("dir:src")).toBeDefined();
    expect(at("dir:src/test")).toBeDefined();
    expect(at("dir:src/test")!.y).toBeGreaterThan(at("dir:src")!.y);
  });

  it("keeps the title of a small box alone in the corner of its folder, next to a busy sibling", () => {
    // `.tools` holds only `.tools/hooks` (one file); `scripts`, right beside it, is full of files.
    const a = openBox(".tools", [[openBox(".tools/hooks", [["f:check.mjs"]])]])(0, 0);
    const b = openBox("scripts", [["f:release-notes.mjs", "f:extract-jar.mjs", "f:verify-tree.mjs"]])(a.w + CARD_GAP, 0);
    const cands = [...a.cands, ...b.cands];
    for (const k of sweep(K_MIN, FLOOR, 0.02)) {
      const out = placeLabels(cands, k, M);
      for (const id of ["dir:.tools", "dir:.tools/hooks", "dir:scripts"]) expect([k, id, out.some((l) => l.type === "box" && l.id === id)]).toEqual([k, id, true]);
      noOverlaps(out);
      attached(out, cands, k);
    }
  });

  it("keeps every title by its own box: starting inside its width, from a line above its top to just under it", () => {
    for (const k of sweep(K_MIN, FLOOR, 0.02)) attached(placeLabels(fixture(), k, M), fixture(), k);
  });

  it("leaves room between rows of the loose grid for the names at K_MIN", () => {
    expect((CARD.h + LOOSE_GAP.y) * K_MIN).toBeGreaterThanOrEqual(CARD_LABEL_H + 2);
  });

  it("stops the zoom where a card is 48 px wide", () => {
    expect(K_MIN * CARD.w).toBe(48);
  });
});

describe("placeLabels", () => {
  it("depends on the zoom only: anchors scale with k, offsets stay in px", () => {
    const c = fileCard("x.ts", 200, 100);
    const [a] = placeLabels([c], 0.5, M);
    expect(a!.ax).toBe(100);
    expect(a!.ay).toBe(60);
    expect(a!.x).toBe(a!.ax + a!.dx);
    expect(a!.y).toBe(a!.ay + a!.dy);
    expect(a!.h).toBe(16);
  });
});

describe("keepsExtension", () => {
  it("keeps a file's extension when a sibling file or folder would read the same without it", () => {
    const keep = keepsExtension([
      { id: "file:src/index.ts", name: "index.ts", kind: "file", box: "dir:src" },
      { id: "file:src/index.js", name: "index.js", kind: "file", box: "dir:src" },
      { id: "file:src/utils.ts", name: "utils.ts", kind: "file", box: "dir:src" },
      { id: "dir:src/utils", name: "utils", kind: "dir", box: "dir:src" },
      { id: "file:src/main.ts", name: "main.ts", kind: "file", box: "dir:src" },
      // The same name in another box is no sibling.
      { id: "file:lib/main.js", name: "main.js", kind: "file", box: "dir:lib" },
      // A folder keeps its dots: `com.example` is no `com`.
      { id: "dir:src/com.example", name: "com.example", kind: "dir", box: "dir:src" },
      { id: "file:src/com.ts", name: "com.ts", kind: "file", box: "dir:src" },
    ]);
    expect([...keep].sort()).toEqual(["file:src/index.js", "file:src/index.ts", "file:src/utils.ts"]);
  });

  it("counts open folders (boxes) as siblings too", () => {
    const keep = keepsExtension([{ id: "file:src/utils.ts", name: "utils.ts", kind: "file", box: "dir:src" }], [{ name: "utils", box: "dir:src" }]);
    expect([...keep]).toEqual(["file:src/utils.ts"]);
  });
});

describe("fitting names", () => {
  it("keeps a name that fits", () => {
    expect(fitName("index.ts", 100, M)).toBe("index.ts");
    expect(fitEnd("index.ts", 100, M)).toBe("index.ts");
  });

  it("drops a file's extension first, then cuts in the middle so the end still tells siblings apart", () => {
    const file = { file: true };
    expect(fitName("Toolbar.tsx", M("Toolbar"), M, file)).toBe("Toolbar");
    expect(fitName("cartTotals.test.ts", M("cartTotals"), M, file)).toBe("cartTotals");
    expect(fitName("index.d.ts", M("index"), M, file)).toBe("index");
    expect(fitName("Makefile", M("Make"), M, file)).toMatch(/…/);
    const a = fitName("OrderSummaryViewPanel.tsx", 80, M, file);
    const b = fitName("OrderSummaryViewService.tsx", 80, M, file);
    expect(a).not.toBe(b);
    expect(M(a)).toBeLessThanOrEqual(80);
    expect(a).toMatch(/^Orde.*….*Panel$/);
  });

  it("never strips a folder's dots, and keeps an extension that disambiguates", () => {
    expect(fitName("com.example.app", M("com.example.ap"), M)).toMatch(/^com.*….*app$/);
    expect(fitName("com.example.app", M("com.example.ap"), M)).not.toBe("com.example");
    expect(fitName("index.ts", M("index"), M, { file: true })).toBe("index");
    expect(fitName("index.ts", M("index"), M, { file: true, keepExt: true })).toMatch(/…/);
    expect(fitName("index.ts", M("index"), M, { file: true, keepExt: true })).not.toBe(fitName("index.js", M("index"), M, { file: true, keepExt: true }));
    expect(stemOf("index.test.ts")).toBe("index");
    expect(stemOf(".env")).toBe(".env");
  });

  it("cuts at the end for names inside cards", () => {
    const s = fitEnd("shipping-rates-table.mjs", 100, M);
    expect(s.endsWith("…")).toBe(true);
    expect(M(s)).toBeLessThanOrEqual(100);
    expect("shipping-rates-table.mjs".startsWith(s.slice(0, -1))).toBe(true);
  });
});

describe("onStage", () => {
  const l: PlacedLabel = { id: "a", text: "a", type: "card", kind: "code", x: 100, y: 100, w: 50, h: 16, ax: 100, ay: 108, dx: 0, dy: -8 };
  it("culls labels off the stage", () => {
    expect(onStage(l, 0, 0, 800, 600)).toBe(true);
    expect(onStage(l, -200, 0, 800, 600)).toBe(false);
    expect(onStage(l, 0, 600, 800, 600)).toBe(false);
    expect(onStage(l, -120, 0, 800, 600)).toBe(true);
  });
});
