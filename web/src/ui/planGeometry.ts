// Drawing helpers for the plan: rounded orthogonal routes, arrowheads, and a free spot for
// the weight tag of the heaviest edge.
import type { Point, Rect } from "../model/plan";

/** Orthogonal polyline with rounded corners. */
export function roundPath(pts: Point[], r = 7): string {
  const f = (n: number) => Math.round(n * 10) / 10;
  let d = `M${f(pts[0]!.x)},${f(pts[0]!.y)}`;
  for (let i = 1; i < pts.length - 1; i++) {
    const p0 = pts[i - 1]!;
    const p1 = pts[i]!;
    const p2 = pts[i + 1]!;
    const d1 = Math.hypot(p1.x - p0.x, p1.y - p0.y) || 1;
    const d2 = Math.hypot(p2.x - p1.x, p2.y - p1.y) || 1;
    const rr = Math.min(r, d1 / 2, d2 / 2);
    const a = { x: p1.x - ((p1.x - p0.x) / d1) * rr, y: p1.y - ((p1.y - p0.y) / d1) * rr };
    const b = { x: p1.x + ((p2.x - p1.x) / d2) * rr, y: p1.y + ((p2.y - p1.y) / d2) * rr };
    d += ` L${f(a.x)},${f(a.y)} Q${f(p1.x)},${f(p1.y)} ${f(b.x)},${f(b.y)}`;
  }
  const l = pts[pts.length - 1]!;
  return `${d} L${f(l.x)},${f(l.y)}`;
}

/** Arrowhead with its tip at `tip`, pointing away from `from`. */
export function arrowPath(tip: Point, from: Point, size = 7): string {
  const dx = tip.x - from.x;
  const dy = tip.y - from.y;
  const len = Math.hypot(dx, dy) || 1;
  const ux = dx / len;
  const uy = dy / len;
  const bx = tip.x - ux * size;
  const by = tip.y - uy * size;
  const f = (n: number) => Math.round(n * 10) / 10;
  return `M${f(tip.x)},${f(tip.y)} L${f(bx - uy * size * 0.5)},${f(by + ux * size * 0.5)} L${f(bx + uy * size * 0.5)},${f(by - ux * size * 0.5)}Z`;
}

/**
 * A spot next to a vertical segment of the route where a tag of width `w` doesn't touch a
 * card or the box label. Tries the longest segments first, near their middle, both sides.
 */
export function tagSpot(route: Point[], w: number, stroke: number, avoid: Rect[]): Point | null {
  const segs: { x: number; y0: number; len: number }[] = [];
  for (let i = 0; i < route.length - 1; i++) {
    const a = route[i]!;
    const b = route[i + 1]!;
    const len = Math.abs(b.y - a.y);
    if (Math.abs(a.x - b.x) < 0.5 && len >= 30) segs.push({ x: a.x, y0: Math.min(a.y, b.y), len });
  }
  segs.sort((a, b) => b.len - a.len);
  for (const s of segs) {
    for (const t of [0.5, 0.35, 0.65, 0.2, 0.8]) {
      for (const side of [1, -1]) {
        const y = s.y0 + s.len * t - 10;
        const x = side > 0 ? s.x + stroke / 2 + 6 : s.x - stroke / 2 - 6 - w;
        const hit = avoid.some((b) => x < b.x + b.w && x + w > b.x && y < b.y + b.h && y + 20 > b.y);
        if (!hit) return { x, y };
      }
    }
  }
  return null;
}

/** Cuts a label to about `max` characters with an ellipsis. */
export function truncate(s: string, max: number): string {
  return s.length > max ? `${s.slice(0, max - 1)}…` : s;
}

/** How the minimap draws a plan: the element's size and where the plan lands inside it. */
export interface MinimapFrame {
  /** Element size in px (at least 48×36, so a thin plan stays clickable). */
  w: number;
  h: number;
  /** Margin around the plan, in plan units (the viewBox starts at -pad, -pad). */
  pad: number;
  /** Plan units → px as rendered: the viewBox fits the element with `meet`, centered. */
  scale: number;
  /** Where the viewBox starts inside the element, in px (non-zero when the size was clamped). */
  ox: number;
  oy: number;
}

export function minimapFrame(width: number, height: number, maxW = 200, maxH = 132, pad = 40): MinimapFrame {
  const lw = width + 2 * pad;
  const lh = height + 2 * pad;
  const s = Math.min(maxW / lw, maxH / lh);
  const w = Math.max(48, lw * s);
  const h = Math.max(36, lh * s);
  // preserveAspectRatio="xMidYMid meet": the clamped side leaves margins on both ends.
  const scale = Math.min(w / lw, h / lh);
  return { w, h, pad, scale, ox: (w - lw * scale) / 2, oy: (h - lh * scale) / 2 };
}

/** A point on the minimap (px from its top-left corner) in plan coordinates. */
export function minimapToPlan(f: MinimapFrame, x: number, y: number): Point {
  return { x: (x - f.ox) / f.scale - f.pad, y: (y - f.oy) / f.scale - f.pad };
}
