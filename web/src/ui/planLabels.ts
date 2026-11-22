// Names over a plan zoomed out past the readability floor. The plan's own text would be under
// 12 px there and is hidden, so the names come back as labels of a fixed on-screen size,
// anchored to their card or box. Every card always shows its name: the plan never
// zooms out past K_MIN, where a card is CARD_MIN px wide, and down to there each card's name,
// cut to fit, has the card's width plus the gap to its neighbour. Card names sit side by
// side in their rows, so they never overlap. Box titles go where card names leave room;
// when a title finds none, its whole depth level goes (deepest first), never some of it.
//
// Placement works in "scaled plan" space (plan units × k): it depends on the layout and the
// zoom only, not on the pan, so dragging the plan never makes a label appear or vanish
// except at the edges of the stage.
import { BOX_PAD, CARD, CARD_GAP, chainOf, type Rect } from "../model/plan";

/** On-screen label font size (px). The plan's floor: no text under 12 px. */
export const LABEL_FONT = 12;
/** Label heights (px): a box title sits on a pill, a card name on its card. */
export const BOX_LABEL_H = 16;
export const CARD_LABEL_H = 16;
/**
 * The narrowest a card gets on screen (px). With the gap to its neighbour, its name has
 * (160 + 18)·0.3 − 5 ≈ 48 px: 40 px of text at 12 px, about four characters, an ellipsis and
 * a fifth character ("Chro…lay"), the least that still tells siblings apart.
 */
export const CARD_MIN = 48;
/** The smallest zoom: every card is CARD_MIN wide. */
export const K_MIN = CARD_MIN / CARD.w;
/** Horizontal padding inside a label (px), both sides together. */
const PAD = 8;
/** Room between a label and the edge of its card or box (px). */
const INSET = 3;
/** A box title gets at least this much room (px), past a narrow box if need be. */
const MIN_ROOM = 56;
/** Longest label (px): a longer name is cut. */
const MAX_W = 170;
/** Clearance between two labels (px). */
const GAP = 2;

export interface LabelCandidate {
  id: string;
  text: string;
  /** Box titles sit at the top-left corner of their box; card names on their card. */
  type: "box" | "card";
  /** In plan units. */
  rect: Rect;
  /** Lower goes first among labels of the same depth. */
  priority: number;
  /** For the kind tint. */
  kind: string;
  /** Path segments: top-level folders are 1. */
  depth: number;
  /** A file: its extension may go when the name is cut… */
  file?: boolean;
  /** …unless a sibling has the same name with another extension (`index.ts`, `index.js`). */
  keepExt?: boolean;
}

export interface PlacedLabel {
  id: string;
  /** The name as shown (cut to fit when needed). */
  text: string;
  type: "box" | "card";
  kind: string;
  /** Top-left corner and size in scaled plan space (px, before the pan). */
  x: number;
  y: number;
  w: number;
  h: number;
  /** The anchor in scaled plan space and the label's offset from it in px: x = ax + dx. */
  ax: number;
  ay: number;
  dx: number;
  dy: number;
}

/** Width of a text in px. */
export type Measure = (text: string) => number;

/** A rough measure of Hanken Grotesk 600 at `px` (no canvas, or the font not loaded yet). */
export const roughMeasure = (px: number): Measure => (text) => text.length * px * 0.575;

const ELLIPSIS = "…";

/** `text` cut at the end to fit `max` px ("shipping-rates-t…"). */
export function fitEnd(text: string, max: number, measure: Measure): string {
  if (measure(text) <= max) return text;
  let lo = 0;
  let hi = text.length - 1;
  while (lo < hi) {
    const mid = Math.ceil((lo + hi) / 2);
    if (measure(text.slice(0, mid) + ELLIPSIS) <= max) lo = mid;
    else hi = mid - 1;
  }
  return text.slice(0, Math.max(1, lo)) + ELLIPSIS;
}

/** The extension of a file name, with a marker before it: `.ts`, `.test.ts`, `.d.ts`, `.module.css`. */
const EXTENSION = /(\.(test|spec|e2e|bench|d|module|stories|story))?\.[^.]+$/i;

/** A file name without its extension (`index.test.ts` → `index`); a dotfile keeps its name. */
export function stemOf(name: string): string {
  const s = name.replace(EXTENSION, "");
  return s.length > 0 ? s : name;
}

/**
 * The files whose cut name keeps its extension: a sibling in the same box (a file, or a
 * folder by its whole name) would read the same without it (`index.ts` and `index.js`,
 * `utils.ts` next to the folder `utils`).
 */
export function keepsExtension(
  nodes: readonly { id: string; name: string; kind: string; box: string | null }[],
  /** Open folders: siblings by their whole name too. */
  boxes: readonly { name: string; box: string | null }[] = [],
): Set<string> {
  const key = (n: { name: string; kind: string; box: string | null }) => `${n.box ?? ""}|${n.kind === "file" ? stemOf(n.name) : n.name}`;
  const count = new Map<string, number>();
  const add = (k: string) => count.set(k, (count.get(k) ?? 0) + 1);
  for (const n of nodes) if (n.kind === "file" || n.kind === "dir") add(key(n));
  for (const b of boxes) add(key({ ...b, kind: "dir" }));
  return new Set(nodes.filter((n) => n.kind === "file" && (count.get(key(n)) ?? 0) > 1).map((n) => n.id));
}

export interface FitOptions {
  /** A file: its extension may go first (the card's color already says what it is). */
  file?: boolean;
  /** Keep the extension anyway: a sibling would read the same without it. */
  keepExt?: boolean;
}

/**
 * A file or folder name cut to fit `max` px for a label: whole if it fits, else (a file only)
 * without its extension, else with an ellipsis in the middle, because the end of a name is
 * what tells siblings apart ("OrderSummary…Panel"). Folder names keep their dots
 * (`com.example.app`).
 */
export function fitName(text: string, max: number, measure: Measure, opts: FitOptions = {}): string {
  if (measure(text) <= max) return text;
  const base = opts.file && !opts.keepExt ? stemOf(text) : text;
  if (measure(base) <= max) return base;
  // The end gets the larger half: "Orde…Panel".
  const cut = (n: number) => base.slice(0, Math.floor(n / 2)) + ELLIPSIS + base.slice(base.length - Math.ceil(n / 2));
  let lo = 1;
  let hi = base.length - 1;
  while (lo < hi) {
    const mid = Math.ceil((lo + hi) / 2);
    if (measure(cut(mid)) <= max) lo = mid;
    else hi = mid - 1;
  }
  return cut(lo);
}

/** Where a box title sits under its box's top edge: centred in the header strip, at most 3 px down. */
export function titleDy(k: number): number {
  return Math.min(3, Math.max(0, Math.floor((BOX_PAD.top * k - BOX_LABEL_H) / 2)));
}

type Box4 = { x: number; y: number; w: number; h: number };

/** Labels already placed, bucketed on a coarse grid for the overlap test. */
class Grid {
  private cells = new Map<string, PlacedLabel[]>();
  readonly all: PlacedLabel[] = [];
  private static CELL = 96;
  private keys(l: Box4): string[] {
    const C = Grid.CELL;
    const out: string[] = [];
    for (let cx = Math.floor((l.x - GAP) / C); cx <= Math.floor((l.x + l.w + GAP) / C); cx++)
      for (let cy = Math.floor((l.y - GAP) / C); cy <= Math.floor((l.y + l.h + GAP) / C); cy++) out.push(`${cx},${cy}`);
    return out;
  }
  /** The placed labels `l` would touch. */
  hits(l: Box4): PlacedLabel[] {
    const out = new Set<PlacedLabel>();
    for (const key of this.keys(l))
      for (const o of this.cells.get(key) ?? [])
        if (l.x < o.x + o.w + GAP && o.x < l.x + l.w + GAP && l.y < o.y + o.h + GAP && o.y < l.y + l.h + GAP) out.add(o);
    return [...out];
  }
  add(l: PlacedLabel) {
    this.all.push(l);
    for (const key of this.keys(l)) {
      const list = this.cells.get(key);
      if (list) list.push(l);
      else this.cells.set(key, [l]);
    }
  }
}

/**
 * Picks and places the labels for zoom `k` (see the top of the file for the rule). Every card
 * gets its name; titles go where they find room, a whole depth level at a time. `fits` caches
 * cut names across calls (room is rounded down to 4 px, so a zoom sweep reuses them); drop it
 * when `measure` changes.
 */
export function placeLabels(cands: readonly LabelCandidate[], k: number, measure: Measure, fits = new Map<string, string>()): PlacedLabel[] {
  const fit = (c: LabelCandidate, room: number) => {
    const max = Math.max(4, Math.floor(Math.min(room, MAX_W) / 4) * 4) - PAD;
    const key = `${max}|${c.file ? (c.keepExt ? "k" : "f") : "d"}|${c.text}`;
    let s = fits.get(key);
    if (s === undefined) fits.set(key, (s = fitName(c.text, max, measure, c)));
    return s;
  };
  const byDepth = (a: LabelCandidate, b: LabelCandidate) => a.depth - b.depth || a.priority - b.priority || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);

  /**
   * A card's name on its card. It may use the gap to the next card too, short of GAP: the
   * next card's name starts INSET into that card, so names in a row never touch.
   */
  const cardLabel = (c: LabelCandidate): PlacedLabel => {
    const sx = c.rect.x * k;
    const cy = (c.rect.y + c.rect.h / 2) * k;
    const text = fit(c, Math.floor((c.rect.w + CARD_GAP) * k - INSET - GAP));
    const dy = -CARD_LABEL_H / 2;
    return { id: c.id, text, type: "card", kind: c.kind, x: sx + INSET, y: cy + dy, w: Math.ceil(measure(text)) + PAD, h: CARD_LABEL_H, ax: sx, ay: cy, dx: INSET, dy };
  };
  /**
   * A box title's spot: in the header strip, else further right in it, across the top edge,
   * above it, then lower down (each also tried further right, past what blocks it, while it
   * starts inside the box). Then on the line of the nearest folder above it whose title is
   * placed, right after it, like a path ("src  test  unit"), when that title sits by the box:
   * a small box alone in the corner of its folder has no room of its own. Last, under the box.
   * Null when none is free.
   */
  const titleSpot = (grid: Grid, c: LabelCandidate, titles: Map<string, PlacedLabel>): PlacedLabel | null => {
    const sx = c.rect.x * k;
    const sy = c.rect.y * k;
    const right = sx + c.rect.w * k - INSET;
    const text = fit(c, Math.max(Math.floor(c.rect.w * k - 2 * INSET), MIN_ROOM));
    const w = Math.ceil(measure(text)) + PAD;
    const top = titleDy(k);
    const dys = [top, -BOX_LABEL_H / 2, -BOX_LABEL_H - 1];
    // Lower lines stack the titles of nested boxes that share a corner ("src", "test", "unit").
    // A title starts inside its box, though a small box's title may hang past its bottom.
    for (let line = 1; line < 6; line++) {
      const dy = top + line * (BOX_LABEL_H + GAP + 1);
      if (dy > c.rect.h * k - 4) break;
      dys.push(dy);
    }
    const at = (dx: number, dy: number): PlacedLabel => ({ id: c.id, text, type: "box", kind: c.kind, x: sx + dx, y: sy + dy, w, h: BOX_LABEL_H, ax: sx, ay: sy, dx, dy });
    for (const dy of dys) {
      let l = at(INSET, dy);
      for (let tries = 0; tries < 4; tries++) {
        const hits = grid.hits(l);
        if (hits.length === 0) return l;
        const x = Math.max(...hits.map((o) => o.x + o.w)) + GAP + 1;
        // A small box's title may run past its right edge, as long as it starts inside the box.
        if (x > right - 4) break;
        l = at(x - sx, dy);
      }
    }
    // Only when that title is by the box: within a line of its top, and the path step starts
    // inside the box or the gap after it. Never pushed further right.
    for (const up of chainOf(c.id).slice(0, -1).reverse()) {
      const t = titles.get(up);
      if (!t) continue;
      const x = t.x + t.w + GAP + 1;
      if (Math.abs(t.y - sy) <= BOX_LABEL_H + GAP + 1 && x >= sx && x <= (c.rect.x + c.rect.w + CARD_GAP) * k) {
        const l = at(x - sx, t.y - sy);
        if (grid.hits(l).length === 0) return l;
      }
      break;
    }
    // A small box with no room left above or inside: its title as a caption under it.
    const under = at(INSET, c.rect.h * k + GAP);
    return grid.hits(under).length === 0 ? under : null;
  };

  const names = cands.filter((c) => c.type === "card").map(cardLabel);
  const boxes = cands.filter((c) => c.type === "box").sort(byDepth);
  /** Every card name and the titles down to `depth`; the depth of the first title with no room (unless `greedy`). */
  const tryDepth = (depth: number, greedy: boolean): { grid: Grid; drop: number | null } => {
    const grid = new Grid();
    for (const l of names) grid.add(l);
    const titles = new Map<string, PlacedLabel>();
    for (const c of boxes) {
      if (c.depth > depth) continue;
      const spot = titleSpot(grid, c, titles);
      if (spot) {
        grid.add(spot);
        titles.set(c.id, spot);
      } else if (!greedy) {
        return { grid, drop: c.depth };
      }
    }
    return { grid, drop: null };
  };
  let depth = Math.max(0, ...boxes.map((c) => c.depth));
  while (depth >= 1) {
    const r = tryDepth(depth, false);
    if (r.drop === null) return r.grid.all;
    depth = r.drop - 1;
  }
  // Even the top titles find no room: as many of them as fit.
  return tryDepth(1, true).grid.all;
}

/** Whether a placed label shows on a stage of `w`×`h` px under the pan `tx`, `ty`. */
export function onStage(l: PlacedLabel, tx: number, ty: number, w: number, h: number): boolean {
  const x = l.x + tx;
  const y = l.y + ty;
  return x + l.w > 0 && y + l.h > 0 && x < w && y < h;
}

let ctx: CanvasRenderingContext2D | null | undefined;
const caches = new Map<number, Map<string, number>>();

/** Whether the plan font has loaded (measures before that are rough and not cached). */
export function fontReady(): boolean {
  return typeof document !== "undefined" && document.fonts?.check?.(`600 ${LABEL_FONT}px "Hanken Grotesk Variable"`) === true;
}

/**
 * A measure of Hanken Grotesk 600 at `px` with a canvas, cached per string. Falls back to the
 * rough measure where there is no canvas (jsdom) and until the font has loaded; the cache only
 * keeps real measures.
 */
export function textMeasure(px: number): Measure {
  const rough = roughMeasure(px);
  let cache = caches.get(px);
  if (!cache) caches.set(px, (cache = new Map()));
  const widths = cache;
  return (text) => {
    const cached = widths.get(text);
    if (cached !== undefined) return cached;
    if (ctx === undefined) {
      ctx = null;
      if (typeof document !== "undefined" && typeof navigator !== "undefined" && !/jsdom/i.test(navigator.userAgent)) {
        ctx = document.createElement("canvas").getContext("2d");
      }
    }
    if (!ctx || !fontReady()) return rough(text);
    ctx.font = `600 ${px}px "Hanken Grotesk Variable", "Hanken Grotesk", "Segoe UI", system-ui, sans-serif`;
    const w = ctx.measureText(text).width;
    widths.set(text, w);
    return w;
  };
}
