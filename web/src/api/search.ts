// Adapter between /api/search and the command palette.
import type { ApiClient } from "./client";
import type { SearchResult } from "./types";

export interface PaletteResult {
  /** Node id, used as the command value. */
  value: string;
  label: string;
  /** Text the palette shows under the label: the parent path, or the ecosystem for packages. */
  context: string;
  kind: SearchResult["kind"];
  group: "Carpetas" | "Archivos" | "Paquetes externos";
  /** [start, end) ranges of `label` that match the query, for highlighting. */
  matches: [number, number][];
}

const GROUP: Record<SearchResult["kind"], PaletteResult["group"]> = {
  dir: "Carpetas",
  file: "Archivos",
  ext: "Paquetes externos",
};

const GROUP_ORDER: PaletteResult["group"][] = ["Carpetas", "Archivos", "Paquetes externos"];

/** Ranges of `text` matched by `query`: one contiguous range if possible, else a subsequence. */
export function matchRanges(text: string, query: string): [number, number][] {
  const q = query.trim().toLowerCase();
  if (!q) return [];
  const t = text.toLowerCase();
  const at = t.indexOf(q);
  if (at >= 0) return [[at, at + q.length]];
  const ranges: [number, number][] = [];
  let qi = 0;
  for (let i = 0; i < t.length && qi < q.length; i++) {
    if (t[i] !== q[qi]) continue;
    qi += 1;
    const last = ranges[ranges.length - 1];
    if (last && last[1] === i) last[1] = i + 1;
    else ranges.push([i, i + 1]);
  }
  return qi === q.length ? ranges : [];
}

export function toPaletteResult(r: SearchResult, query: string): PaletteResult {
  let context: string;
  if (r.kind === "ext") {
    const colon = r.path.indexOf(":");
    context = colon > 0 ? `paquete ${r.path.slice(0, colon)}` : "paquete externo";
  } else {
    const slash = r.path.lastIndexOf("/");
    context = slash > 0 ? r.path.slice(0, slash) : "raíz del repositorio";
  }
  return { value: r.id, label: r.name, context, kind: r.kind, group: GROUP[r.kind], matches: matchRanges(r.name, query) };
}

/** Keeps the server's ranking inside each group and orders groups dirs, files, packages. */
export function groupResults(results: PaletteResult[]): { group: PaletteResult["group"]; items: PaletteResult[] }[] {
  return GROUP_ORDER.map((group) => ({ group, items: results.filter((r) => r.group === group) })).filter(
    (g) => g.items.length > 0,
  );
}

export function createSearch(client: Pick<ApiClient, "search">, limit = 40) {
  return async (query: string, { signal }: { signal: AbortSignal }): Promise<PaletteResult[]> => {
    const q = query.trim();
    if (!q) return [];
    const res = await client.search(q, limit, signal);
    const seen = new Set<string>();
    const out: PaletteResult[] = [];
    for (const r of res.results) {
      if (seen.has(r.id)) continue;
      seen.add(r.id);
      out.push(toPaletteResult(r, q));
    }
    return out;
  };
}
