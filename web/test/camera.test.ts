import { describe, expect, it } from "vitest";
import type { PlanLayout, Rect } from "../src/model/plan";
import { anchoredView, anchorOf, flyToward, pickAnchor, type View } from "../src/ui/camera";

const layout = (cards: Record<string, Rect>, boxes: Record<string, Rect> = {}): PlanLayout =>
  ({ key: "k", cards: new Map(Object.entries(cards)), boxes: new Map(Object.entries(boxes)), width: 2000, height: 2000 }) as unknown as PlanLayout;
const card = (x: number, y: number): Rect => ({ x, y, w: 160, h: 40 });
const screen = (r: Rect, v: View) => [r.x * v.k + v.tx, r.y * v.k + v.ty];

describe("anchoredView", () => {
  const v: View = { k: 0.6, tx: 40, ty: -120 };

  it("keeps a folder card that opens into a box at the same screen point and zoom", () => {
    const prev = layout({ "dir:src": card(400, 300), "file:a.ts": card(0, 0) });
    const next = layout({ "file:src/x.ts": card(520, 380), "file:a.ts": card(0, 0) }, { "dir:src": { x: 500, y: 340, w: 600, h: 300 } });
    const out = anchoredView(prev, next, v, "dir:src");
    expect(out.k).toBe(v.k);
    expect(screen(next.boxes.get("dir:src")!, out)).toEqual(screen(prev.cards.get("dir:src")!, v));
  });

  it("puts the folder a file was folded into (another level) where the file was", () => {
    const prev = layout({ "file:src/ui/App.tsx": card(700, 500) }, { "dir:src": { x: 600, y: 400, w: 800, h: 400 }, "dir:src/ui": { x: 650, y: 450, w: 400, h: 200 } });
    const next = layout({ "dir:src/ui": card(300, 200) }, { "dir:src": { x: 250, y: 150, w: 400, h: 200 } });
    const out = anchoredView(prev, next, v, "file:src/ui/App.tsx");
    expect(out.k).toBe(v.k);
    expect(screen(next.cards.get("dir:src/ui")!, out)).toEqual(screen(prev.cards.get("file:src/ui/App.tsx")!, v));
  });

  it("keeps the box of a file that is no longer drawn (a layer turned off) where the box was", () => {
    const prev = layout({ "file:src/a.ts": card(700, 500) }, { "dir:src": { x: 600, y: 400, w: 800, h: 400 } });
    const next = layout({}, { "dir:src": { x: 620, y: 380, w: 600, h: 300 } });
    const out = anchoredView(prev, next, v, "file:src/a.ts");
    expect(screen(next.boxes.get("dir:src")!, out)).toEqual(screen(prev.boxes.get("dir:src")!, v));
  });

  it("never jumps to the origin when nothing of the anchor is drawn any more", () => {
    const prev = layout({ "file:src/ui/App.tsx": card(700, 500) });
    expect(anchoredView(prev, layout({ "file:b.ts": card(0, 0) }), v, "file:src/ui/App.tsx")).toEqual(v);
  });
});

describe("anchorOf", () => {
  const v: View = { k: 1, tx: 0, ty: 0 };
  const l = layout({ "file:a.ts": card(0, 0), "file:b.ts": card(500, 300), "file:c.ts": card(2000, 2000) });

  it("takes the selected node while it is on screen", () => {
    expect(anchorOf(l, v, 1200, 700, "file:a.ts")).toBe("file:a.ts");
  });

  it("else the card nearest the center of the stage", () => {
    expect(anchorOf(l, v, 1200, 700, "file:c.ts")).toBe("file:b.ts");
    expect(anchorOf(l, v, 1200, 700, null)).toBe("file:b.ts");
  });
});

describe("pickAnchor", () => {
  const v: View = { k: 1, tx: 0, ty: 0 };
  const prev = layout({ "file:a.ts": card(0, 0), "file:b.ts": card(500, 300) });

  it("keeps the folder just toggled when the plan drew it", () => {
    const withDir = layout({ "dir:lib": card(0, 0), "file:b.ts": card(500, 300) });
    expect(pickAnchor(withDir, v, 1200, 700, "dir:lib", null)).toBe("dir:lib");
  });

  it("falls back to the selection, then the card nearest the center, when the toggled folder wasn't drawn", () => {
    // A folder opened from the explorer while unconnected cards are hidden.
    expect(pickAnchor(prev, v, 1200, 700, "dir:hidden", "file:a.ts")).toBe("file:a.ts");
    expect(pickAnchor(prev, v, 1200, 700, "dir:hidden", null)).toBe("file:b.ts");
  });
});

describe("flyToward", () => {
  const FLOOR = 12 / 14;
  const W = 1200;
  const H = 700;
  const inside = (r: Rect, out: View) => {
    const [x, y] = screen(r, out) as [number, number];
    return x >= 0 && y >= 0 && x + r.w * out.k <= W && y + r.h * out.k <= H;
  };

  it("raises a zoomed-out view to the floor and shows the target, even when the target was on screen", () => {
    const v: View = { k: 0.3, tx: 100, ty: 50 };
    const r = card(600, 400); // on screen at k 0.3
    const out = flyToward(v, r, W, H, FLOOR)!;
    expect(out.k).toBeGreaterThanOrEqual(FLOOR);
    expect(inside(r, out)).toBe(true);
  });

  it("centers a far target at the floor", () => {
    const out = flyToward({ k: 0.3, tx: 100, ty: 50 }, card(8000, 6000), W, H, FLOOR)!;
    expect(out.k).toBe(FLOOR);
    const [x, y] = screen(card(8000, 6000), out) as [number, number];
    expect(x + 80 * out.k).toBeCloseTo(W / 2);
    expect(y + 20 * out.k).toBeCloseTo(H / 2);
  });

  it("doesn't move a view at or above the floor whose target is on screen", () => {
    expect(flyToward({ k: 1, tx: 0, ty: 0 }, card(10, 10), W, H, FLOOR)).toBeNull();
  });
});
