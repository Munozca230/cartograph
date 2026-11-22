// The camera of the Flujos view. A new flow (another start) is fitted; any other redraw (a
// function expanded, a re-index) keeps the zoom and keeps one card where it was on screen
// (camera.ts): the card just expanded, else the selection on screen, else the card nearest
// the center. Names are drawn at 14 px. A new flow opens whole when it fits at
// FLOW_K_OPEN or closer (names at 12 px or more); else at FLOW_K_OPEN, framed to show as many
// whole cards as it can, the start among them. "Encuadrar" may go down to FLOW_K_MIN, where the
// wheel stops too and names still read at 7 px (names are always drawn). What the stage
// doesn't show, the minimap does.
import { anchoredView, anchorOf, type View } from "./camera";
import type { FlowLayout } from "../model/callflow";
import type { Rect } from "../model/plan";
import { minimapFrame } from "./planGeometry";

export const FLOW_K_OPEN = 12 / 14;
export const FLOW_K_MIN = 0.5;
export const FLOW_K_MAX = 2;
const MARGIN = 12;
/** Where the minimap sits, from the stage's bottom right corner (Minimap, `right`/`bottom` + border). */
const MINIMAP_INSET = 16;

/** The corner of the stage the minimap covers while the flow overflows (screen px). */
export function minimapCover(layout: FlowLayout, w: number, h: number): Rect {
  const f = minimapFrame(layout.width, layout.height);
  return { x: w - MINIMAP_INSET - f.w, y: h - MINIMAP_INSET - f.h, w: f.w + MINIMAP_INSET, h: f.h + MINIMAP_INSET };
}

function fullyIn(r: Rect, v: View, w: number, h: number, cover: Rect | null): boolean {
  const x0 = r.x * v.k + v.tx;
  const y0 = r.y * v.k + v.ty;
  const x1 = x0 + r.w * v.k;
  const y1 = y0 + r.h * v.k;
  if (x0 < -0.5 || y0 < -0.5 || x1 > w + 0.5 || y1 > h + 0.5) return false;
  return !cover || x1 <= cover.x || y1 <= cover.y;
}

/** Cards wholly on a stage of `w`×`h` px, and not under `cover`. */
export function visibleCards(layout: FlowLayout, v: View, w: number, h: number, cover: Rect | null = null): number {
  let n = 0;
  for (const r of layout.cards.values()) if (fullyIn(r, v, w, h, cover)) n++;
  return n;
}

/**
 * The view a flow opens with on a stage of `w`×`h` px, never below `floor`:
 * - the whole flow, centered, when it fits at `floor` or closer (never past 1:1);
 * - else, at `floor`, the window over the flow that shows the most whole cards (outside the
 *   minimap's corner), with the start (`root`) among them; on a tie, the one most to the top
 *   left, where the flow starts. Nothing of the flow is cut by the stage's edges that a shift
 *   could have shown whole.
 */
export function fitFlow(layout: FlowLayout, w: number, h: number, root: string | null = null, floor = FLOW_K_OPEN): View {
  const W = layout.width;
  const H = layout.height;
  if (W <= 0 || H <= 0 || w <= 0 || h <= 0) return { k: 1, tx: MARGIN, ty: MARGIN };
  const aw = w - 2 * MARGIN;
  const ah = h - 2 * MARGIN;
  const all = Math.min(1, aw / W, ah / H);
  if (all >= floor) return { k: all, tx: (w - W * all) / 2, ty: (h - H * all) / 2 };
  const k = floor;
  const cover = minimapCover(layout, w, h);
  const vw = w / k;
  const vh = h / k;
  const m = MARGIN / k;
  // Candidate left and top edges of the window (layout px): a card at the window's left or top
  // edge; at its right or bottom edge, beside or above the minimap; or just past it, so the
  // window ends in the gap before that card instead of cutting it. A side that fits whole may
  // also be centered (preferred on a tie); else the flow's top left corner is.
  const axis = (size: number, view: number, starts: number[], ends: number[], coverSize: number): number[] => {
    const hi = size - view + m;
    const fits = size <= view - 2 * m;
    const home = fits ? (size - view) / 2 : -m;
    const out = new Set<number>([Math.round(home)]);
    const add = (x: number) => out.add(Math.round(Math.min(hi, Math.max(-view + m, x))));
    if (fits) {
      add(-m);
      add(hi);
    }
    for (const a of starts) {
      add(a - m);
      add(a - view - 2);
    }
    for (const b of ends) {
      add(b - view + m);
      add(b - view + m + coverSize / k);
    }
    return [...out].sort((a, b) => Math.abs(a - home) - Math.abs(b - home) || a - b);
  };
  const rects = [...layout.cards.values()];
  const xs = axis(
    W,
    vw,
    rects.map((r) => r.x),
    rects.map((r) => r.x + r.w),
    cover.w,
  );
  const ys = axis(
    H,
    vh,
    rects.map((r) => r.y),
    rects.map((r) => r.y + r.h),
    cover.h,
  );
  const start = root ? layout.cards.get(root) : undefined;
  // Every window is scored by its whole cards and the cards it cuts. For one left edge, each
  // card is whole (or touched) for a range of top edges, since its top and bottom on screen
  // only fall as the top edge grows: counted over the sorted top edges with difference
  // arrays, the ranges found by binary search. O(n² log n) for n cards.
  const up = [...ys].sort((a, b) => a - b);
  const tys = up.map((y) => -y * k);
  const rank = new Map(up.map((y, i) => [y, i]));
  const ranks = ys.map((y) => rank.get(y)!);
  const ny = up.length;
  /** The first top edge (index) that puts `top` (+ `extra`) on screen under `limit` (or at it, with `orAt`), else ny. */
  const first = (top: number, extra: number, limit: number, orAt: boolean): number => {
    let lo = 0;
    let hi = ny;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      const y = top + tys[mid]! + extra;
      if (y < limit || (orAt && y === limit)) hi = mid;
      else lo = mid + 1;
    }
    return lo;
  };
  const fullD = new Int32Array(ny + 1);
  const touchD = new Int32Array(ny + 1);
  const bothD = new Int32Array(ny + 1);
  const fullN = new Int32Array(ny);
  const cutN = new Int32Array(ny);
  const span = (d: Int32Array, lo: number, hi: number) => {
    if (lo >= hi) return;
    d[lo]!++;
    d[hi]!--;
  };
  let best: View | null = null;
  let bestFull = -1;
  let bestCut = Infinity;
  for (const x of xs) {
    const tx = -x * k;
    if (start) {
      // The start must be whole: no top edge helps a left edge that cuts it.
      const s0 = start.x * k + tx;
      if (s0 < -0.5 || s0 + start.w * k > w + 0.5) continue;
    }
    fullD.fill(0);
    touchD.fill(0);
    bothD.fill(0);
    for (const r of rects) {
      // As in fullyIn and touches, with the left edge fixed.
      const x0 = r.x * k + tx;
      const x1 = x0 + r.w * k;
      const xFull = x0 >= -0.5 && x1 <= w + 0.5;
      const xTouch = x0 < w && x1 > 0;
      if (!xFull && !xTouch) continue;
      // On screen, the card's top is `top + ty` and its bottom that plus `rh`.
      const top = r.y * k;
      const rh = r.h * k;
      let lo = 0;
      let hi = 0;
      if (xFull) {
        // Bottom at h + 0.5 or above, top at -0.5 or below; the minimap's corner only matters
        // to a card that reaches past its left side.
        lo = Math.max(first(top, rh, h + 0.5, true), x1 <= cover.x ? 0 : first(top, rh, cover.y, true));
        hi = first(top, 0, -0.5, false);
        span(fullD, lo, hi);
      }
      if (xTouch) {
        // Top above h, bottom below 0.
        const tlo = first(top, 0, h, false);
        const thi = first(top, rh, 0, true);
        span(touchD, tlo, thi);
        if (xFull) span(bothD, Math.max(lo, tlo), Math.min(hi, thi));
      }
    }
    // Cut: touched and not whole.
    let fullSum = 0;
    let touchSum = 0;
    let bothSum = 0;
    for (let i = 0; i < ny; i++) {
      fullSum += fullD[i]!;
      touchSum += touchD[i]!;
      bothSum += bothD[i]!;
      fullN[i] = fullSum;
      cutN[i] = touchSum - bothSum;
    }
    for (let j = 0; j < ys.length; j++) {
      const v = { k, tx, ty: -ys[j]! * k };
      if (start && !fullyIn(start, v, w, h, cover)) continue;
      // Whole cards first; then as few cut by an edge or the minimap as possible.
      const i = ranks[j]!;
      const full = fullN[i]!;
      const cut = cutN[i]!;
      if (full > bestFull || (full === bestFull && cut < bestCut)) {
        best = v;
        bestFull = full;
        bestCut = cut;
      }
    }
  }
  if (best) return best;
  // The start can't be shown whole (a card wider than the stage): its top left corner.
  const r = start ?? { x: 0, y: 0, w: 0, h: 0 };
  return { k, tx: MARGIN - r.x * k, ty: MARGIN - r.y * k };
}

/**
 * What a redraw does to the camera: another start (or the first flow) is fitted ("fit"); a new
 * layout of the same start keeps a card in place ("keep"); the same layout again, as after a
 * resize, leaves the view alone ("stay"). Only "keep" uses the card just expanded.
 */
export function cameraStep(prev: { root: string; layout: FlowLayout } | null, next: { root: string; layout: FlowLayout }): "fit" | "keep" | "stay" {
  if (!prev || prev.root !== next.root) return "fit";
  return prev.layout === next.layout ? "stay" : "keep";
}

export function zoomAt(v: View, factor: number, x: number, y: number): View {
  const k = Math.min(FLOW_K_MAX, Math.max(FLOW_K_MIN, v.k * factor));
  const f = k / v.k;
  return { k, tx: x - (x - v.tx) * f, ty: y - (y - v.ty) * f };
}

/**
 * The view on `next` after a redraw of the same flow: `hint` (the card just expanded) if the
 * old layout drew it, else the selection on screen or the card nearest the center, kept in
 * place at the same zoom. With nothing of it on `next`, the view stays.
 */
export function keepView(prev: FlowLayout, next: FlowLayout, v: View, w: number, h: number, hint: string | null, selected: string | null): View {
  const anchor = hint && prev.cards.has(hint) ? hint : anchorOf(prev, v, w, h, selected);
  return anchor ? anchoredView(prev, next, v, anchor) : v;
}

/** The view that brings `r` on screen at the current zoom, or null when it already is. */
export function revealCard(v: View, r: { x: number; y: number; w: number; h: number }, w: number, h: number): View | null {
  const x0 = r.x * v.k + v.tx;
  const y0 = r.y * v.k + v.ty;
  if (x0 >= 0 && y0 >= 0 && x0 + r.w * v.k <= w && y0 + r.h * v.k <= h) return null;
  return { k: v.k, tx: w / 2 - (r.x + r.w / 2) * v.k, ty: h / 2 - (r.y + r.h / 2) * v.k };
}
