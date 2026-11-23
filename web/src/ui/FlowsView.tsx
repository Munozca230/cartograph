// "Flujos": what happens when a command, a route or `main` runs. Left, the entries (and any
// function, by search); center, the call flow left to right in swimlanes per package or top
// folder; right, the selected function. Solid lines are exact or resolved calls, dashed ones
// heuristic (a name unique in the repo); magenta ones reach a file with a problem finding.
// Click a card: select it, and open its callees if they were cut ("+N"). Double click: its
// file on the plan. Wheel zooms, drag pans, Esc clears the selection.
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "@ui-sdk/react/popover";
import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { FlowEdgeData, FlowEntry, FlowNodeData, FlowResponse, FunctionResult, SymbolResponse } from "../api/types";
import {
  alertCount,
  cardMeta,
  edgeAlert,
  entryLabel,
  entryTag,
  entryWhere,
  fadeFocus,
  fileLabels,
  FLOW_CARD,
  FLOW_META_FONT,
  FLOW_NAME_FONT,
  flowTitle,
  groupEntries,
  LANE_FONT,
  LANE_TITLE,
  laneLabel,
  laneOrder,
  metaY,
  moreCapped,
  moreText,
  nameLines,
  neighborsOf,
  shortFile,
  type FlowLayout,
  type FlowMeasures,
} from "../model/callflow";
import { edgeKey } from "../model/flow";
import { fileKind } from "../model/kind";
import { controller } from "../state/controller";
import { layoutForFlow } from "../state/flowLayout";
import { flows } from "../state/flows";
import { useAppState } from "../state/store";
import { cameraStep, FLOW_K_MIN, FLOW_K_OPEN, fitFlow, keepView, revealCard, zoomAt } from "./flowCamera";
import { formatNumber, relativeTime, viaLabel, viaTip } from "./format";
import { Icon } from "./icons";
import { Minimap } from "./Minimap";
import { arrowPath, roundPath } from "./planGeometry";
import { textMeasure } from "./planLabels";
import type { View } from "./camera";

/** Longer than a double click: a click's expansion waits, so a double click only jumps. */
const CLICK_MS = 260;
/** How long the stage's size must hold before a resize refits the flow. */
const RESIZE_SETTLE_MS = 150;

const CONFIDENCE: Record<string, string> = { exact: "exacta", resolved: "resuelta", heuristic: "heurística" };


/** Canvas measures of the card fonts (rough until the font has loaded). */
function measures(): FlowMeasures {
  const name = textMeasure(FLOW_NAME_FONT);
  const meta = textMeasure(FLOW_META_FONT);
  const lane = textMeasure(LANE_FONT);
  // Lane titles are uppercase with 0.06 em of letter spacing.
  return { name, meta, lane: (t) => lane(t) + t.length * LANE_FONT * 0.06 };
}

interface Drawn {
  flow: FlowResponse;
  layout: FlowLayout;
}

/** The view that frames `d` on a stage of `w`×`h`: all of it, or as much as `floor` shows, the start included. */
function frame(d: Drawn, w: number, h: number, floor: number): View {
  return fitFlow(d.layout, w, h, d.flow.root, floor);
}

interface Tip {
  title: string;
  lines: { text: string; alert?: boolean }[];
  x: number;
  y: number;
}

const metaMeasure = textMeasure(FLOW_META_FONT);

const kindName = (k: string) => (k === "method" ? "Método" : k === "file" ? "Código de nivel de módulo" : k === "function" ? "Función" : k);

/** The cards, lanes and edges inside the camera transform. Memoized: a pan moves one group. */
const FlowContent = memo(function FlowContent({
  drawn,
  selected,
  onTip,
  onCard,
  onCardDouble,
}: {
  drawn: Drawn;
  selected: string | null;
  onTip: (e: React.PointerEvent, node: FlowNodeData | null, edge?: FlowEdgeData) => void;
  onCard: (n: FlowNodeData, byKey?: boolean) => void;
  onCardDouble: (n: FlowNodeData) => void;
}) {
  const { flow, layout } = drawn;
  const labels = useMemo(() => fileLabels(flow), [flow]);
  // With a function selected, what neither calls it nor is called by it fades (not around the start).
  const focus = fadeFocus(flow, selected);
  const near = focus ? neighborsOf(flow, focus) : null;
  const edges = flow.edges
    .map((e) => {
      const pts = layout.routes.get(edgeKey(e.src, e.dst));
      if (!pts || pts.length < 2) return null;
      const alert = edgeAlert(flow, e);
      const hot = selected !== null && (e.src === selected || e.dst === selected);
      const cls = alert ? "alert" : hot ? "hot" : focus ? "dim" : "norm";
      const end = pts[pts.length - 1]!;
      return { e, d: roundPath(pts), head: arrowPath(end, pts[pts.length - 2]!), end, cls, rank: alert ? 3 : hot ? 2 : 1 };
    })
    .filter((x): x is NonNullable<typeof x> => x !== null)
    .sort((a, b) => a.rank - b.rank);
  return (
    <>
      <g className="lanes">
        {laneOrder(flow).map((l) => {
          const r = layout.lanes.get(l);
          if (!r) return null;
          return (
            <g key={l} className="lane" transform={`translate(${r.x},${r.y})`}>
              <rect width={r.w} height={r.h} rx={10} />
            </g>
          );
        })}
      </g>
      <g className="edges">
        {edges.map(({ e, d, head, end, cls }) => (
          <g key={edgeKey(e.src, e.dst)} className={`edge ${cls}${e.via ? " via" : ""}`}>
            <path className="line" d={d} strokeWidth={1.6} strokeDasharray={e.via ? "2 4" : e.confidence === "heuristic" ? "5 4" : undefined} />
            <path className="head" d={head} />
            {/* A bridge (IPC, HTTP), not a call: its channel or route just before the arrow. */}
            {e.via && (
              <text className="ecount" x={end.x - 12} y={end.y + 13} textAnchor="end">
                {viaLabel(e.via)}
              </text>
            )}
            {/* Several call sites, one edge: how many, just before the arrow. */}
            {e.calls > 1 && (
              <text className="ecount" x={end.x - 12} y={end.y - 5} textAnchor="end">
                ×{e.calls}
              </text>
            )}
          </g>
        ))}
      </g>
      <g className="cards">
        {flow.nodes.map((n) => {
          const r = layout.cards.get(n.id);
          if (!r) return null;
          const isSel = n.id === selected;
          const far = near !== null && !isSel && !near.has(n.id);
          const pill = moreText(n);
          const capped = moreCapped(n);
          const pillW = pill ? metaMeasure(pill) + 2 * FLOW_CARD.pillPad : 0;
          const meta = cardMeta(n, labels);
          const kind = fileKind(n.file);
          const cls = ["card", "fcard", `k-${kind}`, isSel && "sel", far && "far", near?.has(n.id) && "nb", n.id === flow.root && "root"].filter(Boolean).join(" ");
          return (
            <g
              key={n.id}
              className={cls}
              transform={`translate(${r.x},${r.y})`}
              data-id={n.id}
              tabIndex={0}
              role="button"
              aria-pressed={isSel}
              aria-label={`${kindName(n.kind)} ${n.name}, ${n.file}:${n.line}${n.more > 0 ? (capped ? `, ${n.more} llamadas más que el flujo no muestra` : `, ${n.more} llamadas más sin abrir`) : ""}${n.finding ? ", su archivo tiene un hallazgo" : ""}`}
              onClick={() => onCard(n)}
              onDoubleClick={() => onCardDouble(n)}
              onPointerMove={(e) => onTip(e, n)}
              onPointerLeave={(e) => onTip(e, null)}
              onKeyDown={(e) => {
                if (e.key !== "Enter" && e.key !== " ") return;
                e.preventDefault();
                e.stopPropagation();
                if (selected === n.id) onCardDouble(n);
                else onCard(n, true);
              }}
            >
              <rect className="face" width={r.w} height={r.h} rx={8} />
              <path className="stripe" d={`M3,0.8 A8,8 0 0 0 0,8 V${r.h - 8} A8,8 0 0 0 3,${r.h - 0.8} Z`} />
              <text x={FLOW_CARD.padX + FLOW_CARD.stripe} y={FLOW_CARD.nameY} className="cn">
                {nameLines(n.name, r.h).map((t, i) => (
                  <tspan key={i} x={FLOW_CARD.padX + FLOW_CARD.stripe} dy={i === 0 ? 0 : FLOW_CARD.line}>
                    {t}
                  </tspan>
                ))}
              </text>
              <text x={FLOW_CARD.padX + FLOW_CARD.stripe} y={metaY(r.h)} className="cm">
                {meta}
              </text>
              {pill && (
                <g className={capped ? "pill capped" : "pill"} transform={`translate(${r.w - FLOW_CARD.padX - pillW + 4},${metaY(r.h) - 13})`}>
                  <rect width={pillW} height={18} rx={9} />
                  <text x={pillW / 2} y={13} textAnchor="middle">
                    {pill}
                  </text>
                </g>
              )}
              {n.finding && <circle className="pdot" cx={r.w - 11} cy={12} r={3.5} />}
            </g>
          );
        })}
      </g>
      <g className="hits">
        {edges.map(({ e, d }) => (
          <path key={edgeKey(e.src, e.dst)} className="hit" d={d} onPointerMove={(ev) => onTip(ev, null, e)} onPointerLeave={(ev) => onTip(ev, null)} />
        ))}
      </g>
    </>
  );
});

function FlowStage() {
  const flow = useAppState((s) => s.flow);
  const loading = useAppState((s) => s.flowLoading);
  const error = useAppState((s) => s.flowError);
  const selected = useAppState((s) => s.flowSelected);
  const from = useAppState((s) => s.flowFrom);
  const entries = useAppState((s) => s.flowEntries);
  const stageRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [drawn, setDrawn] = useState<Drawn | null>(null);
  const [view, setView] = useState<View>({ k: 1, tx: 0, ty: 0 });
  const [tip, setTip] = useState<Tip | null>(null);
  const [layoutError, setLayoutError] = useState<string | null>(null);
  const [fonts, setFonts] = useState(0);
  const drag = useRef<{ x: number; y: number; tx: number; ty: number; moved: boolean } | null>(null);
  /** The last press was a drag: the click that ends it selects nothing. Cleared by the next press. */
  const justDragged = useRef(false);
  const clickTimer = useRef(0);

  useLayoutEffect(() => {
    const el = stageRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setSize({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    setSize({ w: el.clientWidth, h: el.clientHeight });
    return () => ro.disconnect();
  }, []);

  useEffect(() => {
    let live = true;
    void document.fonts?.ready.then(() => live && setFonts((f) => f + 1));
    return () => {
      live = false;
    };
  }, []);

  // Layout (cards are sized by measuring their names, again once the font has loaded).
  useEffect(() => {
    if (!flow || !flow.indexed) return;
    try {
      const layout = layoutForFlow(flow, measures());
      setLayoutError(null);
      setDrawn({ flow, layout });
    } catch (err: unknown) {
      setLayoutError(err instanceof Error ? err.message : String(err));
    }
  }, [flow, fonts]);

  // A tooltip belongs to what was under the pointer: another drawing drops it.
  useEffect(() => setTip(null), [drawn]);

  // Camera: another start fits; the same flow redrawn keeps a card in place.
  const viewRef = useRef(view);
  viewRef.current = view;
  const shown = useRef<Drawn | null>(null);
  const moved = useRef(false);
  /** How the view was last framed: a new flow opens readable, "Encuadrar" shows as much as names allow. */
  const floor = useRef(FLOW_K_OPEN);
  useLayoutEffect(() => {
    if (!drawn || size.w === 0) return;
    const prev = shown.current;
    shown.current = drawn;
    const step = cameraStep(prev && { root: prev.flow.root, layout: prev.layout }, { root: drawn.flow.root, layout: drawn.layout });
    // A resize alone redraws the same layout: the card just expanded waits for its new layout.
    if (step === "stay") return;
    const hint = flows.takePendingAnchor();
    if (step === "keep" && prev) {
      setView(keepView(prev.layout, drawn.layout, viewRef.current, size.w, size.h, hint, selected));
      moved.current = true;
      return;
    }
    moved.current = false;
    floor.current = FLOW_K_OPEN;
    setView(frame(drawn, size.w, size.h, FLOW_K_OPEN));
    // `selected` is read when the layout changes, not followed.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [drawn, size]);

  // A resize refits a flow the user hasn't moved, once the size settles: not on every tick of
  // a drag. A flow the user moved keeps its view.
  const lastSize = useRef(size);
  useEffect(() => {
    if (lastSize.current.w === size.w && lastSize.current.h === size.h) return;
    const t = window.setTimeout(() => {
      lastSize.current = size;
      if (drawn && !moved.current) setView(frame(drawn, size.w, size.h, floor.current));
    }, RESIZE_SETTLE_MS);
    return () => window.clearTimeout(t);
  }, [size, drawn]);

  // A card selected from the panel (a caller, a callee) slides into view. Only a new selection
  // moves the camera: a redraw keeps it.
  const sizeRef = useRef(size);
  sizeRef.current = size;
  useEffect(() => {
    const d = shown.current;
    if (!d || !selected || selected === d.flow.root) return;
    const r = d.layout.cards.get(selected);
    if (!r) return;
    const { w, h } = sizeRef.current;
    setView((v) => revealCard(v, r, w, h) ?? v);
  }, [selected]);

  useEffect(
    () =>
      flows.onCamera((req) => {
        const el = stageRef.current;
        if (!el || !drawn) return;
        const w = el.clientWidth;
        const h = el.clientHeight;
        if (req.type === "fit") {
          moved.current = false;
          floor.current = FLOW_K_MIN;
          setView(frame(drawn, w, h, FLOW_K_MIN));
        } else {
          moved.current = true;
          setView((v) => zoomAt(v, req.factor, w / 2, h / 2));
        }
      }),
    [drawn],
  );

  useEffect(() => {
    const el = stageRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const rect = el.getBoundingClientRect();
      moved.current = true;
      setView((v) => zoomAt(v, Math.exp(-e.deltaY * 0.0015), e.clientX - rect.left, e.clientY - rect.top));
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, []);

  useEffect(() => () => window.clearTimeout(clickTimer.current), []);

  const onCard = useCallback((n: FlowNodeData, byKey = false) => {
    if (justDragged.current && !byKey) return;
    flows.select(n.id, { expand: false });
    window.clearTimeout(clickTimer.current);
    if (n.more > 0 && !moreCapped(n)) clickTimer.current = window.setTimeout(() => flows.expand(n.id), CLICK_MS);
  }, []);
  const onCardDouble = useCallback((n: FlowNodeData) => {
    window.clearTimeout(clickTimer.current);
    flows.jumpToPlan(n.file);
  }, []);

  const drawnRef = useRef(drawn);
  drawnRef.current = drawn;
  const onTip = useCallback((e: React.PointerEvent, n: FlowNodeData | null, edge?: FlowEdgeData) => {
    if (drag.current?.moved) return;
    const d = drawnRef.current;
    if (!d || (!n && !edge)) {
      setTip(null);
      return;
    }
    if (edge) {
      const src = d.flow.nodes.find((x) => x.id === edge.src);
      const dst = d.flow.nodes.find((x) => x.id === edge.dst);
      const lines: Tip["lines"] = [
        { text: `${src ? shortFile(src.file, "") : ""}:${edge.line}${edge.calls > 1 ? ` · ${edge.calls} llamadas` : ""}` },
        { text: edge.via ? viaTip(edge.via) : edge.confidence === "heuristic" ? "Heurística: el nombre es único en el repositorio" : `Llamada ${CONFIDENCE[edge.confidence]}` },
      ];
      if (dst?.finding) lines.push({ text: `Llega a un archivo con ${dst.finding === "error" ? "un error" : "un aviso"} de arquitectura`, alert: true });
      setTip({ title: `${src?.name ?? edge.src} → ${dst?.name ?? edge.dst}`, lines, x: e.clientX, y: e.clientY });
      return;
    }
    if (!n) return;
    const lines: Tip["lines"] = [{ text: `${kindName(n.kind)} · ${n.file}:${n.line}` }];
    lines.push({ text: `La llaman ${formatNumber(n.callers)} ${n.callers === 1 ? "sitio" : "sitios"} · llama a ${formatNumber(n.callees)} ${n.callees === 1 ? "función" : "funciones"}` });
    if (moreCapped(n)) lines.push({ text: `Límite del flujo: ${formatNumber(n.more)} llamadas más no caben. «Empezar aquí» muestra su propio flujo` });
    else if (n.more > 0) lines.push({ text: `${formatNumber(n.more)} sin abrir: clic para abrirlas` });
    if (n.finding) lines.push({ text: `Su archivo tiene ${n.finding === "error" ? "un error" : "un aviso"} de arquitectura`, alert: true });
    lines.push({ text: "Doble clic: su archivo en el Plano" });
    setTip({ title: n.name, lines, x: e.clientX, y: e.clientY });
  }, []);

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    justDragged.current = false;
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
    moved.current = true;
    setView((v) => ({ ...v, tx: d.tx + dx, ty: d.ty + dy }));
  };
  const onPointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    drag.current = null;
    if (d?.moved) {
      justDragged.current = true;
      e.currentTarget.releasePointerCapture?.(e.pointerId);
      return;
    }
    if (!(e.target as Element).closest(".card, .hit")) flows.select(null);
  };
  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const step = 60;
    const pan: Record<string, [number, number]> = { ArrowLeft: [step, 0], ArrowRight: [-step, 0], ArrowUp: [0, step], ArrowDown: [0, -step] };
    const p = pan[e.key];
    if (p) {
      e.preventDefault();
      moved.current = true;
      setView((v) => ({ ...v, tx: v.tx + p[0], ty: v.ty + p[1] }));
      return;
    }
    if (e.key === "+" || e.key === "=") flows.zoom(1.25);
    else if (e.key === "-" || e.key === "_") flows.zoom(1 / 1.25);
    else if (e.key === "0") flows.fit();
    else return;
    e.preventDefault();
  };

  const current = drawn;
  // Part of the flow is off the stage: the minimap shows all of it, and where the stage is.
  const overflow = current
    ? view.tx < -1 || view.ty < -1 || current.layout.width * view.k + view.tx > size.w + 1 || current.layout.height * view.k + view.ty > size.h + 1
    : false;
  let empty: React.ReactNode = null;
  if (flow && !flow.indexed) empty = <NoSymbols />;
  else if (entries && !entries.indexed) empty = <NoSymbols />;
  else if (!from && entries && entries.entries.length === 0)
    empty = (
      <p className="stage-empty">
        <span>
          No se encontraron entradas en este repositorio (main, rutas HTTP, comandos, escenas de Godot).
          <br />
          Busca cualquier función a la izquierda para ver qué llama.
        </span>
      </p>
    );
  else if (!from) empty = <p className="stage-empty">Elige una entrada o busca una función a la izquierda.</p>;
  else if (error) empty = <p className="stage-empty">No se pudo calcular el flujo: {error}</p>;
  else if (layoutError) empty = <p className="stage-empty">No se pudo dibujar el flujo: {layoutError}</p>;

  return (
    <div
      ref={stageRef}
      className="stage flow-stage"
      data-ready={current && !loading ? "" : undefined}
      tabIndex={0}
      role="application"
      aria-roledescription="flujo de llamadas"
      aria-label="Flujo de llamadas. Flechas: mover. Más y menos: acercar y alejar. Cero: encuadrar. Intro: seleccionar y, si ya está seleccionada, ir a su archivo en el Plano. Escape: quitar la selección."
      onKeyDown={onKeyDown}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerLeave={() => setTip(null)}
    >
      {current && !empty && (
        <svg className="dia flow-dia" width={size.w} height={size.h} viewBox={`0 0 ${size.w} ${size.h}`} role="group" aria-label="Flujo de llamadas por carriles">
          <g transform={`translate(${view.tx},${view.ty}) scale(${view.k})`}>
            <FlowContent drawn={current} selected={selected} onTip={onTip} onCard={onCard} onCardDouble={onCardDouble} />
          </g>
          <LaneTitles drawn={current} view={view} width={size.w} height={size.h} />
        </svg>
      )}
      {current && !empty && overflow && (
        <Minimap
          right={14}
          layout={current.layout}
          view={view}
          stage={size}
          label="Minimapa: todo el flujo y la parte a la vista. Pulsa para ir a otra parte."
          onMove={(cx, cy) => {
            moved.current = true;
            setView((v) => ({ ...v, tx: size.w / 2 - cx * v.k, ty: size.h / 2 - cy * v.k }));
          }}
        />
      )}
      {empty}
      {loading && current && <p className="stage-busy">Siguiendo las llamadas…</p>}
      {tip && (
        <div className="tip" role="tooltip" style={{ left: Math.min(window.innerWidth - 320, tip.x + 14), top: Math.min(window.innerHeight - 110, tip.y + 14) }}>
          <b>{tip.title}</b>
          {tip.lines.map((l) => (
            <span key={l.text} className={l.alert ? "alert" : undefined}>
              {l.text}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * Lane titles in screen space: at the top left of their lane, and held inside the stage while
 * the lane is partly scrolled out of it, so a big flow never hides which lane is which. They
 * scale with the zoom (never past 1:1), so they keep fitting their lane.
 */
function LaneTitles({ drawn, view, width, height }: { drawn: Drawn; view: View; width: number; height: number }) {
  const f = Math.min(1, view.k);
  return (
    <g className="lane-heads" aria-hidden="true">
      {laneOrder(drawn.flow).map((l) => {
        const r = drawn.layout.lanes.get(l);
        if (!r) return null;
        const left = r.x * view.k + view.tx;
        const right = (r.x + r.w) * view.k + view.tx;
        const top = r.y * view.k + view.ty;
        const bottom = (r.y + r.h) * view.k + view.ty;
        if (right < 0 || left > width || bottom < 24 || top > height) return null;
        const text = laneLabel(l).toUpperCase();
        const tw = text.length * LANE_FONT * f * 0.8;
        const x = Math.max(left + LANE_TITLE.x * view.k, Math.min(8, right - LANE_TITLE.x * view.k - tw));
        const y = Math.min(Math.max(top + LANE_TITLE.y * view.k, 18), bottom - 8);
        return (
          <text key={l} className="lane-t" x={x} y={y} style={{ fontSize: `${LANE_FONT * f}px`, letterSpacing: `${0.06 * f}em` }}>
            {text}
          </text>
        );
      })}
    </g>
  );
}

function NoSymbols() {
  return (
    <div className="stage-empty">
      <div className="empty-card">
        <p>Este índice no tiene la capa de funciones y llamadas: se escribió con una versión anterior de cartograph.</p>
        <button type="button" className="btn primary" onClick={() => void controller.reindex()}>
          Reindexar ahora
        </button>
      </div>
    </div>
  );
}

function EntryRow({ e, active }: { e: FlowEntry; active: boolean }) {
  return (
    <li>
      <button type="button" className="entry" aria-current={active ? "true" : undefined} title={`${e.name} · ${e.file}${e.line > 0 ? `:${e.line}` : ""}`} onClick={() => void flows.open(e.root, e)}>
        <span className={`etag t-${e.kind}`}>{entryTag(e)}</span>
        <span className="ename">
          <Breakable text={entryLabel(e)} />
        </span>
        <span className="ewhere">{entryWhere(e)}</span>
      </button>
    </li>
  );
}

function FlowRail() {
  const entries = useAppState((s) => s.flowEntries);
  const error = useAppState((s) => s.flowEntriesError);
  const from = useAppState((s) => s.flowFrom);
  const [q, setQ] = useState("");
  const [found, setFound] = useState<{ q: string; results: FunctionResult[] } | null>(null);
  useEffect(() => {
    const text = q.trim();
    if (text.length < 2) return;
    const ctl = new AbortController();
    const t = window.setTimeout(() => {
      flows.api
        .searchFunctions(text, 30, ctl.signal)
        .then((r) => setFound({ q: text, results: r.results }))
        .catch(() => {
          // Aborted by the next keystroke, or the server is away: keep what is listed.
        });
    }, 160);
    return () => {
      window.clearTimeout(t);
      ctl.abort();
    };
  }, [q]);
  const groups = useMemo(() => groupEntries(entries?.entries ?? [], q), [entries, q]);
  const fns = q.trim().length >= 2 && found && found.q === q.trim() ? found.results : null;
  const isActive = (e: FlowEntry) => from?.entry !== null && from?.entry?.name === e.name && from.entry.file === e.file && from.entry.line === e.line;
  return (
    <aside className="flow-rail" aria-label="Entradas y funciones">
      <label className="rail-search">
        <Icon name="search" />
        <input
          type="search"
          value={q}
          placeholder="Buscar entrada o función…"
          aria-label="Buscar entrada o función"
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => {
            // Enter opens the first match: a listed entry first, then a function.
            if (e.key !== "Enter") return;
            const entry = groups[0]?.items[0];
            if (entry) void flows.open(entry.root, entry);
            else if (fns?.[0]) void flows.open(fns[0].id);
          }}
        />
      </label>
      <div className="rail-scroll">
        {fns && (
          <section>
            <h3>
              Funciones · {formatNumber(fns.length)}
              {fns.length === 30 ? "+" : ""}
            </h3>
            {fns.length === 0 ? (
              <p className="small faint">Ninguna función se llama así.</p>
            ) : (
              <ul>
                {fns.map((f) => (
                  <li key={f.id}>
                    <button type="button" className="entry" aria-current={from?.id === f.id ? "true" : undefined} title={`${f.path}:${f.line}`} onClick={() => void flows.open(f.id)}>
                      <span className="etag t-fn">{f.symbol_kind === "method" ? "método" : "fn"}</span>
                      <span className="ename">
                        <Breakable text={f.name} />
                      </span>
                      <span className="ewhere">{entryWhere({ file: f.path, line: f.line } as FlowEntry)}</span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </section>
        )}
        {error && <p className="note alert small">No se pudieron leer las entradas: {error}</p>}
        {entries && !entries.indexed && <p className="note small">Este índice no tiene funciones ni llamadas. Reindexa para ver los flujos.</p>}
        {entries && entries.indexed && entries.entries.length === 0 && (
          <p className="note small">
            No hay entradas detectadas (main, rutas HTTP, comandos, escenas de Godot). Escribe arriba el nombre de cualquier función para ver su flujo.
          </p>
        )}
        {entries && entries.entries.length > 0 && (
          <h3 className="rail-total">
            Entradas · {formatNumber(entries.total)}
            {entries.hidden > 0 && <span title="Entradas en carpetas de pruebas, fixtures y ejemplos, que no se listan"> · {formatNumber(entries.hidden)} en pruebas</span>}
          </h3>
        )}
        {groups.map((g) => (
          <section key={g.title}>
            <h4>
              {g.title} · {formatNumber(g.items.length)}
            </h4>
            <ul>
              {g.items.map((e) => (
                <EntryRow key={`${e.kind}|${e.file}|${e.line}|${e.name}`} e={e} active={isActive(e)} />
              ))}
            </ul>
          </section>
        ))}
        {!entries && !error && <div className="detail-loading" aria-label="Cargando" />}
      </div>
    </aside>
  );
}

/** A card's title before its data arrives: the function's name, or the file's for `file:` code. */
function cardName(id: string): string {
  if (id.startsWith("file:")) return id.slice(id.lastIndexOf("/") + 1).replace(/^file:/, "");
  return id.slice(id.lastIndexOf("#") + 1);
}

/** The last three parts of a path (`cli/src/main.rs`): enough to tell files apart in a list. */
function tailPath(path: string): string {
  const parts = path.split("/");
  return parts.length > 3 ? parts.slice(-3).join("/") : path;
}

/** A name that may wrap after its dots, slashes and underscores (`Manager.<wbr>start`), never inside a word. */
function Breakable({ text }: { text: string }) {
  const parts = text.split(/(?<=[./_])/);
  return (
    <>
      {parts.map((p, i) => (
        <span key={i}>
          {p}
          {i < parts.length - 1 && <wbr />}
        </span>
      ))}
    </>
  );
}

/** "hace 3 h", "hace 18 días", "hace 4 meses". */
function ago(seconds: number): string {
  const ms = Date.now() - seconds * 1000;
  const days = Math.floor(ms / 86_400_000);
  if (days < 1) return relativeTime(ms);
  if (days < 60) return `hace ${days} ${days === 1 ? "día" : "días"}`;
  const months = Math.round(days / 30);
  return months < 24 ? `hace ${months} meses` : `hace ${Math.round(days / 365)} años`;
}

function GitLine({ git }: { git: NonNullable<SymbolResponse["git"]> }) {
  const top = git.authors[0];
  const share = top && git.commits > 0 ? Math.round((100 * top.commits) / git.commits) : 0;
  return (
    <>
      <dt>Cambios</dt>
      <dd>
        {formatNumber(git.commits)} {git.commits === 1 ? "commit" : "commits"} · último {ago(git.last_time)}
      </dd>
      <dt>Dueños</dt>
      <dd>
        {git.authors_total <= 1 ? "un autor" : `${formatNumber(git.authors_total)} autores · ${top?.name ?? ""} ${share} %`}
      </dd>
    </>
  );
}

function FlowPanel() {
  const card = useAppState((s) => s.flowCard);
  const flow = useAppState((s) => s.flow);
  const from = useAppState((s) => s.flowFrom);
  const data = card?.data && card.data.indexed ? card.data : null;
  const inFlow = (id: string | null) => Boolean(id && flow?.nodes.some((n) => n.id === id));
  const go = (id: string | null) => {
    if (!id) return;
    if (inFlow(id)) flows.select(id, { expand: false });
    else void flows.open(id);
  };
  return (
    <aside className="flow-panel" aria-label="Función seleccionada">
      {!card && (
        <p className="note small">
          Clic en una tarjeta: la selecciona y abre las llamadas que el flujo dejó fuera (+N). Doble clic: su archivo en el Plano. El flujo se limita por profundidad y agrupa «+N» para no saturar.
        </p>
      )}
      {card && (
        <>
          <h3 className="panel-kicker">{data ? kindName(data.symbol.kind) : "Función seleccionada"}</h3>
          <div>
            <h2 tabIndex={-1}>
              <Breakable text={data?.symbol.name ?? cardName(card.id)} />
            </h2>
            {data &&
              (data.symbol.kind === "file" ? (
                <div className="small faint path">
                  {data.symbol.file} · {formatNumber(data.symbol.end_line)} {data.symbol.end_line === 1 ? "línea de código" : "líneas de código"}
                </div>
              ) : (
                <div className="small faint path">
                  {data.symbol.file}:{data.symbol.line}
                  {data.symbol.end_line > data.symbol.line ? `–${data.symbol.end_line}` : ""}
                  {data.symbol.exported ? " · pública" : ""}
                </div>
              ))}
          </div>
          {card.error && (
            <p className="note alert small">
              {card.id.startsWith("file:") ? "No se pudo leer el archivo" : "No se pudo leer la función"}: {card.error}
            </p>
          )}
          {card.loading && !data && <div className="detail-loading" aria-label="Cargando" />}
          {data && (
            <>
              <dl className="kv">
                <dt>Llama a</dt>
                <dd>
                  {formatNumber(data.callees_total)} {data.callees_total === 1 ? "función" : "funciones"}
                  {data.callee_lanes > 1 ? ` (${data.callee_lanes} carriles)` : ""}
                </dd>
                <dt>La llaman</dt>
                <dd>
                  {formatNumber(data.callers.production_total)} {data.callers.production_total === 1 ? "sitio" : "sitios"} en {formatNumber(data.callers.production_files)}{" "}
                  {data.callers.production_files === 1 ? "archivo" : "archivos"}
                  {data.callers.test_sites > 0 ? ` · ${formatNumber(data.callers.test_sites)} en pruebas` : ""}
                </dd>
                {data.git && <GitLine git={data.git} />}
              </dl>
              {data.findings.map((t) => (
                <p key={t} className="note alert small">
                  {t}
                </p>
              ))}
              {data.entries.length > 0 && (
                <p className="note small">
                  Empieza {data.entries.length === 1 ? "la entrada" : "las entradas"} <b>{data.entries.map((e) => e.name).join(", ")}</b>
                </p>
              )}
              <section className="block">
                <h5>{data.symbol.kind === "file" ? "Quién lo llama" : "Quién la llama"}</h5>
                {data.callers.production.length === 0 ? (
                  <p className="small faint">
                    {data.symbol.kind === "file"
                      ? data.entries.length > 0
                        ? "Lo ejecuta la entrada; ninguna función lo llama."
                        : "Ninguna función lo llama: se ejecuta al cargarse."
                      : data.callers.test_sites > 0
                        ? "Solo la llaman las pruebas."
                        : "Nadie la llama desde el código del repositorio."}
                  </p>
                ) : (
                  <ul className="calls">
                    {data.callers.production.slice(0, 8).map((c) => (
                      <li key={`${c.file}:${c.line}`}>
                        <button type="button" onClick={() => go(c.caller)} disabled={!c.caller} title={c.caller ?? "código de nivel de módulo"}>
                          <span className="cf" title={`${c.file}:${c.line}`}>
                            <Breakable text={`${tailPath(c.file)}:${c.line}`} />
                          </span>
                          <span className={`conf c-${c.confidence}`}>{CONFIDENCE[c.confidence]}</span>
                          <span className="cc">{c.caller_name ?? "(nivel de módulo)"}</span>
                        </button>
                      </li>
                    ))}
                    {data.callers.production_total > 8 && <li className="more small faint">y {formatNumber(data.callers.production_total - 8)} más</li>}
                    {data.callers.test_sites > 0 && (
                      <li className="more small faint">
                        +{formatNumber(data.callers.test_sites)} en pruebas ({formatNumber(data.callers.test_files)} {data.callers.test_files === 1 ? "archivo" : "archivos"})
                      </li>
                    )}
                  </ul>
                )}
              </section>
              {data.callees.length > 0 && (
                <section className="block">
                  <h5>Llama a</h5>
                  <ul className="calls">
                    {data.callees.slice(0, 8).map((c) => (
                      <li key={c.id}>
                        <button type="button" onClick={() => go(c.id)} title={`${c.file}:${c.line}`}>
                          <span className="cf">
                            <Breakable text={c.name} />
                          </span>
                          <span className={`conf c-${c.confidence}`}>{CONFIDENCE[c.confidence]}</span>
                          <span className="cc">
                            {tailPath(c.file)}:{c.line}
                          </span>
                        </button>
                      </li>
                    ))}
                    {data.callees_total > 8 && <li className="more small faint">y {formatNumber(data.callees_total - 8)} más</li>}
                  </ul>
                </section>
              )}
              <div className="actions">
                <button type="button" className="btn" onClick={() => flows.jumpToPlan(data.symbol.file)}>
                  Ver en el Plano
                </button>
                {from?.id !== data.symbol.id && (
                  <button type="button" className="btn primary" onClick={() => void flows.open(data.symbol.id)}>
                    Empezar aquí
                  </button>
                )}
              </div>
            </>
          )}
        </>
      )}
    </aside>
  );
}

/** The Flujos toolbar: what flow this is, and the camera. */
export function FlowToolbar() {
  const flow = useAppState((s) => s.flow);
  const from = useAppState((s) => s.flowFrom);
  const repo = useAppState((s) => s.meta?.repo.name ?? "");
  const loading = useAppState((s) => s.flowLoading);
  const shown = flow && flow.indexed && from && flow.root === from.id ? flow : null;
  const root = shown?.nodes.find((n) => n.id === shown.root);
  const title = flowTitle(from?.entry ?? null, repo, root?.name ?? "");
  const alerts = shown ? alertCount(shown) : 0;
  return (
    <div className="toolbar flow-toolbar">
      {shown ? (
        <p className="crumbs">
          Flujo de <b>{title}</b> · profundidad {shown.depth} · {formatNumber(shown.nodes.length)} {shown.nodes.length === 1 ? "función" : "funciones"}
          {shown.truncated ? " · con llamadas sin abrir (+N)" : ""}
          {alerts > 0 && (
            <span className="alert">
              {" "}
              · {formatNumber(alerts)} {alerts === 1 ? "llamada a un archivo con hallazgo" : "llamadas a archivos con hallazgos"}
            </span>
          )}
        </p>
      ) : (
        <p className="crumbs faint">{loading ? "Siguiendo las llamadas…" : "Flujos: qué pasa cuando se ejecuta una entrada"}</p>
      )}
      <div className="toolbar-end">
        <div className="zoom">
          <button type="button" className="icon-btn" aria-label="Alejar" title="Alejar (−)" onClick={() => flows.zoom(1 / 1.25)}>
            <Icon name="minus" />
          </button>
          <button type="button" className="icon-btn" aria-label="Acercar" title="Acercar (+)" onClick={() => flows.zoom(1.25)}>
            <Icon name="plus" />
          </button>
          <button type="button" className="btn fit" title="Encuadrar el flujo (0)" onClick={() => flows.fit()}>
            <Icon name="fit" />
            <span className="fit-text">Encuadrar</span>
          </button>
        </div>
        <Popover>
          <PopoverTrigger className="icon-btn" aria-label="Cómo leer el flujo" title="Cómo leer el flujo">
            <Icon name="help" />
          </PopoverTrigger>
          <PopoverContent className="pop legend-pop" side="bottom" align="end" sideOffset={8}>
            <PopoverTitle className="pop-title">Cómo leer el flujo</PopoverTitle>
            <FlowLegend />
          </PopoverContent>
        </Popover>
      </div>
    </div>
  );
}

function FlowLegend() {
  return (
    <div className="flow-legend">
      <p>
        De izquierda a derecha, lo que llama cada función. Cada carril es un paquete o una carpeta. ←N: sitios que la llaman en todo el repositorio. +N: llamadas que el
        flujo dejó fuera; clic para abrirlas.
      </p>
      <span>
        <i />
        llamada exacta o resuelta
      </span>
      <span>
        <i className="d" />
        heurística (el nombre es único en el repositorio)
      </span>
      <span className="alert">
        <i />
        llega a un archivo con un error o aviso de arquitectura
      </span>
      <span>×N: la llama desde N sitios</span>
      <span>
        <i className="f" />
        atenuada: no llama ni es llamada por la función seleccionada
      </span>
      <p className="faint">Lo que no cabe a la vista sale en el minimapa; «Encuadrar» muestra todo lo que permiten los nombres legibles.</p>
      <p className="faint">Clic: seleccionar y abrir · doble clic: su archivo en el Plano · rueda: zoom · arrastrar: mover · Esc: quitar la selección</p>
    </div>
  );
}

export function FlowsView({ active }: { active: boolean }) {
  return (
    <section className="flows" data-active={active ? "" : undefined} aria-hidden={!active} inert={!active}>
      <FlowRail />
      <FlowStage />
      <FlowPanel />
    </section>
  );
}
