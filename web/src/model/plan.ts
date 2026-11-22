// The layered plan ("Plano por capas"): which cards, boxes and edges are drawn for a graph
// view, and how they are laid out with ELK. Every open folder is a box around its children,
// so boxes nest. Everything here is pure; the ELK instance is injected (a Web Worker in the
// app, the bundled build in tests).
//
// Two drawing modes, chosen by size:
// - "cards" (up to CARDS_MODE_MAX cards): every edge goes card to card, crossing box borders,
//   in one ELK run over the whole hierarchy. This is the reference look.
// - "nested" (bigger views, like "Todo"): each box is laid out on its own, and an edge is drawn
//   between the two children of the innermost box that holds both ends (a card or a box). On
//   a view with hundreds of cards this keeps the plan a few screens big instead of dozens.
// Direction ("contra la corriente") is decided the same way in both: box by box (flow.ts).
import type { ElkExtendedEdge, ElkNode } from "elkjs/lib/elk-api";
import type { ConfidenceCounts, GraphResponse } from "../api/types";
import { cycles, edgeKey, liftEdge, ROOT, upstreamEdges } from "./flow";

export type PlanMode = "cards" | "nested";

export interface PlanNode {
  id: string;
  kind: "dir" | "file" | "ext";
  name: string;
  path: string;
  loc: number;
  files: number;
  langs: Record<string, number>;
  lang?: string;
  unresolved: number;
  expandable: boolean;
  /** The open box that holds it (null: the plan itself). */
  box: string | null;
  /** Nothing in this view imports it and it imports nothing (drawn faded, on request). */
  isolated: boolean;
  /** External packages: ecosystem, and for an "otras N" card how many packages it stands for. */
  eco?: string;
  others?: string[];
}

export interface PlanBox {
  id: string;
  name: string;
  path: string;
  /** The box that holds it (null: the plan itself). */
  box: string | null;
}

export interface PlanEdge {
  key: string;
  src: string;
  dst: string;
  weight: number;
  confidence: ConfidenceCounts;
  /** Drawn pointing up, in magenta: the lightest way to break the cycles of its box. */
  upstream: boolean;
  /** Mostly heuristic resolution: drawn dashed. */
  heuristic: boolean;
  /** To an external package card. */
  external: boolean;
  /** The box whose layout routes it (null: the plan itself). */
  container: string | null;
}

export interface Plan {
  mode: PlanMode;
  /** Cards on the plan (folders, files and, with libraries on, packages). */
  nodes: PlanNode[];
  /** Open folders drawn as boxes, outermost first. */
  boxes: PlanBox[];
  /** Drawn edges: card to card ("cards"), or between the children of a box ("nested"). */
  edges: PlanEdge[];
  /** Every dependency between visible cards, drawn or not (never to a package). */
  allEdges: PlanEdge[];
  /** Card-to-package edges, drawn or not. */
  externalEdges: PlanEdge[];
  /** Drawn edges left out because their box is dense (never an upstream one). */
  hidden: number;
  /** Lightest weight drawn when light edges are hidden (the plan itself, "cards" mode). */
  minWeight: number;
  /** Cards whose every edge is hidden ("cards" mode): they come back with the hidden edges. */
  lightOnly: PlanNode[];
  /** Cards with no dependency in this view, left off the plan unless `showIsolated`. */
  isolated: PlanNode[];
  /** Number of cards with no dependency in this view, drawn or not. */
  isolatedCount: number;
  /** Cycles among the visible cards (strongly connected components). */
  cycles: string[][];
  /** Cards touching an upstream edge. */
  onCycle: Set<string>;
  heaviest: PlanEdge | null;
  card: CardSize;
  /** Layout identity: mode, boxes, cards, drawn edges with their direction, card size. Weights are not part of it. */
  key: string;
}

export interface PlanOptions {
  showAllEdges?: boolean;
  showIsolated?: boolean;
  showExternals?: boolean;
  /** Above this many drawn edges in a box, the lightest ones are hidden. */
  maxEdges?: number;
  /** Above this many connected cards in a box, its edge budget shrinks to 3/4. */
  maxNodes?: number;
  /** Up to this many cards the plan is drawn card to card. */
  cardsModeMax?: number;
  /** Packages shown per ecosystem; the rest go into one "otras N" card. */
  perEco?: number;
}

export interface CardSize {
  w: number;
  h: number;
}

/** One card size: a name and a size bar. Text inside the plan is 14 px. */
export const CARD: CardSize = { w: 160, h: 40 };
/** Horizontal gap between two cards of a layer or of the loose grid (elk.spacing.nodeNode). */
export const CARD_GAP = 18;
export const CARDS_MODE_MAX = 40;
export const PLAN_FONT = 14;

/** Edge width by weight (file pairs), capped. */
export function strokeFor(weight: number): number {
  return Math.min(6, 1 + 0.9 * Math.log2(Math.max(1, weight)));
}

const byId = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);

/** Parent folder from an id alone ("dir:a/b" → "dir:a"; top level → null). */
export function parentOf(id: string): string | null {
  if (id.startsWith("ext:") || id.startsWith("extmore:")) return null;
  const path = id.slice(id.indexOf(":") + 1);
  const i = path.lastIndexOf("/");
  return i < 0 ? null : `dir:${path.slice(0, i)}`;
}

/** The open boxes holding `id`, outermost first, then `id`. */
export function chainOf(id: string): string[] {
  const out = [id];
  for (let p = parentOf(id); p !== null; p = parentOf(p)) out.push(p);
  return out.reverse();
}

function nameFromId(id: string): string {
  const p = id.slice(id.indexOf(":") + 1);
  return p.slice(p.lastIndexOf("/") + 1);
}

/** Ecosystem and package name of an external id ("ext:npm:react"). */
export function extParts(id: string): { eco: string; name: string } {
  const rest = id.replace(/^ext:/, "");
  const i = rest.indexOf(":");
  return i < 0 ? { eco: "", name: rest } : { eco: rest.slice(0, i), name: rest.slice(i + 1) };
}

/**
 * The density rule for one group of edges: above the budget, hide the lightest weight
 * classes (never an upstream edge), but always keep the heaviest class. Returns the lightest
 * weight that stays.
 */
function densityFloor(edges: readonly PlanEdge[], linked: number, maxEdges: number, maxNodes: number): number {
  const budget = linked > maxNodes ? Math.round(maxEdges * 0.75) : maxEdges;
  if (edges.length <= budget) return 1;
  const weights = [...new Set(edges.filter((e) => !e.upstream).map((e) => e.weight))].sort((a, b) => a - b);
  let min = 1;
  for (const w of weights) {
    min = w + 1;
    if (edges.filter((e) => e.upstream || e.weight >= min).length <= budget) break;
  }
  return Math.min(min, weights[weights.length - 1] ?? 1);
}

export function buildPlan(graph: GraphResponse, opts: PlanOptions = {}): Plan {
  const { showAllEdges = false, showIsolated = false, showExternals = false, maxEdges = 30, maxNodes = 25, cardsModeMax = CARDS_MODE_MAX, perEco = 4 } = opts;
  const all = [...graph.nodes].sort((a, b) => byId(a.id, b.id));
  const ids = new Set(all.map((n) => n.id));
  const rawEdges = graph.edges
    .filter((e) => e.src !== e.dst && ids.has(e.src) && ids.has(e.dst))
    .sort((a, b) => byId(a.src, b.src) || byId(a.dst, b.dst));

  // External packages: the most used per ecosystem as cards, the rest in one "otras N" card.
  const extCards: PlanNode[] = [];
  const extCardOf = new Map<string, string>();
  if (showExternals) {
    const use = new Map<string, number>();
    for (const x of graph.externals) if (ids.has(x.src)) use.set(x.ext, (use.get(x.ext) ?? 0) + x.weight);
    const ecos = new Map<string, string[]>();
    for (const ext of [...use.keys()].sort(byId)) {
      const { eco } = extParts(ext);
      if (!ecos.has(eco)) ecos.set(eco, []);
      ecos.get(eco)!.push(ext);
    }
    for (const [eco, list] of [...ecos].sort((a, b) => byId(a[0], b[0]))) {
      const ranked = [...list].sort((a, b) => use.get(b)! - use.get(a)! || byId(a, b));
      const keep = ranked.length > perEco + 1 ? ranked.slice(0, perEco) : ranked;
      const rest = ranked.slice(keep.length);
      const base = { kind: "ext" as const, loc: 0, files: 0, langs: {}, unresolved: 0, expandable: false, box: null, isolated: false, eco };
      for (const ext of keep) {
        extCardOf.set(ext, ext);
        extCards.push({ ...base, id: ext, name: extParts(ext).name, path: ext.slice(4) });
      }
      if (rest.length > 0) {
        const id = `extmore:${eco}`;
        for (const ext of rest) extCardOf.set(ext, id);
        extCards.push({ ...base, id, name: `otras ${rest.length}`, path: `${eco}: ${rest.length} paquetes más`, others: rest.map((x) => extParts(x).name) });
      }
    }
  }
  const extPairs = new Map<string, { src: string; dst: string; weight: number }>();
  for (const x of showExternals ? graph.externals : []) {
    const card = extCardOf.get(x.ext);
    if (!card || !ids.has(x.src)) continue;
    const k = edgeKey(x.src, card);
    const p = extPairs.get(k);
    if (p) p.weight += x.weight;
    else extPairs.set(k, { src: x.src, dst: card, weight: x.weight });
  }

  const linked = new Set<string>();
  for (const e of rawEdges) {
    linked.add(e.src);
    linked.add(e.dst);
  }
  const withExt = new Set(linked);
  for (const p of extPairs.values()) withExt.add(p.src);
  const isolatedNodes = all.filter((n) => !withExt.has(n.id));
  const shown = all.filter((n) => showIsolated || withExt.has(n.id));
  const shownIds = new Set(shown.map((n) => n.id));

  const up = upstreamEdges(rawEdges, chainOf);
  const allEdges: PlanEdge[] = rawEdges.map((e) => ({
    key: edgeKey(e.src, e.dst),
    src: e.src,
    dst: e.dst,
    weight: e.weight,
    confidence: e.confidence,
    upstream: up.has(edgeKey(e.src, e.dst)),
    heuristic: e.confidence.heuristic * 2 > e.weight,
    external: false,
    container: null,
  }));
  const onCycle = new Set(allEdges.filter((e) => e.upstream).flatMap((e) => [e.src, e.dst]));
  const extEdgeOf = (src: string, dst: string, weight: number): PlanEdge => ({
    key: edgeKey(src, dst),
    src,
    dst,
    weight,
    confidence: { exact: 0, resolved: weight, heuristic: 0 },
    upstream: false,
    heuristic: false,
    external: true,
    container: null,
  });
  const externalEdges = [...extPairs.values()].sort((a, b) => byId(a.src, b.src) || byId(a.dst, b.dst)).map((p) => extEdgeOf(p.src, p.dst, p.weight));

  // Decided on connected cards only, so drawing the unconnected ones doesn't change the mode.
  const mode: PlanMode = shown.filter((n) => withExt.has(n.id)).length <= cardsModeMax ? "cards" : "nested";
  let drawn: PlanEdge[];
  let hidden = 0;
  let minWeight = 1;
  let onPlan = shown;
  const lightOnly: typeof shown = [];

  if (mode === "cards") {
    // One budget for the whole plan; a card whose every edge is hidden goes with them.
    minWeight = showAllEdges ? 1 : densityFloor(allEdges, linked.size, maxEdges, maxNodes);
    drawn = allEdges.filter((e) => e.upstream || e.weight >= minWeight);
    hidden = allEdges.length - drawn.length;
    const ends = new Set(drawn.flatMap((e) => [e.src, e.dst]));
    for (const p of extPairs.values()) ends.add(p.src);
    onPlan = shown.filter((n) => ends.has(n.id) || !withExt.has(n.id));
    lightOnly.push(...shown.filter((n) => withExt.has(n.id) && !ends.has(n.id)));
  } else {
    // Each box keeps its own budget over the edges lifted onto its children.
    const lifted = new Map<string, PlanEdge>();
    for (const e of allEdges) {
      const l = liftEdge(chainOf, e.src, e.dst);
      const k = edgeKey(l.src, l.dst);
      const prev = lifted.get(k);
      if (prev) {
        prev.weight += e.weight;
        prev.confidence = {
          exact: prev.confidence.exact + e.confidence.exact,
          resolved: prev.confidence.resolved + e.confidence.resolved,
          heuristic: prev.confidence.heuristic + e.confidence.heuristic,
        };
        prev.heuristic = prev.confidence.heuristic * 2 > prev.weight;
      } else {
        lifted.set(k, { ...e, key: k, src: l.src, dst: l.dst, container: l.container === ROOT ? null : l.container });
      }
    }
    const byBox = new Map<string | null, PlanEdge[]>();
    for (const e of lifted.values()) {
      if (!byBox.has(e.container)) byBox.set(e.container, []);
      byBox.get(e.container)!.push(e);
    }
    drawn = [];
    for (const list of byBox.values()) {
      const ends = new Set(list.flatMap((e) => [e.src, e.dst]));
      const floor = showAllEdges ? 1 : densityFloor(list, ends.size, maxEdges, maxNodes);
      for (const e of list) {
        if (e.upstream || e.weight >= floor) drawn.push(e);
        else hidden += 1;
      }
    }
  }

  // Package edges: from the card ("cards") or from its top-level box ("nested"), lightest hidden past a budget.
  if (showExternals) {
    const onPlanIds = new Set(onPlan.map((n) => n.id));
    const pairs = new Map<string, PlanEdge>();
    for (const e of externalEdges) {
      if (!onPlanIds.has(e.src)) continue;
      const src = mode === "cards" ? e.src : chainOf(e.src)[0]!;
      const k = edgeKey(src, e.dst);
      const prev = pairs.get(k);
      if (prev) prev.weight += e.weight;
      else pairs.set(k, extEdgeOf(src, e.dst, e.weight));
    }
    const list = [...pairs.values()].sort((a, b) => b.weight - a.weight || byId(a.key, b.key));
    const budget = showAllEdges ? Infinity : Math.max(8, Math.ceil(1.5 * extCards.length));
    // Every package card keeps its heaviest edge, so none floats alone.
    const kept = new Set<string>();
    const seen = new Set<string>();
    for (const e of list) {
      if (!seen.has(e.dst)) {
        seen.add(e.dst);
        kept.add(e.key);
      }
    }
    for (const e of list) if (kept.size < budget) kept.add(e.key);
    for (const e of list) {
      if (kept.has(e.key)) drawn.push(e);
      else hidden += 1;
    }
    if (mode === "cards") {
      // A card whose only edges were package edges, all hidden, goes with the hidden edges too.
      const ends = new Set(drawn.flatMap((e) => [e.src, e.dst]));
      const gone = onPlan.filter((n) => withExt.has(n.id) && !ends.has(n.id));
      lightOnly.push(...gone);
      onPlan = onPlan.filter((n) => !gone.includes(n));
    }
  }
  drawn.sort((a, b) => byId(a.key, b.key));

  const toNode = (n: GraphResponse["nodes"][number]): PlanNode => {
    const parent = n.parent && n.parent !== "repo:" ? n.parent : null;
    return {
      id: n.id,
      kind: n.kind,
      name: n.name,
      path: n.path,
      loc: n.loc,
      files: n.files,
      langs: n.langs ?? {},
      ...(n.lang ? { lang: n.lang } : {}),
      unresolved: n.unresolved,
      expandable: n.expandable,
      box: parent,
      isolated: !withExt.has(n.id),
    };
  };
  const nodes = [...onPlan.map(toNode), ...extCards.filter((c) => drawn.some((e) => e.dst === c.id))];
  const boxIds = new Set<string>();
  for (const n of nodes) for (const b of chainOf(n.id).slice(0, -1)) boxIds.add(b);
  const boxes: PlanBox[] = [...boxIds]
    .sort((a, b) => chainOf(a).length - chainOf(b).length || byId(a, b))
    .map((id) => ({ id, name: nameFromId(id), path: id.slice(4), box: parentOf(id) }));

  const heaviest = drawn.filter((e) => !e.external).reduce<PlanEdge | null>((m, e) => (m === null || e.weight > m.weight ? e : m), null);
  const key = JSON.stringify([
    mode,
    boxes.map((b) => b.id),
    nodes.map((n) => n.id),
    // Routes are stored by edge key, so the key holds each edge's identity and its direction:
    // a flipped import (upstream a→b vs plain b→a) is drawn the same way but needs its own route.
    drawn.map((e) => `${e.src}>${e.dst}|${e.upstream ? 1 : 0}`).sort(),
    CARD.w,
  ]);
  return {
    mode,
    nodes,
    boxes,
    edges: drawn,
    allEdges,
    externalEdges,
    hidden,
    minWeight,
    lightOnly: lightOnly.map(toNode),
    isolated: showIsolated ? [] : isolatedNodes.map(toNode),
    isolatedCount: isolatedNodes.length,
    cycles: cycles([...shownIds], rawEdges),
    onCycle,
    heaviest,
    card: CARD,
    key,
  };
}

export const BOX_PAD = { top: 40, side: 16, bottom: 16 };

const LAYERED: Record<string, string> = {
  "elk.algorithm": "layered",
  "elk.direction": "DOWN",
  "elk.edgeRouting": "ORTHOGONAL",
  "elk.layered.spacing.nodeNodeBetweenLayers": "46",
  "elk.spacing.nodeNode": String(CARD_GAP),
  "elk.layered.spacing.edgeNodeBetweenLayers": "14",
  "elk.spacing.edgeEdge": "7",
  "elk.layered.spacing.edgeEdgeBetweenLayers": "7",
  "elk.spacing.componentComponent": "28",
  "elk.layered.nodePlacement.strategy": "NETWORK_SIMPLEX",
  "elk.layered.considerModelOrder.strategy": "NODES_AND_EDGES",
  "elk.layered.thoroughness": "30",
  "elk.randomSeed": "1",
};

/** ELK input for a plan. Children and edges are sorted by id, so equal plans give equal layouts. */
export function elkGraph(plan: Plan, extra: Record<string, string> = {}): ElkNode {
  const nested = plan.mode === "nested";
  const root: ElkNode = {
    id: "root",
    layoutOptions: {
      ...LAYERED,
      "elk.hierarchyHandling": nested ? "SEPARATE_CHILDREN" : "INCLUDE_CHILDREN",
      "elk.padding": "[top=0,left=0,bottom=0,right=0]",
      ...extra,
    },
    children: [],
    edges: [],
  };
  const elkBoxes = new Map<string, ElkNode>();
  const holder = (box: string | null): ElkNode => (box === null ? root : elkBoxes.get(box)!);
  for (const b of plan.boxes) {
    const node: ElkNode = {
      id: b.id,
      layoutOptions: {
        ...(nested ? { ...LAYERED, ...extra } : {}),
        "elk.padding": `[top=${BOX_PAD.top},left=${BOX_PAD.side},bottom=${BOX_PAD.bottom},right=${BOX_PAD.side}]`,
      },
      children: [],
      edges: [],
    };
    elkBoxes.set(b.id, node);
    holder(b.box).children!.push(node);
  }
  const skip = new Set(looseCards(plan).map((n) => n.id));
  for (const n of plan.nodes) {
    if (skip.has(n.id)) continue;
    const card: ElkNode = { id: n.id, width: plan.card.w, height: plan.card.h };
    // Packages sit in the bottom layer of the plan.
    if (n.kind === "ext") card.layoutOptions = { "elk.layered.layering.layerConstraint": "LAST" };
    holder(n.box).children!.push(card);
  }
  for (const e of plan.edges) {
    const edge: ElkExtendedEdge = {
      id: e.key,
      // Upstream edges go to ELK reversed, so the layers point down.
      sources: [e.upstream ? e.dst : e.src],
      targets: [e.upstream ? e.src : e.dst],
      layoutOptions: { "elk.edge.thickness": String(strokeFor(e.weight)) },
    };
    // Card to card: ELK routes every edge across the hierarchy from the root. Nested: each box routes its own.
    (nested ? holder(e.container) : root).edges!.push(edge);
  }
  const sortAll = (n: ElkNode) => {
    n.children?.sort((a, b) => byId(a.id, b.id));
    n.edges?.sort((a, b) => byId(a.id, b.id));
    for (const c of n.children ?? []) sortAll(c);
  };
  sortAll(root);
  return root;
}

export interface Point {
  x: number;
  y: number;
}

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface PlanLayout {
  key: string;
  width: number;
  height: number;
  cards: Map<string, Rect>;
  boxes: Map<string, Rect>;
  /** Polyline per edge key, in the direction ELK drew it (reversed for upstream edges). */
  routes: Map<string, Point[]>;
  /** Where the cards without any edge are packed, under the layered drawing (null if none). */
  loose: Rect | null;
}

/**
 * Cards on the plan itself with no drawn edge, in "cards" mode (only when the user asks to
 * draw unconnected cards). ELK would put them all in one row, so they are packed in a grid
 * under the plan. In "nested" mode ELK packs each box's unconnected children itself.
 */
export function looseCards(plan: Plan): PlanNode[] {
  if (plan.mode === "nested") return [];
  const ends = new Set(plan.edges.flatMap((e) => [e.src, e.dst]));
  return plan.nodes.filter((n) => n.box === null && !ends.has(n.id));
}

/** Gaps of the loose grid. Rows sit (40 + 20)·0.3 = 18 px apart at the zoom limit: a 16 px name and 2 px of air. */
export const LOOSE_GAP = { x: CARD_GAP, y: 20, top: 44 };

/** Packs loose cards under the layered drawing, choosing the column count that best fits a landscape stage. */
export function packLoose(layout: PlanLayout, loose: PlanNode[], card: CardSize, aspect = 1.5): PlanLayout {
  if (loose.length === 0) return layout;
  const n = loose.length;
  const cw = card.w + LOOSE_GAP.x;
  const ch = card.h + LOOSE_GAP.y;
  const top = layout.height > 0 ? layout.height + LOOSE_GAP.top : 0;
  let best = { cols: 1, score: -Infinity };
  for (let cols = 1; cols <= n; cols++) {
    const rows = Math.ceil(n / cols);
    const w = Math.max(layout.width, cols * cw - LOOSE_GAP.x);
    const h = top + rows * ch - LOOSE_GAP.y;
    const score = Math.min(aspect / w, 1 / h);
    if (score > best.score) best = { cols, score };
  }
  const cards = new Map(layout.cards);
  const sorted = [...loose].sort((a, b) => byId(a.id, b.id));
  sorted.forEach((node, i) => {
    cards.set(node.id, { x: (i % best.cols) * cw, y: top + Math.floor(i / best.cols) * ch, w: card.w, h: card.h });
  });
  const rows = Math.ceil(n / best.cols);
  const gridW = best.cols * cw - LOOSE_GAP.x;
  const gridH = rows * ch - LOOSE_GAP.y;
  return {
    ...layout,
    cards,
    width: Math.max(layout.width, gridW),
    height: top + gridH,
    loose: { x: 0, y: top, w: gridW, h: gridH },
  };
}

/** Absolute positions from ELK output (child and edge coordinates are relative to their container). */
export function readLayout(plan: Plan, out: ElkNode): PlanLayout {
  const abs = new Map<string, Rect>();
  abs.set("root", { x: 0, y: 0, w: out.width ?? 0, h: out.height ?? 0 });
  const owners: { n: ElkNode; id: string }[] = [{ n: out, id: "root" }];
  const walk = (n: ElkNode, ox: number, oy: number) => {
    for (const c of n.children ?? []) {
      const x = ox + (c.x ?? 0);
      const y = oy + (c.y ?? 0);
      abs.set(c.id, { x, y, w: c.width ?? 0, h: c.height ?? 0 });
      owners.push({ n: c, id: c.id });
      walk(c, x, y);
    }
  };
  walk(out, 0, 0);
  const cards = new Map<string, Rect>();
  for (const n of plan.nodes) {
    const r = abs.get(n.id);
    if (r) cards.set(n.id, r);
  }
  const boxes = new Map<string, Rect>();
  for (const b of plan.boxes) {
    const r = abs.get(b.id);
    if (r) boxes.set(b.id, r);
  }
  const routes = new Map<string, Point[]>();
  for (const { n, id } of owners) {
    for (const e of (n.edges ?? []) as ElkExtendedEdge[]) {
      const off = abs.get(e.container ?? id) ?? { x: 0, y: 0 };
      const pts: Point[] = [];
      for (const s of e.sections ?? []) {
        for (const p of [s.startPoint, ...(s.bendPoints ?? []), s.endPoint]) {
          const q = { x: p.x + off.x, y: p.y + off.y };
          const last = pts[pts.length - 1];
          if (!last || Math.abs(last.x - q.x) > 0.01 || Math.abs(last.y - q.y) > 0.01) pts.push(q);
        }
      }
      if (pts.length >= 2) routes.set(e.id, pts);
    }
  }
  return { key: plan.key, width: out.width ?? 0, height: out.height ?? 0, cards, boxes, routes, loose: null };
}

/** The part of ELK the plan needs (the worker-backed `elk-api` and the bundled build both fit). */
export interface ElkLike {
  layout(graph: ElkNode): Promise<ElkNode>;
}

export async function layoutPlan(elk: ElkLike, plan: Plan, extra: Record<string, string> = {}): Promise<PlanLayout> {
  const loose = looseCards(plan);
  const empty: PlanLayout = { key: plan.key, width: 0, height: 0, cards: new Map(), boxes: new Map(), routes: new Map(), loose: null };
  const core = plan.nodes.length > loose.length ? readLayout(plan, await elk.layout(elkGraph(plan, extra))) : empty;
  return packLoose(core, loose, plan.card);
}

/**
 * Where the camera goes for `id`: its card, its box, or the card or box that contains it (a
 * node hidden as light-only or unconnected, a file inside a folder card). Null when nothing
 * on the plan holds it.
 */
export function flyTarget(layout: PlanLayout, id: string): Rect | null {
  for (let cur: string | null = id; cur !== null; cur = parentOf(cur)) {
    const r = layout.cards.get(cur) ?? layout.boxes.get(cur);
    if (r) return r;
  }
  return null;
}

/** Center of a card. */
export function center(r: Rect): Point {
  return { x: r.x + r.w / 2, y: r.y + r.h / 2 };
}
