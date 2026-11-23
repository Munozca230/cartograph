// The detail drawer: nothing until a card is selected. Then its name, one sentence about its
// place in the view, three numbers, and "Ver más" for the lists. Esc or × closes it.
import { useState } from "react";
import type { DirDetail, ExtDetail, FileDetail, FileFlowsResponse, NodeDetail } from "../api/types";
import { entryLabel, entryTag, entryWhere } from "../model/callflow";
import { chainOf, type Plan } from "../model/plan";
import { canOpenFolder, isOpen } from "../model/view";
import { controller } from "../state/controller";
import { flows } from "../state/flows";
import { findingLines } from "../model/findings";
import { useFindings, usePlan, useProblems } from "../state/plan";
import { store, useAppState } from "../state/store";
import { describeKind, formatLoc, formatNumber, locUnit, nameOf, pathOf, reasonLabel } from "./format";
import { Icon } from "./icons";

const MAX_ROWS = 8;

interface Row {
  id: string;
  label: string;
  value?: number;
  meta?: string;
}

function Block({ title, unit, rows, tone, empty }: { title: string; unit?: string; rows: Row[]; tone?: "out" | "in"; empty: string }) {
  const max = Math.max(1, ...rows.map((r) => r.value ?? 0));
  const shown = rows.slice(0, MAX_ROWS);
  return (
    <section className="block">
      <h5>
        {title}
        {unit && rows.length > 0 && <span>{unit}</span>}
      </h5>
      {rows.length === 0 ? (
        <p className="small faint">{empty}</p>
      ) : (
        <ul className={`rows${tone === "out" ? " out" : ""}`}>
          {shown.map((r) => (
            <li key={`${r.id}-${r.meta ?? ""}`}>
              <button
                type="button"
                className="n"
                title={r.id.startsWith("ext:") ? r.id.slice(4) : pathOf(r.id)}
                onClick={() => controller.select(r.id, { fly: true })}
                onPointerEnter={() => controller.hover(r.id)}
                onPointerLeave={() => controller.hover(null)}
              >
                {r.label}
              </button>
              {r.value !== undefined ? (
                <>
                  <span className="bar2" aria-hidden="true">
                    <i style={{ width: `${Math.max(6, (100 * r.value) / max)}%` }} />
                  </span>
                  <span className="w">{formatNumber(r.value)}</span>
                </>
              ) : (
                <span className="w wide">{r.meta}</span>
              )}
            </li>
          ))}
        </ul>
      )}
      {rows.length > MAX_ROWS && <div className="small faint">y {formatNumber(rows.length - MAX_ROWS)} más</div>}
    </section>
  );
}

const short = (id: string) => (id.startsWith("ext:") ? id.slice(4) : pathOf(id) || "raíz");
const plural = (n: number, one: string, many: string) => `${formatNumber(n)} ${n === 1 ? one : many}`;

/** One plain sentence about the node's place in this view. */
function sentence(id: string, kind: string, plan: Plan | null, problems: number): { text: string; alert: boolean } | null {
  if (!plan) return null;
  if (plan.boxes.some((b) => b.id === id)) {
    const inside = plan.nodes.filter((n) => chainOf(n.id).includes(id)).length;
    const tail = problems > 0 ? ` Tiene ${plural(problems, "problema", "problemas")}.` : "";
    return { text: `Carpeta abierta: su caja muestra ${plural(inside, "elemento", "elementos")} del plano.${tail}`, alert: problems > 0 };
  }
  const cycle = plan.cycles.find((c) => c.includes(id));
  const outs = new Set(plan.allEdges.filter((e) => e.src === id).map((e) => e.dst));
  const ins = new Set(plan.allEdges.filter((e) => e.dst === id).map((e) => e.src));
  const it = kind === "file" ? "lo" : "la";
  let text: string;
  if (cycle) {
    const mutual = [...outs].find((o) => ins.has(o));
    text = mutual ? `Está en un ciclo: ${nameOf(id)} y ${nameOf(mutual)} se importan entre sí.` : `Está en un ciclo con ${plural(cycle.length - 1, "elemento", "elementos")} de esta vista.`;
  } else if (outs.size === 0 && ins.size === 0) {
    text = plan.lightOnly.some((n) => n.id === id) ? "Solo tiene conexiones livianas, que esta vista densa oculta." : "No se conecta con nada de esta vista.";
  } else if (outs.size === 0) {
    text = `Es una base: ${it} usan ${plural(ins.size, "elemento", "elementos")} y no depende de nada de esta vista.`;
  } else if (ins.size === 0) {
    text = `Es un punto de entrada: depende de ${plural(outs.size, "elemento", "elementos")} y nada de esta vista ${it} usa.`;
  } else {
    text = `Depende de ${plural(outs.size, "elemento", "elementos")} y ${it} usan ${formatNumber(ins.size)}.`;
  }
  if (problems > 0) text += ` Tiene ${plural(problems, "problema", "problemas")}.`;
  return { text, alert: Boolean(cycle) || problems > 0 };
}

function Facts({ items }: { items: [number | string, string][] }) {
  return (
    <div className="facts">
      {items.map(([v, label]) => (
        <span key={label}>
          <b>{typeof v === "number" ? formatNumber(v) : v}</b>
          {label}
        </span>
      ))}
    </div>
  );
}

const locFact = (loc: number): [string, string] => [loc >= 10000 ? formatLoc(loc) : formatNumber(loc), locUnit(loc)];

function DirBody({ d, more }: { d: DirDetail; more: boolean }) {
  return (
    <>
      <Facts items={[[d.node.files ?? 0, "archivos"], locFact(d.node.loc), [d.deps_out.length, d.deps_out.length === 1 ? "dependencia" : "dependencias"]]} />
      {more && (
        <>
          <Block title="Depende de" unit="pares de archivos" tone="out" rows={d.deps_out.map((x) => ({ id: x.id, label: short(x.id), value: x.weight }))} empty="No importa nada de fuera." />
          <Block title="La usan" unit="pares de archivos" rows={d.deps_in.map((x) => ({ id: x.id, label: short(x.id), value: x.weight }))} empty="Nada de fuera la importa." />
          <Block title="Librerías" unit="archivos" rows={d.externals.map((x) => ({ id: x.ext, label: x.ext.slice(4), value: x.weight }))} empty="No usa librerías externas." />
        </>
      )}
    </>
  );
}

function FileBody({ d, more }: { d: FileDetail; more: boolean }) {
  const seen = new Set<string>();
  const imports = d.imports
    .filter((i) => !i.target.startsWith("ext:") && !seen.has(i.target) && seen.add(i.target))
    .map((i) => ({ id: i.target, label: nameOf(i.target), meta: `línea ${i.line}` }));
  const seenIn = new Set<string>();
  const importers = d.importers.filter((i) => !seenIn.has(i.source) && seenIn.add(i.source)).map((i) => ({ id: i.source, label: short(i.source), meta: `línea ${i.line}` }));
  const exts = [...new Set([...d.externals.map((e) => e.ext), ...d.imports.filter((i) => i.target.startsWith("ext:")).map((i) => i.target)])];
  return (
    <>
      <Facts items={[locFact(d.node.loc), [importers.length, "lo importan"], [imports.length, "importa"]]} />
      {more && (
        <>
          {d.unresolved.length > 0 && (
            <p className="note alert small">
              {d.unresolved.slice(0, 3).map((u) => `${u.specifier} (${reasonLabel(u.reason).toLowerCase()}, línea ${u.line})`).join("; ")}
              {d.unresolved.length > 3 ? "…" : ""}
            </p>
          )}
          <Block title="Importa" rows={imports} tone="out" empty="No importa otros archivos del repositorio." />
          <Block title="Lo importan" rows={importers} empty="Ningún archivo lo importa: puede ser un punto de entrada o código sin uso." />
          <Block title="Librerías" rows={exts.map((e) => ({ id: e, label: e.slice(4) }))} empty="No usa librerías externas." />
          {(d.bridges?.length ?? 0) > 0 && <Block title="Puentes" unit="IPC, HTTP, paquetes" rows={bridgeRows(d)} empty="" />}
        </>
      )}
    </>
  );
}

/** The bridges of a file as rows: the other end, with the channel or route as the meta. */
function bridgeRows(d: FileDetail) {
  const me = d.node.id;
  return (d.bridges ?? []).map((b) => {
    const out = b.src === me;
    const other = out ? b.dst : b.src;
    const line = out ? b.src_line : b.dst_line;
    return {
      id: other,
      label: `${out ? "→" : "←"} ${nameOf(other)}`,
      meta: `${b.kind} ${b.subtype} ${b.key}${line ? ` · línea ${line}` : ""}`,
    };
  });
}

function ExtBody({ d, more }: { d: ExtDetail; more: boolean }) {
  const seen = new Set<string>();
  const files = d.importers.filter((i) => !seen.has(i.source) && seen.add(i.source));
  return (
    <>
      <Facts items={[[files.length, files.length === 1 ? "archivo la usa" : "archivos la usan"], [d.importers.length, "imports"]]} />
      {more && <Block title="La usan" rows={files.map((f) => ({ id: f.source, label: short(f.source), meta: f.specifier }))} empty="Nadie la importa." />}
    </>
  );
}

function isFile(d: NodeDetail): d is FileDetail {
  return d.node.kind === "file";
}
function isDirLike(d: NodeDetail): d is DirDetail {
  return d.node.kind === "dir" || d.node.kind === "repo";
}
function isExt(d: NodeDetail): d is ExtDetail {
  return d.node.kind === "ext";
}

export function DetailPanel() {
  const detail = useAppState((s) => s.detail);
  const hierarchy = useAppState((s) => s.hierarchy);
  const view = useAppState((s) => s.view);
  const plan = usePlan();
  const problems = useProblems();
  const found = useFindings();
  const [more, setMore] = useState<{ id: string; on: boolean }>({ id: "", on: false });
  const fileFlows = useAppState((s) => s.fileFlows);
  if (!detail) return null;
  const id = detail.id;
  const fileFlowsOpen = fileFlows?.id === id;
  const showMore = more.id === id && more.on;
  const h = hierarchy?.nodes.get(id);
  const data = detail.data;
  const planNode = plan?.nodes.find((n) => n.id === id);
  const kind = data ? data.node.kind : (h?.kind ?? (id.startsWith("ext") ? "ext" : "dir"));
  const lang = data && isFile(data) ? data.node.lang : h?.lang;
  const title = planNode?.kind === "ext" ? planNode.name : (h?.name ?? nameOf(id));
  const path = id.startsWith("ext") ? (planNode?.eco ? `librería ${planNode.eco}` : "") : pathOf(id);
  const bad = problems.get(id) ?? 0;
  const line = id.startsWith("ext") ? null : sentence(id, kind, plan, bad);
  const folder = hierarchy !== null && (kind === "dir" || kind === "repo") && id !== hierarchy.root;
  const open = folder && hierarchy !== null && isOpen(hierarchy, view, id);
  const canOpen = folder && hierarchy !== null && canOpenFolder(hierarchy, id);
  const hasLists = Boolean(data) || id.startsWith("extmore:");
  return (
    <aside className="detail" aria-label="Detalle" aria-busy={detail.loading}>
      <div className="detail-head">
        <div className="kind small">{id.startsWith("extmore:") ? "Librerías" : describeKind(kind === "ext" ? "ext" : kind, lang)}</div>
        <button type="button" className="icon-btn" aria-label="Cerrar el detalle (Esc)" title="Cerrar (Esc)" onClick={() => controller.select(null)}>
          <Icon name="close" />
        </button>
      </div>
      <div>
        <h2 id="detail-title" tabIndex={-1}>
          {title}
        </h2>
        {path && path !== title && <div className="small faint path">{path}</div>}
      </div>
      {line && <p className={`lead${line.alert ? " alert" : ""}`}>{line.text}</p>}
      {findingLines(found.byId.get(id), 4, id).map((l) => (
        <p key={l.key} className={`note small${l.alert ? " alert" : ""}`}>
          {l.text}
          {l.where && <span className="faint path"> · {l.where}</span>}
        </p>
      ))}
      {detail.error && (
        <p className="note alert small" role="alert">
          No se pudo cargar el detalle: {detail.error}
        </p>
      )}
      {!data && !detail.error && !id.startsWith("extmore:") && <div className="detail-loading" aria-label="Cargando" />}
      {data && isFile(data) && <FileBody d={data} more={showMore} />}
      {data && isDirLike(data) && <DirBody d={data} more={showMore} />}
      {data && isExt(data) && <ExtBody d={data} more={showMore} />}
      {id.startsWith("extmore:") && planNode?.others && (
        <>
          <Facts items={[[planNode.others.length, "librerías menos usadas"]]} />
          {showMore && <Block title="Librerías" rows={planNode.others.map((name) => ({ id: `ext:${planNode.eco}:${name}`, label: name }))} empty="" />}
        </>
      )}
      <div className="actions">
        {hasLists && (
          <button type="button" className="btn" aria-expanded={showMore} onClick={() => setMore({ id, on: !showMore })}>
            {showMore ? "Ver menos" : "Ver más"}
          </button>
        )}
        {kind === "file" && (
          <button
            type="button"
            className="btn"
            aria-expanded={fileFlowsOpen}
            onClick={() => (fileFlowsOpen ? flows.closeFileFlows() : void flows.loadFileFlows(id))}
            title="Los flujos que pasan por este archivo y sus funciones, en la vista Flujos"
          >
            Ver flujos
          </button>
        )}
        {folder &&
          (open ? (
            <button type="button" className="btn" onClick={() => void controller.closeFolder(id)}>
              Cerrar carpeta
            </button>
          ) : (
            canOpen && (
              <button type="button" className="btn primary" onClick={() => controller.activate(id)}>
                Abrir carpeta
              </button>
            )
          ))}
      </div>
      {fileFlowsOpen && fileFlows && <FileFlowsBlock data={fileFlows.data} error={fileFlows.error} />}
    </aside>
  );
}

/** "Ver flujos" of a file: the flows that reach it and its functions; each opens Flujos there. */
function FileFlowsBlock({ data, error }: { data: FileFlowsResponse | null; error: string | null }) {
  if (error) return <p className="note alert small">No se pudieron leer los flujos: {error}</p>;
  if (!data) return <div className="detail-loading" aria-label="Cargando" />;
  if (!data.indexed) return <p className="note small">Este índice no tiene funciones ni llamadas: reindexa para ver los flujos.</p>;
  return (
    <>
      <section className="block file-flows">
        <h5>
          Flujos que pasan por aquí
          {data.entries.length > 0 && <span>{formatNumber(data.entries.length)}</span>}
        </h5>
        {data.entries.length === 0 ? (
          <p className="small faint">Ninguna entrada llega a este archivo{data.partial ? " (búsqueda cortada)" : ""}.</p>
        ) : (
          <ul className="calls">
            {data.entries.slice(0, 10).map((e) => (
              <li key={`${e.kind}|${e.file}|${e.line}|${e.name}`}>
                <button type="button" onClick={() => void flows.open(e.root, e)} title={`${e.file}:${e.line}`}>
                  <span className="cf">
                    <span className={`etag t-${e.kind}`}>{entryTag(e)}</span> {entryLabel(e)}
                  </span>
                  <span className="cc">{entryWhere(e)}</span>
                </button>
              </li>
            ))}
            {data.entries.length > 10 && <li className="more small faint">y {formatNumber(data.entries.length - 10)} más</li>}
          </ul>
        )}
      </section>
      <section className="block file-flows">
        <h5>
          Sus funciones
          {data.functions_total > 0 && <span>{formatNumber(data.functions_total)}</span>}
        </h5>
        {data.functions.length === 0 ? (
          <p className="small faint">No define funciones.</p>
        ) : (
          <ul className="calls">
            {data.functions.slice(0, 12).map((f) => (
              <li key={f.id}>
                <button type="button" onClick={() => void flows.open(f.id)} title={`Ver el flujo de ${f.name}`}>
                  <span className="cf">{f.name}</span>
                  <span className="cc">
                    línea {f.line} · ←{formatNumber(f.callers)} · llama a {formatNumber(f.callees)}
                  </span>
                </button>
              </li>
            ))}
            {data.functions_total > 12 && <li className="more small faint">y {formatNumber(data.functions_total - 12)} más</li>}
          </ul>
        )}
      </section>
    </>
  );
}

/**
 * Esc closes the topmost thing: the explorer drawer (in either view), then the selection of
 * the view on screen. Dialogs and popovers handle their own.
 */
export function closeTopmost(): boolean {
  const s = store.get();
  if (s.paletteOpen || s.issuesOpen) return false;
  if (s.explorerOpen) {
    store.set({ explorerOpen: false });
    return true;
  }
  if (s.mode === "flows") {
    // Flujos has its own selection; the plan's stays as it is.
    if (!s.flowSelected) return false;
    flows.select(null);
    return true;
  }
  if (s.selected) {
    controller.select(null);
    return true;
  }
  return false;
}
