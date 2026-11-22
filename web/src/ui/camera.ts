// Where the camera goes when the plan is redrawn with another layout (a folder opened or
// closed, another level, a layer toggled, a re-index that changed the shape). Only the first
// drawing and "Encuadrar" fit the whole plan; every other change keeps the zoom and keeps one
// node, the anchor, at the same place on screen.
import { chainOf, type PlanLayout, type Rect } from "../model/plan";

export interface View {
  k: number;
  tx: number;
  ty: number;
}

/** A card or open box of the layout. */
function rectOf(layout: PlanLayout, id: string): Rect | null {
  return layout.cards.get(id) ?? layout.boxes.get(id) ?? null;
}

function onScreen(r: Rect, v: View, w: number, h: number): boolean {
  const x = r.x * v.k + v.tx;
  const y = r.y * v.k + v.ty;
  return x + r.w * v.k > 0 && y + r.h * v.k > 0 && x < w && y < h;
}

/**
 * The node to keep in place before a change: the selected node if it is on screen, else the
 * card (else the box) nearest the center of the stage of `w`×`h` px.
 */
export function anchorOf(layout: PlanLayout, v: View, w: number, h: number, selected: string | null): string | null {
  if (selected) {
    const r = rectOf(layout, selected);
    if (r && onScreen(r, v, w, h)) return selected;
  }
  const cx = (w / 2 - v.tx) / v.k;
  const cy = (h / 2 - v.ty) / v.k;
  const nearest = (rects: Map<string, Rect>) => {
    let best: string | null = null;
    let bestD = Infinity;
    for (const [id, r] of rects) {
      const dx = Math.max(r.x - cx, 0, cx - (r.x + r.w));
      const dy = Math.max(r.y - cy, 0, cy - (r.y + r.h));
      const d = dx * dx + dy * dy;
      if (d < bestD || (d === bestD && best !== null && id < best)) {
        best = id;
        bestD = d;
      }
    }
    return best;
  };
  return nearest(layout.cards) ?? nearest(layout.boxes);
}

/**
 * The node to keep in place: the folder just opened or closed (`hint`) when the old plan drew
 * it; else, as for any other change, the selection on screen or the card nearest the center.
 * (A folder toggled in the explorer may not have been drawn: an unconnected one while those
 * are hidden.)
 */
export function pickAnchor(prev: PlanLayout, v: View, w: number, h: number, hint: string | null, selected: string | null): string | null {
  if (hint && rectOf(prev, hint)) return hint;
  return anchorOf(prev, v, w, h, selected);
}

/** Room left around a box bigger than the stage when flying to it. */
const FLY_MARGIN = 28;

/** The view centered on `r`, or null when `r` is already fully on screen. */
export function flyView(v: View, r: Rect, w: number, h: number): View | null {
  const x0 = r.x * v.k + v.tx;
  const y0 = r.y * v.k + v.ty;
  if (x0 >= 0 && y0 >= 0 && x0 + r.w * v.k <= w && y0 + r.h * v.k <= h) return null;
  // A box bigger than the stage: show its top-left corner, where its name is.
  if (r.w * v.k > w || r.h * v.k > h) return { k: v.k, tx: FLY_MARGIN - r.x * v.k, ty: FLY_MARGIN - r.y * v.k };
  return { k: v.k, tx: w / 2 - (r.x + r.w / 2) * v.k, ty: h / 2 - (r.y + r.h / 2) * v.k };
}

/**
 * A fly to `r` (palette, explorer): never below `floor`, where the plan shows its own names.
 * A view under it is first zoomed to the floor about the stage center, then `r` is brought on
 * screen. Null when nothing has to move.
 */
export function flyToward(v: View, r: Rect, w: number, h: number, floor: number): View | null {
  let base = v;
  if (v.k < floor) {
    const f = floor / v.k;
    base = { k: floor, tx: w / 2 - (w / 2 - v.tx) * f, ty: h / 2 - (h / 2 - v.ty) * f };
  }
  return flyView(base, r, w, h) ?? (base === v ? null : base);
}

/**
 * The view on `next` that keeps the zoom of `v` and keeps `anchor` (a node of `prev`) where it
 * was on screen. The top-left corner of the anchor stays on the same screen point, also when
 * it changed shape (a folder card that opened into a box, or a box that closed). When the
 * anchor is gone, its nearest folder drawn on `next` takes its place: a box still drawn as
 * a box stays where it was; one folded into a card (another level) puts that card where the
 * anchor was. With nothing of it drawn on `next`, the view stays as it is (never the origin).
 */
export function anchoredView(prev: PlanLayout, next: PlanLayout, v: View, anchor: string): View {
  const from = rectOf(prev, anchor);
  if (!from) return v;
  const align = (a: Rect, b: Rect): View => ({ k: v.k, tx: v.tx + (a.x - b.x) * v.k, ty: v.ty + (a.y - b.y) * v.k });
  for (const id of chainOf(anchor).reverse()) {
    const b = rectOf(next, id);
    if (!b) continue;
    if (id === anchor) return align(from, b);
    const wasBox = prev.boxes.has(id);
    const isBox = next.boxes.has(id);
    const a = rectOf(prev, id);
    return a && wasBox === isBox ? align(a, b) : align(from, b);
  }
  return v;
}
