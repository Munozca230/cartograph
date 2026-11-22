// The layered plan, drawn as SVG. Position encodes dependency direction: every edge points
// down except the magenta ones, which close a cycle. Open folders are boxes around their
// children. Pan with drag or arrows, zoom with the wheel, the toolbar or + and -; 0 fits.
//
// Readability floor: text inside the plan is 14 px, so under FLOOR it would read under 12 px.
// There the plan hides its own text and the names come back as 12 px labels (planLabels.ts).
// The plan never zooms out past K_MIN, where every card still has room for its name:
// a plan bigger than the stage at that scale is panned (a minimap shows where you are).
//
// Each card says what it is (model/kind.ts): a stripe, a glyph and a size bar in the color
// of its kind; a folder also has a tab, and a closed one a stack behind it.
import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { FOLDER_LABEL, KIND_LABEL, KIND_SHORT, KINDS, kindOf, type Kind, type Kinds } from "../model/kind";
import { chainOf, flyTarget, LOOSE_GAP, PLAN_FONT, strokeFor, type Plan, type PlanEdge, type PlanLayout, type PlanNode, type Rect } from "../model/plan";
import { controller } from "../state/controller";
import { findingLines, type FindingsOnPlan } from "../model/findings";
import { layoutFor, useFindings, useIssueCounts, useKinds, usePlan, useProblems } from "../state/plan";
import { store, useAppState } from "../state/store";
import type { ViewSpec } from "../model/view";
import { formatLoc, formatNumber, locUnit, pairs } from "./format";
import { Minimap } from "./Minimap";
import { KindGlyph } from "./kindIcons";
import { arrowPath, roundPath, tagSpot, truncate } from "./planGeometry";
import { fitEnd, K_MIN, keepsExtension, LABEL_FONT, onStage, placeLabels, textMeasure, type LabelCandidate, type PlacedLabel } from "./planLabels";
import { anchoredView, flyToward, flyView, pickAnchor, type View } from "./camera";

export type { View } from "./camera";

interface Drawn {
  plan: Plan;
  layout: PlanLayout;
  /** The view the plan's graph was fetched for (a pending fly waits for it). */
  view: ViewSpec | null;
}

interface Tip {
  title: string;
  lines: { text: string; alert?: boolean; kind?: Kind }[];
  x: number;
  y: number;
}

/** Lowest fitted scale: 14 px plan text renders at 12 px. */
export const FLOOR = 12 / PLAN_FONT;
const AXIS = 44; // room on the left for the direction axis
const MARGIN = 28;
const MARGIN_MIN = 10;
const K_MAX = 2;
/** Width of the detail drawer, which lies over the right edge of the stage. */
export const DRAWER_W = 300;
/** Longer than a double click: the plan never moves under a card being double-clicked. */
const SETTLE_MS = 500;

const KEY_HELP =
  "Plano de dependencias. Flechas: mover. Más y menos: acercar y alejar. Cero: encuadrar. Tab: recorrer las tarjetas; Intro selecciona y, si ya está seleccionada, abre o cierra la carpeta. Escape: quitar la selección.";

/**
 * The fitted view: the whole plan if it fits between the zoom limit and 1:1, else its top-left
 * corner at the limit. `w` is the stage width the drawer leaves free, as for the limit itself.
 */
export function fitView(layout: PlanLayout, w: number, h: number): View {
  const dw = layout.width + AXIS;
  const dh = layout.height;
  if (dw <= 0 || dh <= 0 || w <= 0 || h <= 0) return { k: 1, tx: AXIS, ty: MARGIN };
  // A plan that misses 1:1 by less than the margin is shown at 1:1 with a thinner margin.
  const tight = Math.min(1, (w - 2 * MARGIN_MIN) / dw, (h - 2 * MARGIN_MIN) / dh);
  const fit = tight === 1 ? 1 : Math.min(1, (w - 2 * MARGIN) / dw, (h - 2 * MARGIN) / dh);
  // Never under the limit the wheel and − stop at, or − right after a fit would zoom in.
  const k = Math.max(kMinFor(layout, w, h), fit);
  // Too big at K_MIN: start at the top-left corner, where the entries and the first box names are.
  const tx = dw * k <= w ? (w - dw * k) / 2 + AXIS * k : MARGIN_MIN + AXIS * k;
  const ty = dh * k <= h - 2 * MARGIN_MIN ? (h - dh * k) / 2 : MARGIN_MIN + 6;
  return { k, tx, ty };
}

/** The view that shows the whole plan, whatever the scale (minimap clicks, hand zoom limit). */
function overviewK(layout: PlanLayout, w: number, h: number): number {
  const dw = layout.width + AXIS;
  if (dw <= 0 || layout.height <= 0) return 1;
  return Math.min(1, (w - 2 * MARGIN_MIN) / dw, (h - 2 * MARGIN_MIN) / layout.height);
}

/** How far the plan zooms out: never past K_MIN, nor much past the whole plan when that is bigger. */
export function kMinFor(layout: PlanLayout, w: number, h: number): number {
  return Math.max(K_MIN, Math.min(FLOOR, overviewK(layout, w, h) * 0.9));
}

function zoomAt(v: View, factor: number, x: number, y: number, kMin: number): View {
  const k = Math.min(K_MAX, Math.max(kMin, v.k * factor));
  const f = k / v.k;
  return { k, tx: x - (x - v.tx) * f, ty: y - (y - v.ty) * f };
}

function pathOf(id: string): string {
  return id.slice(id.indexOf(":") + 1);
}

const BAR_X = 12;
/** Where a card's name starts: after the kind glyph. */
const NAME_X = 29;
const nameMeasure = textMeasure(PLAN_FONT);
const labelMeasure = textMeasure(LABEL_FONT);

/**
 * Size bar: length by lines (log scale against the biggest card of the view), colored by
 * kind. A folder's bar is split by the kinds of its files, its own kind first.
 */
function sizeBar(n: PlanNode, kind: Kind, kinds: Kinds | null, w: number, y: number, maxLoc: number) {
  const track = w - BAR_X - 12;
  const ratio = n.loc > 0 ? Math.max(0.06, Math.log10(1 + n.loc) / Math.log10(1 + Math.max(maxLoc, 1))) : 0;
  const mix = n.kind === "dir" ? kinds?.mix.get(n.id) : undefined;
  const entries: [Kind, number][] = mix ? KINDS.filter((k) => mix[k] > 0).map((k) => [k, mix[k]]) : [[kind, 1]];
  entries.sort((a, b) => (a[0] === kind ? -1 : b[0] === kind ? 1 : b[1] - a[1]));
  const total = entries.reduce((s, [, v]) => s + v, 0) || 1;
  let x = BAR_X;
  const segs = entries.map(([k, v]) => {
    const seg = (track * ratio * v) / total;
    const r = <rect key={k} x={x} y={y} width={Math.max(0, seg - (entries.length > 1 ? 1 : 0))} height={3} rx={1.5} style={{ fill: `var(--kind-${k})` }} />;
    x += seg;
    return r;
  });
  return (
    <g className="size">
      <rect className="track" x={BAR_X} y={y} width={track} height={3} rx={1.5} />
      {segs}
    </g>
  );
}

/** The kind stripe on the left edge of a card, following its rounded corners (rx 6). */
function stripePath(h: number): string {
  return `M3,0.8 A6,6 0 0 0 0,6 V${h - 6} A6,6 0 0 0 3,${h - 0.8} Z`;
}

/** A folder's tab, over the top-left of the card (its fill hides the card's top line there). */
const TAB = "M8,0.6 L10.6,-3.4 Q11,-4 11.8,-4 H40.2 Q41,-4 41.4,-3.4 L44,0.6";

interface Scene {
  edges: { edge: PlanEdge; d: string; head: string; width: number; cls: string }[];
  /** Cards linked to the selection in this view (true endpoints, even inside boxes). */
  neighbors: Set<string>;
  selected: string | null;
  inN: Map<string, number>;
  outN: Map<string, number>;
  tag: { x: number; y: number; w: number; text: string } | null;
  maxLoc: number;
}

/** Inside `x` (itself or a descendant), by id. */
const within = (id: string, x: string) => id === x || chainOf(id).includes(x);

/** Geometry and classes for one frame: edges sorted so alerts draw on top. */
function sceneOf({ plan, layout }: Drawn, chosen: string | null, found: FindingsOnPlan): Scene {
  // Only a card dims the rest: a selected box keeps the whole plan readable.
  const selected = chosen !== null && plan.nodes.some((n) => n.id === chosen) ? chosen : null;
  const neighbors = new Set<string>();
  const outs = new Set<string>();
  const ins = new Set<string>();
  const inN = new Map<string, number>();
  const outN = new Map<string, number>();
  for (const e of [...plan.allEdges, ...plan.externalEdges]) {
    outN.set(e.src, (outN.get(e.src) ?? 0) + 1);
    inN.set(e.dst, (inN.get(e.dst) ?? 0) + 1);
    if (e.src === selected) outs.add(e.dst);
    if (e.dst === selected) ins.add(e.src);
  }
  for (const id of [...outs, ...ins]) neighbors.add(id);
  const touches = (e: PlanEdge) =>
    selected !== null &&
    ((within(selected, e.src) && [...outs].some((o) => within(o, e.dst))) || (within(selected, e.dst) && [...ins].some((i) => within(i, e.src))));
  // Magenta: an edge against the flow (it closes a cycle) or one that proves a finding
  // (a broken layer rule, a cycle). Only the color: the layout never sees findings.
  const alert = (e: PlanEdge) => e.upstream || found.byEdge.has(e.key);
  const rank = (e: PlanEdge) => (alert(e) ? 3 : touches(e) ? 2 : 1);
  const edges: Scene["edges"] = [];
  for (const e of [...plan.edges].sort((a, b) => rank(a) - rank(b))) {
    const pts = layout.routes.get(e.key);
    if (!pts || pts.length < 2) continue;
    // ELK drew upstream edges reversed: the real target is where the route starts.
    const tip = e.upstream ? pts[0]! : pts[pts.length - 1]!;
    const from = e.upstream ? pts[1]! : pts[pts.length - 2]!;
    const hot = touches(e);
    edges.push({
      edge: e,
      d: roundPath(pts),
      head: arrowPath(tip, from),
      width: e.external ? Math.min(2.5, strokeFor(e.weight)) : strokeFor(e.weight),
      cls: alert(e) ? "alert" : hot ? "hot" : selected ? "dim" : e.external ? "ext" : "norm",
    });
  }
  let tag: Scene["tag"] = null;
  const heaviest = plan.heaviest && !plan.heaviest.upstream ? plan.heaviest : null;
  if (heaviest && plan.edges.length > 1) {
    const route = layout.routes.get(heaviest.key);
    const text = pairs(heaviest.weight);
    const w = text.length * 7.6 + 18;
    const avoid: Rect[] = [...layout.cards.values()].map((r) => ({ x: r.x - 4, y: r.y - 4, w: r.w + 8, h: r.h + 8 }));
    for (const b of layout.boxes.values()) avoid.push({ x: b.x, y: b.y, w: Math.min(b.w, 240), h: 34 });
    const spot = route ? tagSpot(route, w, strokeFor(heaviest.weight), avoid) : null;
    if (spot) tag = { ...spot, w, text };
  }
  const maxLoc = plan.nodes.reduce((m, n) => Math.max(m, n.kind === "ext" ? 0 : n.loc), 0);
  return { edges, neighbors, selected, inN, outN, tag, maxLoc };
}

interface ContentProps {
  drawn: Drawn;
  scene: Scene;
  selected: string | null;
  hovered: string | null;
  pulsing: Set<string>;
  problems: Map<string, number>;
  kinds: Kinds | null;
  /** Bumped when the plan font has loaded: names are cut by measuring them. */
  fonts: number;
  onTip: (e: React.PointerEvent, id: string | null, edge?: PlanEdge) => void;
}

/** Everything inside the camera transform. Memoized, so panning only moves one group. */
const PlanContent = memo(function PlanContent({ drawn, scene, selected, hovered, pulsing, problems, kinds, onTip }: ContentProps) {
  // `fonts` is a prop only to redraw once the font has loaded (names are measured in it).
  const { plan, layout } = drawn;
  const axisH = layout.loose ? layout.loose.y - LOOSE_GAP.top : layout.height;
  return (
    <>
      {layout.loose && (
        <text className="axis-t" x={0} y={layout.loose.y - 14}>
          Sin conexiones en esta vista
        </text>
      )}
      {axisH > 60 && (
        <g className="axis" aria-hidden="true">
          <path d={`M-30,6 V${axisH - 10}`} />
          <path d={`M-33.5,${axisH - 14} L-30,${axisH - 6} L-26.5,${axisH - 14}`} />
          <text className="axis-t" transform="translate(-44,6) rotate(90)">
            entradas
          </text>
          <text className="axis-t" textAnchor="end" transform={`translate(-44,${axisH - 18}) rotate(90)`}>
            base
          </text>
        </g>
      )}
      <g className="boxes">
        {plan.boxes.map((b) => {
          const r = layout.boxes.get(b.id);
          if (!r) return null;
          const bad = (problems.get(b.id) ?? 0) > 0;
          return (
            <g
              key={b.id}
              className={`ctr k-${kindOf(kinds, b.id, b.path, true)}${selected === b.id ? " sel" : ""}${hovered === b.id ? " hover" : ""}`}
              transform={`translate(${r.x},${r.y})`}
              data-id={b.id}
              data-box=""
            >
              <rect width={r.w} height={r.h} rx={10} />
              {/* The name strip is the box's handle: click selects, double click closes. */}
              <g
                className="ctr-head"
                tabIndex={0}
                role="button"
                aria-label={`Carpeta abierta ${b.path}`}
                onClick={() => controller.select(b.id, { reveal: false })}
                onDoubleClick={() => controller.activate(b.id)}
                onPointerMove={(e) => onTip(e, b.id)}
                onPointerLeave={(e) => onTip(e, null)}
                onKeyDown={(e) => {
                  if (e.key !== "Enter" && e.key !== " ") return;
                  e.preventDefault();
                  e.stopPropagation();
                  if (store.get().selected === b.id) controller.activate(b.id);
                  else controller.select(b.id, { reveal: false });
                }}
              >
                <rect className="ctr-hit" width={Math.min(r.w, 16 + b.name.length * 8.4 + 24)} height={34} rx={10} />
                <text x={14} y={24} className="cname">
                  {truncate(b.name, Math.max(6, Math.floor((r.w - 40) / 8.4)))}
                </text>
                {bad && <circle className="pdot" cx={Math.min(r.w - 12, 14 + Math.min(b.name.length, Math.floor((r.w - 40) / 8.4)) * 8.4 + 10)} cy={19} r={3.5} />}
              </g>
            </g>
          );
        })}
      </g>
      <g className="edges">
        {scene.edges.map((s) => (
          <g key={s.edge.key} className={`edge ${s.cls}`}>
            <path className="line" d={s.d} strokeWidth={s.width} strokeDasharray={s.edge.heuristic || s.edge.external ? "4 3" : undefined} />
            <path className="head" d={s.head} />
          </g>
        ))}
      </g>
      <g className="cards">
        {plan.nodes.map((n) => {
          const r = layout.cards.get(n.id);
          if (!r) return null;
          const isSel = n.id === selected;
          const far = scene.selected !== null && !isSel && !scene.neighbors.has(n.id);
          const ext = n.kind === "ext";
          const folder = n.kind === "dir" && n.expandable;
          const kind = ext ? null : kindOf(kinds, n.id, n.path, n.kind === "dir");
          const dot = (problems.get(n.id) ?? 0) > 0 || plan.onCycle.has(n.id);
          const label = ext
            ? truncate(n.name, Math.floor((r.w - 24 - (dot ? 12 : 0) - (n.eco ? n.eco.length * 8 + 8 : 0)) / 8.1))
            : fitEnd(n.name, r.w - NAME_X - 10 - (dot ? 12 : 0), nameMeasure);
          const cls = [
            "card",
            ext && "ext",
            kind && `k-${kind}`,
            n.isolated && "iso",
            isSel && "sel",
            far && "far",
            scene.selected !== null && scene.neighbors.has(n.id) && "nb",
            hovered === n.id && "hover",
            pulsing.has(n.id) && "pulse",
          ]
            .filter(Boolean)
            .join(" ");
          return (
            <g
              key={n.id}
              className={cls}
              transform={`translate(${r.x},${r.y})`}
              data-id={n.id}
              tabIndex={0}
              role="button"
              aria-pressed={isSel}
              aria-label={ext ? `Librería ${n.name}` : `${n.path}, ${formatNumber(n.loc)} líneas${dot ? ", con problemas" : ""}`}
              onClick={() => controller.select(n.id, { reveal: false })}
              onDoubleClick={() => controller.activate(n.id)}
              onPointerMove={(e) => onTip(e, n.id)}
              onPointerLeave={(e) => onTip(e, null)}
              onKeyDown={(e) => {
                if (e.key !== "Enter" && e.key !== " ") return;
                e.preventDefault();
                e.stopPropagation();
                if (store.get().selected === n.id) controller.activate(n.id);
                else controller.select(n.id, { reveal: false });
              }}
            >
              {/* A closed folder is a small stack: there is more inside. */}
              {folder && <rect className="stack" x={3} y={3} width={r.w} height={r.h} rx={6} />}
              <rect className="face" width={r.w} height={r.h} rx={ext ? r.h / 2 : 6} />
              {n.kind === "dir" && <path className="tab" d={TAB} />}
              {kind && <path className="stripe" d={stripePath(r.h)} />}
              {kind && <KindGlyph className="glyph" kind={kind} folder={n.kind === "dir"} transform="translate(10,8) scale(0.8125)" />}
              <text x={ext ? 16 : NAME_X} y={ext ? r.h / 2 + 5 : 20} className="cn">
                {label}
              </text>
              {ext && n.eco && (
                <text x={r.w - 14} y={r.h / 2 + 5} className="eco" textAnchor="end">
                  {n.eco}
                </text>
              )}
              {kind && sizeBar(n, kind, kinds, r.w, r.h - 10, scene.maxLoc)}
              {dot && <circle className="pdot" cx={r.w - 12} cy={ext ? r.h / 2 : 15} r={3.5} />}
            </g>
          );
        })}
      </g>
      {scene.tag && (
        <g className="tag" transform={`translate(${scene.tag.x},${scene.tag.y})`}>
          <rect width={scene.tag.w} height={22} rx={11} />
          <text x={scene.tag.w / 2} y={16} textAnchor="middle">
            {scene.tag.text}
          </text>
        </g>
      )}
      <g className="hits">
        {scene.edges.map((s) => (
          <path
            key={s.edge.key}
            className="hit"
            d={s.d}
            onPointerMove={(e) => onTip(e, null, s.edge)}
            onClick={() => controller.select(s.edge.src, { reveal: false })}
            onPointerLeave={(e) => onTip(e, null)}
          />
        ))}
      </g>
    </>
  );
});

/** The tooltip line that says what a card or box is; a folder of mixed kinds lists them. */
function kindLine(kinds: Kinds | null, id: string, path: string, isDir: boolean): Tip["lines"][number] {
  const kind = kindOf(kinds, id, path, isDir);
  if (!isDir) return { text: KIND_LABEL[kind], kind };
  const mix = kinds?.mix.get(id);
  const parts = mix ? KINDS.filter((k) => mix[k] > 0) : [];
  const detail = parts.length > 1 ? ` (${parts.map((k) => `${formatNumber(mix![k])} ${KIND_SHORT[k]}`).join(" · ")})` : "";
  return { text: `${FOLDER_LABEL[kind]}${detail}`, kind };
}

/**
 * Every name the plan can show as a label (planLabels.ts decides which show at a zoom, from
 * the zoom and the depth). A file's extension may go when its name is cut, unless a sibling
 * would then read the same (`index.ts`, `index.js`, `utils.ts` next to the folder `utils`).
 */
function labelCandidates({ plan, layout }: Drawn, kinds: Kinds | null): LabelCandidate[] {
  const out: LabelCandidate[] = [];
  plan.boxes.forEach((b, i) => {
    const r = layout.boxes.get(b.id);
    if (r) out.push({ id: b.id, text: b.name, type: "box", rect: r, priority: i, kind: kindOf(kinds, b.id, b.path, true), depth: chainOf(b.id).length });
  });
  const keepExt = keepsExtension(plan.nodes, plan.boxes);
  plan.nodes.forEach((n, i) => {
    const r = layout.cards.get(n.id);
    if (!r) return;
    const file = n.kind === "file";
    out.push({
      id: n.id,
      text: n.name,
      type: "card",
      rect: r,
      priority: 1000 + i,
      kind: n.kind === "ext" ? "ext" : kindOf(kinds, n.id, n.path, n.kind === "dir"),
      depth: n.kind === "ext" ? 1 : chainOf(n.id).length,
      file,
      keepExt: keepExt.has(n.id),
    });
  });
  return out;
}

/** Labels near a window of the scaled plan (px, before the pan). */
const LabelList = memo(function LabelList({ list, f, x0, y0, x1, y1 }: { list: PlacedLabel[]; f: number; x0: number; y0: number; x1: number; y1: number }) {
  return list.map((l) => {
    // Placed for another zoom: until the next placement, anchors follow the current one.
    const x = l.ax * f + l.dx;
    const y = l.ay * f + l.dy;
    if (!onStage({ ...l, x: x - x0, y: y - y0 }, 0, 0, x1 - x0, y1 - y0)) return null;
    return (
      <span key={`${l.type}|${l.id}`} data-id={l.id} className={`lbl ${l.type} k-${l.kind}`} style={{ left: Math.round(x), top: Math.round(y), width: l.w }}>
        {l.text}
      </span>
    );
  });
});

/** Over the plan, in screen px: the pan moves the whole layer, and only labels near the stage are drawn. */
function LabelLayer({ list, lk, view, stage }: { list: PlacedLabel[]; lk: number; view: View; stage: { w: number; h: number } }) {
  // The drawn window snaps to a coarse grid with a margin, so a pan re-renders the list rarely.
  const STEP = 256;
  const x0 = Math.floor(-view.tx / STEP) * STEP - STEP;
  const y0 = Math.floor(-view.ty / STEP) * STEP - STEP;
  const x1 = Math.ceil((stage.w - view.tx) / STEP) * STEP + STEP;
  const y1 = Math.ceil((stage.h - view.ty) / STEP) * STEP + STEP;
  return (
    <div className="labels" aria-hidden="true" style={{ transform: `translate(${view.tx}px, ${view.ty}px)` }}>
      <LabelList list={list} f={view.k / lk} x0={x0} y0={y0} x1={x1} y1={y1} />
    </div>
  );
}

export function PlanView() {
  const plan = usePlan();
  const problems = useProblems();
  const issueCounts = useIssueCounts();
  const found = useFindings();
  const kinds = useKinds();
  const selected = useAppState((s) => s.selected);
  const hovered = useAppState((s) => s.hovered);
  const pulse = useAppState((s) => s.pulse);
  const phase = useAppState((s) => s.phase);
  // The detail drawer covers the right of the stage; the plan works in what is left.
  const covered = useAppState((s) => (s.detail !== null ? DRAWER_W : 0));
  const coveredRef = useRef(covered);
  coveredRef.current = covered;
  const stageRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [drawn, setDrawn] = useState<Drawn | null>(null);
  const [view, setView] = useState<View>({ k: 1, tx: 0, ty: 0 });
  const [error, setError] = useState<string | null>(null);
  const [laying, setLaying] = useState(false);
  const [tip, setTip] = useState<Tip | null>(null);
  // A new drawing drops the tooltip: the card under it may be gone (no pointerleave fires then).
  const [tipFor, setTipFor] = useState<Drawn | null>(null);
  if (tipFor !== drawn) {
    setTipFor(drawn);
    if (tip) setTip(null);
  }
  const fittedKey = useRef<string | null>(null);
  const userMoved = useRef(false);
  const drag = useRef<{ x: number; y: number; tx: number; ty: number; moved: boolean } | null>(null);

  // Stage size.
  useLayoutEffect(() => {
    const el = stageRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setSize({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    setSize({ w: el.clientWidth, h: el.clientHeight });
    return () => ro.disconnect();
  }, []);

  // Layout: same key, same positions (new numbers draw on the old geometry right away).
  useEffect(() => {
    if (!plan) return;
    let live = true;
    if (drawn && drawn.layout.key === plan.key) {
      // A layout started for a plan in between may have been dropped: nothing is being laid out.
      setLaying(false);
      setError(null);
      if (drawn.plan !== plan) setDrawn({ plan, layout: drawn.layout, view: store.get().graphView });
      return;
    }
    setLaying(true);
    layoutFor(plan)
      .then((layout) => {
        if (!live) return;
        setError(null);
        setLaying(false);
        setDrawn({ plan, layout, view: store.get().graphView });
      })
      .catch((err: unknown) => {
        if (!live) return;
        setLaying(false);
        setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      // The layout in flight is dropped; the next run starts another one if it still needs it.
      live = false;
      setLaying(false);
    };
  }, [plan, drawn]);

  // The first drawing fits the plan into the part of the stage the drawer leaves free, and so
  // does a resize while the user hasn't moved it. Any later layout (a folder opened or closed,
  // another level, a layer toggled, a re-index) keeps the zoom and keeps one node where it was
  // on screen (camera.ts): the folder just toggled, else the selection if it was on screen,
  // else the card nearest the center. A fly that waited for this plan goes after that.
  // Opening or closing the drawer never refits.
  const viewRef = useRef(view);
  viewRef.current = view;
  const shownRef = useRef<Drawn | null>(null);
  useLayoutEffect(() => {
    if (!drawn || size.w === 0) return;
    const w = size.w - coveredRef.current;
    const prev = shownRef.current;
    const relayout = fittedKey.current !== drawn.layout.key;
    shownRef.current = drawn;
    const id = controller.takePendingFly(drawn.view);
    const hint = controller.takePendingAnchor(drawn.view);
    const flyTo = id ? flyTarget(drawn.layout, id) : null;
    let base: View | null = null;
    if (relayout && prev && fittedKey.current !== null) {
      const v = viewRef.current;
      const anchor = pickAnchor(prev.layout, v, w, size.h, hint, store.get().selected);
      base = anchor ? anchoredView(prev.layout, drawn.layout, v, anchor) : v;
      // A smaller plan zooms out less far: never below its own limit.
      const min = kMinFor(drawn.layout, w, size.h);
      if (base.k < min) base = zoomAt(base, min / base.k, w / 2, size.h / 2, min);
      userMoved.current = true;
    } else if (relayout || !userMoved.current) {
      base = fitView(drawn.layout, w, size.h);
      userMoved.current = false;
    }
    fittedKey.current = drawn.layout.key;
    if (base) {
      // As a fly asked for on the drawn plan: never below the floor, where names are plan text.
      const moved = flyTo ? flyToward(base, flyTo, w, size.h, FLOOR) : null;
      if (moved) userMoved.current = true;
      setView(moved ?? base);
    } else if (flyTo) {
      setView((v) => flyToward(v, flyTo, w, size.h, FLOOR) ?? v);
    }
  }, [drawn, size]);

  // When the drawer opens or closes, or another card is selected, once a double click had its
  // chance: an untouched plan re-fits into the part left free, and a selected card the drawer
  // covers slides into view. A new drawing alone never moves the plan here.
  const lastCovered = useRef(covered);
  const lastSelected = useRef(selected);
  useEffect(() => {
    if (!drawn || size.w === 0) return;
    const changed = lastCovered.current !== covered;
    const picked = lastSelected.current !== selected;
    const r = selected ? (drawn.layout.cards.get(selected) ?? null) : null;
    if (!changed && (!picked || !r || covered === 0)) {
      lastSelected.current = selected;
      return;
    }
    const t = window.setTimeout(() => {
      lastCovered.current = covered;
      lastSelected.current = selected;
      const w = size.w - covered;
      setView((v) => {
        const base = changed && !userMoved.current ? fitView(drawn.layout, w, size.h) : v;
        const moved = r ? flyView(base, r, w, size.h) : null;
        if (moved) userMoved.current = true;
        return moved ?? base;
      });
    }, SETTLE_MS);
    return () => window.clearTimeout(t);
  }, [drawn, selected, covered, size]);

  // The same free width as the fit (the drawer covers the right of the stage).
  const kMin = drawn ? kMinFor(drawn.layout, size.w - covered, size.h) : FLOOR;

  // Camera requests from the toolbar, the palette and the tree.
  useEffect(
    () =>
      controller.onCamera((req) => {
        const el = stageRef.current;
        if (!el || !drawn) return;
        const w = el.clientWidth - coveredRef.current;
        const h = el.clientHeight;
        if (req.type === "fit") {
          userMoved.current = false;
          setView(fitView(drawn.layout, w, h));
        } else if (req.type === "zoom") {
          userMoved.current = true;
          setView((v) => zoomAt(v, req.factor, w / 2, h / 2, kMin));
        } else if (req.type === "fly") {
          // Its card, or the card or box that holds it; nothing on the plan holds it: stay.
          const r = flyTarget(drawn.layout, req.id);
          if (!r) return;
          setView((v) => {
            const moved = flyToward(v, r, w, h, FLOOR);
            if (!moved) return v;
            userMoved.current = true;
            return moved;
          });
        }
      }),
    [drawn, kMin],
  );

  // Wheel zoom around the pointer (a native listener, so it can preventDefault).
  const kMinRef = useRef(kMin);
  kMinRef.current = kMin;
  useEffect(() => {
    const el = stageRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const rect = el.getBoundingClientRect();
      userMoved.current = true;
      setView((v) => zoomAt(v, Math.exp(-e.deltaY * 0.0015), e.clientX - rect.left, e.clientY - rect.top, kMinRef.current));
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, []);

  const scene = useMemo(() => (drawn ? sceneOf(drawn, selected, found) : null), [drawn, selected, found]);
  // The controller clears the pulse after a few seconds, so a card mounted later doesn't replay it.
  const pulsing = useMemo(() => new Set(pulse?.ids ?? []), [pulse]);

  const sceneRef = useRef(scene);
  sceneRef.current = scene;
  const drawnRef = useRef(drawn);
  drawnRef.current = drawn;
  const issuesRef = useRef(issueCounts);
  issuesRef.current = issueCounts;
  const foundRef = useRef(found);
  foundRef.current = found;
  const kindsRef = useRef(kinds);
  kindsRef.current = kinds;
  const onTip = useCallback((e: React.PointerEvent, id: string | null, edge?: PlanEdge) => {
    if (drag.current?.moved) return;
    const d = drawnRef.current;
    const sc = sceneRef.current;
    if (!d || !sc || (id === null && !edge)) {
      setTip(null);
      return;
    }
    if (edge) {
      const lines = [{ text: `${pairs(edge.weight)} de archivos` }];
      if (edge.upstream) lines.push({ text: "Contra la corriente: cierra un ciclo de esta vista", alert: true } as never);
      for (const l of findingLines(foundRef.current.byEdge.get(edge.key))) lines.push(l as never);
      if (edge.heuristic) lines.push({ text: "Resolución aproximada (heurística)" });
      if (edge.external) lines[0] = { text: `La usan ${formatNumber(edge.weight)} ${edge.weight === 1 ? "archivo" : "archivos"}` };
      setTip({ title: `${pathOf(edge.src) || edge.src} → ${edge.external ? (d.plan.nodes.find((n) => n.id === edge.dst)?.name ?? edge.dst) : pathOf(edge.dst)}`, lines, x: e.clientX, y: e.clientY });
      return;
    }
    const box = d.plan.boxes.find((b) => b.id === id);
    if (box) {
      const hn = store.get().hierarchy?.nodes.get(box.id);
      const lines: Tip["lines"] = [kindLine(kindsRef.current, box.id, box.path, true)];
      if (hn) lines.push({ text: `${formatNumber(hn.files)} archivos · ${formatLoc(hn.loc)} ${locUnit(hn.loc)}` });
      const bad = issuesRef.current.get(box.id) ?? 0;
      if (bad) lines.push({ text: `${formatNumber(bad)} ${bad === 1 ? "problema" : "problemas"} (imports sin resolver u omitidos)`, alert: true });
      lines.push(...findingLines(foundRef.current.byId.get(box.id)));
      lines.push({ text: "Doble clic: cerrar la carpeta" });
      setTip({ title: box.path, lines, x: e.clientX, y: e.clientY });
      return;
    }
    const n = d.plan.nodes.find((x) => x.id === id);
    if (!n) return setTip(null);
    const lines: Tip["lines"] = [];
    if (n.kind === "ext") {
      if (n.others) lines.push({ text: `${n.others.slice(0, 6).join(", ")}${n.others.length > 6 ? "…" : ""}` });
      lines.push({ text: `Librería ${n.eco}; la usan ${formatNumber(sc.inN.get(n.id) ?? 0)} elementos de esta vista` });
    } else {
      lines.push(kindLine(kindsRef.current, n.id, n.path, n.kind === "dir"));
      lines.push({ text: n.kind === "dir" ? `${formatNumber(n.files)} archivos · ${formatLoc(n.loc)} ${locUnit(n.loc)}` : `${formatLoc(n.loc)} ${locUnit(n.loc)}` });
      if (n.isolated) lines.push({ text: "Sin conexiones en esta vista" });
      else lines.push({ text: `Depende de ${formatNumber(sc.outN.get(n.id) ?? 0)} · La usan ${formatNumber(sc.inN.get(n.id) ?? 0)}` });
      const bad = issuesRef.current.get(n.id) ?? 0;
      if (bad) lines.push({ text: `${formatNumber(bad)} ${bad === 1 ? "problema" : "problemas"} (imports sin resolver u omitidos)`, alert: true });
      lines.push(...findingLines(foundRef.current.byId.get(n.id)));
      if (d.plan.onCycle.has(n.id)) lines.push({ text: "En un ciclo: tiene una conexión contra la corriente", alert: true });
      if (n.kind === "dir" && n.expandable) lines.push({ text: "Doble clic: abrir la carpeta" });
    }
    setTip({ title: n.kind === "ext" ? n.path : n.path, lines, x: e.clientX, y: e.clientY });
  }, []);

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    drag.current = { x: e.clientX, y: e.clientY, tx: view.tx, ty: view.ty, moved: false };
  };
  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d) return;
    const dx = e.clientX - d.x;
    const dy = e.clientY - d.y;
    if (!d.moved && Math.hypot(dx, dy) < 4) return;
    if (!d.moved) {
      d.moved = true;
      e.currentTarget.setPointerCapture(e.pointerId);
      setTip(null);
    }
    userMoved.current = true;
    setView((v) => ({ ...v, tx: d.tx + dx, ty: d.ty + dy }));
  };
  const onPointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    drag.current = null;
    if (d?.moved) {
      e.currentTarget.releasePointerCapture?.(e.pointerId);
      return;
    }
    // A click on the background clears the selection.
    if (!(e.target as Element).closest(".card, .ctr-head, .hit, .minimap")) controller.select(null);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const step = 60;
    const pan: Record<string, [number, number]> = { ArrowLeft: [step, 0], ArrowRight: [-step, 0], ArrowUp: [0, step], ArrowDown: [0, -step] };
    const p = pan[e.key];
    if (p) {
      e.preventDefault();
      userMoved.current = true;
      setView((v) => ({ ...v, tx: v.tx + p[0], ty: v.ty + p[1] }));
      return;
    }
    if (e.key === "+" || e.key === "=") controller.zoom(1.25);
    else if (e.key === "-" || e.key === "_") controller.zoom(1 / 1.25);
    else if (e.key === "0") controller.fitAll();
    else return;
    e.preventDefault();
  };

  const ready = Boolean(drawn && plan && drawn.layout.key === plan.key && size.w > 0);
  const tiny = view.k < FLOOR - 1e-3;

  // Names over a plan zoomed out past the floor: candidates per drawing, placement per zoom,
  // in an animation frame (a burst of wheel events places them once per frame).
  const candidates = useMemo(() => (drawn ? labelCandidates(drawn, kinds) : []), [drawn, kinds]);
  const [labels, setLabels] = useState<{ k: number; drawn: Drawn | null; list: PlacedLabel[] } | null>(null);
  // Names are cut by measuring them: once the font has loaded, measure again. `fits` keeps the
  // cut label names across zooms.
  const [fonts, setFonts] = useState(() => ({ tick: 0, fits: new Map<string, string>() }));
  const { tick: fontsTick, fits } = fonts;
  useEffect(() => {
    let live = true;
    void document.fonts?.ready.then(() => live && setFonts((f) => ({ tick: f.tick + 1, fits: new Map() })));
    return () => {
      live = false;
    };
  }, []);
  // One frame at a time, never cancelled by the next wheel event: a continuous zoom re-places
  // the labels every frame, with whatever is current when the frame runs.
  const placeNow = useRef<() => void>(() => {});
  placeNow.current = () => {
    if (!tiny) return;
    setLabels({ k: view.k, drawn, list: placeLabels(candidates, view.k, labelMeasure, fits) });
  };
  const raf = useRef(0);
  useEffect(() => {
    if (!tiny || raf.current) return;
    raf.current = requestAnimationFrame(() => {
      raf.current = 0;
      placeNow.current();
    });
  }, [tiny, view.k, drawn, candidates, fits]);
  useEffect(
    () => () => {
      cancelAnimationFrame(raf.current);
      // A remount (StrictMode, Fast Refresh) keeps the ref: it must not think a frame is pending.
      raf.current = 0;
    },
    [],
  );
  // Labels of another drawing would sit on cards that moved: none until the next placement.
  const shown = tiny && labels && labels.drawn === drawn ? labels : null;
  const overflow = drawn ? (drawn.layout.width + AXIS) * view.k > size.w + 1 || drawn.layout.height * view.k > size.h + 1 : false;

  return (
    <div
      ref={stageRef}
      className="stage"
      data-ready={ready ? "" : undefined}
      tabIndex={0}
      role="application"
      aria-roledescription="plano"
      aria-label={KEY_HELP}
      onKeyDown={onKeyDown}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerLeave={() => setTip(null)}
    >
      {scene && drawn && (
        <svg
          className={`dia${tiny ? " tiny" : ""}`}
          id="dia"
          data-k={view.k.toFixed(3)}
          data-mode={drawn.plan.mode}
          width={size.w}
          height={size.h}
          viewBox={`0 0 ${size.w} ${size.h}`}
          role="group"
          aria-label="Plano por capas"
        >
          <g transform={`translate(${view.tx},${view.ty}) scale(${view.k})`}>
            <PlanContent drawn={drawn} scene={scene} selected={selected} hovered={hovered} pulsing={pulsing} problems={problems} kinds={kinds} fonts={fontsTick} onTip={onTip} />
          </g>
        </svg>
      )}
      {shown && <LabelLayer list={shown.list} lk={shown.k} view={view} stage={size} />}
      {drawn && overflow && (
        <Minimap
          right={14 + covered}
          layout={drawn.layout}
          view={view}
          stage={size}
          onMove={(cx, cy) => {
            userMoved.current = true;
            setView((v) => ({ ...v, tx: (size.w - covered) / 2 - cx * v.k, ty: size.h / 2 - cy * v.k }));
          }}
        />
      )}
      {laying && drawn && <p className="stage-busy">Dibujando el plano…</p>}
      {plan && plan.nodes.length === 0 && phase === "ready" && (
        <p className="stage-empty">Nada que dibujar aquí: nada de esta vista importa a otra cosa. Prueba otro nivel o «sin conexiones».</p>
      )}
      {error && <p className="stage-empty">No se pudo calcular el plano: {error}</p>}
      {tip && (
        <div className="tip" role="tooltip" style={{ left: Math.min(window.innerWidth - 320, tip.x + 14), top: Math.min(window.innerHeight - 110, tip.y + 14) }}>
          <b>{tip.title}</b>
          {tip.lines.map((l) => (
            <span key={l.text} className={l.alert ? "alert" : l.kind ? `kind k-${l.kind}` : undefined}>
              {l.text}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}
