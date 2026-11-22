// Findings (/api/findings) on the plan: which card or box each one lands on, which drawn
// edges prove it, and how the UI words it. Pure; the store memoizes it per plan.
import type { Finding, FindingsResponse } from "../api/types";
import { edgeKey, liftEdge } from "./flow";
import { ROOT_ID } from "./hierarchy";
import { chainOf, parentOf, type Plan } from "./plan";

/** A finding worth a magenta mark: a broken rule or a warning. Info findings only explain. */
export function isProblem(f: Finding): boolean {
  return f.severity !== "info";
}

/**
 * Problem findings (errors and warnings) of the whole repo, from the summary: the list that
 * came back may be cut at the request's limit.
 */
export function problemCount(r: FindingsResponse): number {
  return (r.summary.by_severity.error ?? 0) + (r.summary.by_severity.warning ?? 0);
}

/** A location as the UI writes it: `src/ui/` for a folder, `raíz` for the repo root. */
export function placeOf(id: string): string {
  if (id === ROOT_ID) return "raíz";
  return id.slice(id.indexOf(":") + 1) + (id.startsWith("dir:") ? "/" : "");
}

export interface FindingsOnPlan {
  /** Findings on each card or box (a location lands on the nearest one that holds it). */
  byId: Map<string, Finding[]>;
  /** Problem findings proven by each drawn edge, by edge key. */
  byEdge: Map<string, Finding[]>;
}

export const NO_FINDINGS: FindingsOnPlan = { byId: new Map(), byEdge: new Map() };

/** The card or open box of the plan that holds `id` (itself or its nearest folder). */
export function holderOf(where: ReadonlySet<string>, id: string): string | null {
  for (let cur: string | null = id; cur !== null; cur = parentOf(cur)) {
    if (where.has(cur)) return cur;
    // A file id whose path is a folder on the plan (never the case today, but cheap).
    const dir = `dir:${cur.slice(cur.indexOf(":") + 1)}`;
    if (cur.startsWith("file:") && where.has(dir)) return dir;
  }
  return null;
}

const push = <K, V>(m: Map<K, V[]>, k: K, v: V) => {
  const list = m.get(k);
  if (!list) m.set(k, [v]);
  else if (!list.includes(v)) list.push(v);
};

export function findingsOnPlan(plan: Plan, findings: readonly Finding[]): FindingsOnPlan {
  if (findings.length === 0) return NO_FINDINGS;
  const cards = new Set(plan.nodes.filter((n) => n.kind !== "ext").map((n) => n.id));
  const where = new Set([...cards, ...plan.boxes.map((b) => b.id)]);
  const drawn = new Set(plan.edges.map((e) => e.key));
  const byId = new Map<string, Finding[]>();
  const byEdge = new Map<string, Finding[]>();
  for (const f of findings) {
    for (const id of f.nodes) {
      // The repo root (a Go package at the top, say) has no card: it lands on the plan's root.
      const at = id === ROOT_ID ? ROOT_ID : holderOf(where, id);
      if (at) push(byId, at, f);
    }
    // With edges left out, the ones that came back are a sample: marking them would leave
    // the rest unmarked, so such a finding stays on its nodes.
    if (!isProblem(f) || (f.edges_more ?? 0) > 0) continue;
    for (const e of f.edges) {
      const a = holderOf(cards, e.src);
      const b = holderOf(cards, e.dst);
      if (!a || !b || a === b) continue;
      // "cards" plans draw card to card; "nested" ones between the children of a box.
      const l = plan.mode === "cards" ? { src: a, dst: b } : liftEdge(chainOf, a, b);
      const key = edgeKey(l.src, l.dst);
      if (drawn.has(key)) push(byEdge, key, f);
    }
  }
  return { byId, byEdge };
}

const fmt = (n: number | undefined) => (n ?? 0).toLocaleString("es");

/** What the plan says about a finding, in Spanish (the server's title is English). */
export function findingLabel(f: Finding): string {
  const m = f.metrics;
  const rule = f.violation ? "Regla rota: " : "";
  switch (f.kind) {
    case "cycle":
      return `${rule}${m.files === 2 ? "Ciclo: dos archivos se importan entre sí" : `Ciclo entre ${fmt(m.files)} archivos`}`;
    case "folder_cycle":
      return `${rule}Ciclo entre ${fmt(m.folders)} carpetas hermanas`;
    case "layer_violation": {
      const [from, to] = f.id.slice(f.id.indexOf(":") + 1).split("->");
      return `Regla rota: la capa ${from ?? "?"} no puede depender de ${to ?? "?"} (${fmt(m.imports)} ${m.imports === 1 ? "import" : "imports"})`;
    }
    case "fan_out_limit":
      return `Regla rota: depende de ${fmt(m.fan_out)} archivos (máximo ${fmt(m.max)})`;
    case "god_module":
      return `Archivo dios: ${fmt(m.loc)} líneas, lo usan ${fmt(m.fan_in)} y usa ${fmt(m.fan_out)}`;
    case "hub":
      return `Cuello de botella: en el ${Math.round((m.share ?? 0) * 1000) / 10} % de los caminos de dependencias`;
    case "sdp":
      return "Una carpeta estable depende de otra inestable";
    case "orphan":
      if (f.severity === "info") return "Solo lo usan las pruebas";
      return f.nodes[0]?.startsWith("dir:") || f.nodes[0] === ROOT_ID ? "Paquete que nadie importa" : "Nadie lo usa: ¿código muerto?";
    case "deep_chain":
      return `Cadena de dependencias de ${fmt(m.depth)} pasos`;
    case "hidden_coupling":
      return `Acoplamiento oculto: cambian juntos en ${fmt(m.together)} commits sin importarse`;
    default:
      return f.title;
  }
}

export interface FindingLine {
  /** Unique within the lines (the finding's id), for React keys. */
  key: string;
  text: string;
  alert: boolean;
  /** The file or folder the finding is about, when it is not the card `at` itself. */
  where?: string;
}

/** The first location of `f` inside the card or box `at` (any location without `at`). */
function locationIn(f: Finding, at: string | undefined): string | undefined {
  if (at === undefined || at === ROOT_ID) return f.nodes[0];
  const prefix = `${at.slice(at.indexOf(":") + 1)}/`;
  return f.nodes.find((n) => n === at || n.slice(n.indexOf(":") + 1).startsWith(prefix)) ?? f.nodes[0];
}

/**
 * Tooltip and detail lines for the findings on a card, problems first, at most `max`. With
 * `at` (the card or box they sit on), each line says which file or folder inside it it is
 * about: two orphans in one folder read the same otherwise.
 */
export function findingLines(list: readonly Finding[] | undefined, max = 3, at?: string): FindingLine[] {
  if (!list || list.length === 0) return [];
  const sorted = [...list].sort((a, b) => Number(isProblem(b)) - Number(isProblem(a)));
  const out: FindingLine[] = sorted.slice(0, max).map((f) => {
    const loc = locationIn(f, at);
    const line: FindingLine = { key: f.id, text: findingLabel(f), alert: isProblem(f) };
    if (at !== undefined && loc !== undefined && loc !== at) line.where = placeOf(loc);
    return line;
  });
  if (sorted.length > max) out.push({ key: "more", text: `y ${sorted.length - max} hallazgos más`, alert: false });
  return out;
}
