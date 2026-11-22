// The "Flujos" view: a call flow from /api/flow, left to right, in one swimlane per package or
// top folder. Lanes are horizontal bands stacked in the order the flow reaches them; the
// columns are shared by every lane (swimlanes.ts), so a flow is as wide as it is deep and as
// tall as its lanes, not one column per lane. A card is as wide as its name needs: names are
// never cut; the file under it gives up its first folders to fit. Everything here is pure.
import type { EntryKind, FlowEdgeData, FlowEntry, FlowNodeData, FlowResponse } from "../api/types";
import { edgeKey } from "./flow";
import type { PlanLayout, Rect } from "./plan";
import { swimlanes, type SwimlaneOptions } from "./swimlanes";

/**
 * Card geometry: two lines (name, `file:line · ←N`), or three when a long qualified name is
 * drawn on two (`BackendServiceManager.` over `startServices`); `line` apart.
 */
export const FLOW_CARD = { h: 48, line: 17, minW: 120, padX: 12, stripe: 4, pillGap: 8, pillPad: 7, nameY: 20 };
/** A qualified name wider than this may go on two lines. */
const WRAP_OVER = 220;
/** Name and meta font sizes, as drawn. */
export const FLOW_NAME_FONT = 14;
export const FLOW_META_FONT = 12;
export const LANE_FONT = 11;
/** Room around the cards of a lane, and above them for its title. */
export const LANE_PAD = { side: 10, top: 26, bottom: 10 };
/** The lane title's inset from the lane's left and top edges. */
export const LANE_TITLE = { x: 10, y: 17 };

export type Measure = (text: string) => number;

export interface FlowMeasures {
  name: Measure;
  meta: Measure;
  lane: Measure;
}

/** Rough measures (no canvas): Hanken Grotesk is about 0.575 em wide on average at 600. */
export const ROUGH: FlowMeasures = {
  name: (t) => t.length * FLOW_NAME_FONT * 0.575,
  meta: (t) => t.length * FLOW_META_FONT * 0.575,
  lane: (t) => t.length * LANE_FONT * 0.72,
};

/** The lane's title: its folder's last name (`crates/cartograph-cli` → `cartograph-cli`). */
export function laneLabel(lane: string): string {
  if (!lane) return "raíz";
  const parts = lane.split("/");
  // Under a source root (`src/ui`) both levels say something.
  if (parts.length === 2 && ["src", "lib", "app", "source", "pkg", "internal", "packages"].includes(parts[0]!.toLowerCase())) return lane;
  return parts[parts.length - 1]!;
}

/** A file path as the card shows it: relative to its lane's folder (`src/lib.rs`). */
export function shortFile(file: string, lane: string): string {
  if (lane && file.startsWith(`${lane}/`)) return file.slice(lane.length + 1);
  return file;
}

/** `src/lib.rs:214 · ←33` (no line for a whole file, no count when nobody calls it). */
export function metaLine(n: FlowNodeData, file: string = shortFile(n.file, n.lane)): string {
  const where = n.kind === "file" ? file : `${file}:${n.line}`;
  return n.callers > 0 ? `${where} · ←${n.callers}` : where;
}

/**
 * How each file of a flow is named on its cards: the shortest end of its path that no other
 * file of its lane shares (`lib.rs`, `git/mod.rs` beside `imports/mod.rs`), the way editors
 * tell their tabs apart; the lane's title says the rest. The full path is in the tooltip and
 * the panel.
 */
export function fileLabels(flow: FlowResponse): Map<string, string> {
  const lanes = new Map<string, Set<string>>();
  for (const n of flow.nodes) {
    const set = lanes.get(n.lane) ?? new Set<string>();
    set.add(n.file);
    lanes.set(n.lane, set);
  }
  const out = new Map<string, string>();
  for (const [lane, set] of lanes) {
    const files = [...set];
    for (const f of files) {
      if (out.has(f)) continue;
      const parts = shortFile(f, lane).split("/");
      let label = parts.join("/");
      for (let k = 1; k < parts.length; k++) {
        const end = parts.slice(-k).join("/");
        if (!files.some((g) => g !== f && (g === end || g.endsWith(`/${end}`)))) {
          label = end;
          break;
        }
      }
      out.set(f, label);
    }
  }
  return out;
}

/** The meta line of a card, with its file as `labels` name it. */
export function cardMeta(n: FlowNodeData, labels: Map<string, string>): string {
  return metaLine(n, labels.get(n.file) ?? shortFile(n.file, n.lane));
}

/** The "+N" pill of a card whose callees were cut. */
export function moreText(n: FlowNodeData): string | null {
  return n.more > 0 ? `+${n.more}` : null;
}

/**
 * A card already opened that still leaves calls out: the flow's cap (40 calls per card, 300
 * cards) was reached. Its pill says so instead of offering a click that opens nothing.
 */
export function moreCapped(n: FlowNodeData): boolean {
  return n.more > 0 && n.open === true;
}

/** Lane id as a layout box id (never a symbol id). */
export function laneId(lane: string): string {
  return `lane:${lane}`;
}

export interface FlowCardSize {
  w: number;
  h: number;
}

/** Width of the "+N" pill and the gap before it (0 without one). */
export function pillRoom(n: FlowNodeData, m: Measure): number {
  const pill = moreText(n);
  return pill ? m(pill) + 2 * FLOW_CARD.pillPad + FLOW_CARD.pillGap : 0;
}

/**
 * A qualified name split after its last dot (`Manager.` and `start`), or null. Every character
 * is kept: the two lines read as the whole name.
 */
export function splitName(name: string): [string, string] | null {
  const dot = name.lastIndexOf(".");
  return dot > 0 && dot < name.length - 1 ? [name.slice(0, dot + 1), name.slice(dot + 1)] : null;
}

/** The lines of a card's name: two for a card of the taller height. */
export function nameLines(name: string, h: number): string[] {
  const parts = h > FLOW_CARD.h ? splitName(name) : null;
  return parts ?? [name];
}

/** Baseline of the meta line in a card of height `h`. */
export function metaY(h: number): number {
  return FLOW_CARD.nameY + FLOW_CARD.line * (h > FLOW_CARD.h ? 2 : 1);
}

/** Room for text in a card of width `w`: what the padding and the stripe leave. */
export function textRoom(w: number): number {
  return w - 2 * FLOW_CARD.padX - FLOW_CARD.stripe;
}

/**
 * Width of every card: its whole name (with the finding dot), at least the minimum, and the
 * meta line with its pill. A long qualified name goes on two lines when that makes the card a
 * fifth narrower or more.
 */
export function cardSizes(flow: FlowResponse, m: FlowMeasures = ROUGH): Map<string, FlowCardSize> {
  const out = new Map<string, FlowCardSize>();
  const pad = 2 * FLOW_CARD.padX + FLOW_CARD.stripe;
  const labels = fileLabels(flow);
  for (const n of flow.nodes) {
    const meta = m.meta(cardMeta(n, labels)) + pillRoom(n, m.meta);
    const dot = n.finding ? 16 : 0;
    const width = (name: number) => Math.ceil(Math.max(name + dot, textRoom(FLOW_CARD.minW), meta) + pad);
    const one = width(m.name(n.name));
    const parts = m.name(n.name) > WRAP_OVER ? splitName(n.name) : null;
    const two = parts ? width(Math.max(m.name(parts[0]), m.name(parts[1]))) : Infinity;
    out.set(n.id, two <= one * 0.8 ? { w: two, h: FLOW_CARD.h + FLOW_CARD.line } : { w: one, h: FLOW_CARD.h });
  }
  return out;
}

/** How wide a lane must be for its title: the title's inset on both sides. */
export function laneTitleW(lane: string, m: FlowMeasures = ROUGH): number {
  return Math.ceil(m.lane(laneLabel(lane).toUpperCase()) + 2 * LANE_TITLE.x);
}

/** Lanes in the order the flow reaches them (the start's lane first). */
export function laneOrder(flow: FlowResponse): string[] {
  const seen: string[] = [];
  for (const l of flow.lanes) if (!seen.includes(l.id)) seen.push(l.id);
  for (const n of flow.nodes) if (!seen.includes(n.lane)) seen.push(n.lane);
  return seen;
}

/** Same cards, same widths and same edges give the same key: a re-index that only changes numbers keeps the layout. */
export function flowKey(flow: FlowResponse, sizes: Map<string, FlowCardSize>): string {
  const nodes = flow.nodes.map((n) => `${n.id}@${n.lane}=${sizes.get(n.id)?.w ?? 0}x${sizes.get(n.id)?.h ?? 0}`).sort();
  const edges = flow.edges.map((e) => edgeKey(e.src, e.dst)).sort();
  return `${nodes.join("|")}#${edges.join("|")}`;
}

/** Spacing of the drawing, in layout px. */
export const SWIMLANES: SwimlaneOptions = {
  rowGap: 8,
  channelMin: 32,
  channelPadLeft: 12,
  channelPadRight: 18,
  track: 7,
  laneGap: 8,
  lanePad: LANE_PAD,
  portGap: 7,
  // The stage between the rail and the panel on a laptop (966×619 at 1534×711).
  aspect: 1.55,
};

export interface FlowLayout extends PlanLayout {
  /** Lane rectangles by lane (not lane id). */
  lanes: Map<string, Rect>;
}

export function layoutFlow(flow: FlowResponse, m: FlowMeasures = ROUGH): FlowLayout {
  const sizes = cardSizes(flow, m);
  const key = flowKey(flow, sizes);
  const order = laneOrder(flow);
  const out = swimlanes(
    flow.nodes.map((n) => ({ id: n.id, lane: n.lane, w: sizes.get(n.id)?.w ?? FLOW_CARD.minW, h: sizes.get(n.id)?.h ?? FLOW_CARD.h })),
    flow.edges.map((e) => ({ id: edgeKey(e.src, e.dst), src: e.src, dst: e.dst })),
    flow.root,
    order,
    { ...SWIMLANES, laneMinW: new Map(order.map((l) => [l, laneTitleW(l, m)])) },
  );
  const boxes = new Map<string, Rect>();
  for (const [l, r] of out.lanes) boxes.set(laneId(l), r);
  return { key, width: out.width, height: out.height, cards: out.cards, boxes, routes: out.routes, loose: null, lanes: out.lanes };
}

/** An edge into a file with an error or warning finding: drawn magenta. */
export function edgeAlert(flow: FlowResponse, e: FlowEdgeData): boolean {
  return flow.nodes.some((n) => n.id === e.dst && n.finding !== undefined);
}

/** Calls into files with findings, for the header ("2 llamadas a archivos con hallazgos"). */
export function alertCount(flow: FlowResponse): number {
  return flow.edges.filter((e) => edgeAlert(flow, e)).length;
}

/** Entry groups of the left rail, in a fixed order (commands, routes, main, Godot). */
export const ENTRY_GROUPS: { kinds: EntryKind[]; title: string }[] = [
  { kinds: ["cli"], title: "Comandos" },
  { kinds: ["http"], title: "HTTP" },
  { kinds: ["main"], title: "main" },
  { kinds: ["godot_main_scene", "godot_autoload"], title: "Godot" },
];

/** The rail's groups, with the entries matching `filter` (name or file, case-insensitive). */
export function groupEntries(entries: readonly FlowEntry[], filter = ""): { title: string; items: FlowEntry[] }[] {
  const q = filter.trim().toLowerCase();
  const hit = (e: FlowEntry) => !q || e.name.toLowerCase().includes(q) || e.file.toLowerCase().includes(q);
  return ENTRY_GROUPS.map((g) => ({ title: g.title, items: entries.filter((e) => g.kinds.includes(e.kind) && hit(e)) })).filter((g) => g.items.length > 0);
}

/** The tag before an entry in the rail: `CLI`, the HTTP method, `main`, `escena`, `autoload`. */
export function entryTag(e: FlowEntry): string {
  switch (e.kind) {
    case "cli":
      return "CLI";
    case "http": {
      const m = /^([A-Z]+)\s/.exec(e.name);
      return m ? m[1]! : "HTTP";
    }
    case "main":
      return "main";
    case "godot_main_scene":
      return "escena";
    case "godot_autoload":
      return "autoload";
  }
}

/** The entry's name without the method a tag already shows (`GET /api/tree` → `/api/tree`). */
export function entryLabel(e: FlowEntry): string {
  if (e.kind === "http") return e.name.replace(/^[A-Z]+\s+/, "");
  if (e.kind === "main" && e.name === "main") return e.file.split("/").slice(-2).join("/");
  return e.name;
}

/** `main.rs:37`: the file name and line of an entry. */
export function entryWhere(e: FlowEntry): string {
  const name = e.file.slice(e.file.lastIndexOf("/") + 1);
  return e.line > 0 ? `${name}:${e.line}` : name;
}

/** The title of a flow: "cartograph index" for a command (repo name + command), else the entry or function name. */
export function flowTitle(entry: FlowEntry | null, repo: string, fallback: string): string {
  if (!entry) return fallback;
  if (entry.kind === "cli") return repo ? `${repo} ${entry.name}` : entry.name;
  return entry.name;
}

/**
 * The function the flow fades around: the selection, unless it is the start (every flow opens
 * with its start selected, and fading around it would fade most of the flow on arrival).
 */
export function fadeFocus(flow: FlowResponse, selected: string | null): string | null {
  return selected !== null && selected !== flow.root ? selected : null;
}

/** Ids of the nodes linked to `id` by an edge (both ways). */
export function neighborsOf(flow: FlowResponse, id: string): Set<string> {
  const out = new Set<string>();
  for (const e of flow.edges) {
    if (e.src === id) out.add(e.dst);
    if (e.dst === id) out.add(e.src);
  }
  return out;
}
