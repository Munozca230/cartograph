// What the plan opens: a level ("Nivel 1 · 2 · 3 · Todo") plus the folders the user opened
// below it or closed inside it. Any number of folders can be open at once; /api/graph gets
// the level as `depth` and the overrides as `expanded` / `collapsed`, so "Todo" on a repo
// with thousands of folders is still a short URL. The rule mirrors the server's:
// a folder is open when its parent is open, it isn't closed, and it lies above the level
// (top-level folders are level 1) or was opened.
import type { GraphQuery } from "../api/types";
import { ancestors, canonicalId, isDir, ROOT_ID, type Hierarchy } from "./hierarchy";

/** A depth (1 opens nothing, 2 the top-level folders, …) or every folder. */
export type Level = number | "all";

export const LEVELS: readonly Level[] = [1, 2, 3, "all"];

export interface ViewSpec {
  level: Level;
  /** Folders opened below the level. Sorted. */
  open: string[];
  /** Folders closed inside the level. Sorted. */
  closed: string[];
}

export const TOP_LEVEL: ViewSpec = { level: 1, open: [], closed: [] };

const sorted = (ids: Iterable<string>) => [...new Set(ids)].sort();

/** A stable string for a view, to compare requests. */
export function viewKey(v: ViewSpec): string {
  return `${v.level}|${v.open.join("\n")}|${v.closed.join("\n")}`;
}

export function sameView(a: ViewSpec, b: ViewSpec): boolean {
  return viewKey(a) === viewKey(b);
}

/** The query for /api/graph. */
export function graphQuery(v: ViewSpec): GraphQuery {
  return { depth: v.level, expanded: v.open, collapsed: v.closed };
}

function byLevel(level: Level, depth: number): boolean {
  return level === "all" || depth < level;
}

/** The open folders of a view (never the root), in tree order. */
export function openDirs(h: Hierarchy, v: ViewSpec): string[] {
  const open = new Set(v.open);
  const closed = new Set(v.closed);
  const out: string[] = [];
  const walk = (id: string) => {
    for (const c of h.nodes.get(id)!.children) {
      const n = h.nodes.get(c)!;
      if (n.kind === "file") continue;
      if (!closed.has(c) && (byLevel(v.level, n.depth) || open.has(c))) {
        out.push(c);
        walk(c);
      }
    }
  };
  walk(h.root);
  return out;
}

/** Whether a folder is open in a view. */
export function isOpen(h: Hierarchy, v: ViewSpec, id: string): boolean {
  const cid = canonicalId(id);
  if (cid === h.root) return true;
  const n = h.nodes.get(cid);
  if (!n || n.kind === "file") return false;
  if (v.closed.includes(cid)) return false;
  if (!byLevel(v.level, n.depth) && !v.open.includes(cid)) return false;
  return n.parent === null || isOpen(h, v, n.parent);
}

/** Drops ids that no longer name a folder (a re-index deleted them) and redundant ones. */
export function resolveView(h: Hierarchy, v: ViewSpec): ViewSpec {
  const dirs = (ids: string[]) => ids.map(canonicalId).filter((id) => id !== ROOT_ID && isDir(h, id));
  return {
    level: v.level,
    open: sorted(dirs(v.open).filter((id) => !byLevel(v.level, h.nodes.get(id)!.depth))),
    closed: sorted(dirs(v.closed).filter((id) => byLevel(v.level, h.nodes.get(id)!.depth))),
  };
}

/** A level with nothing opened or closed by hand. */
export function atLevel(level: Level): ViewSpec {
  return { level, open: [], closed: [] };
}

/** Opens a folder and, so that it shows, every folder above it. */
export function openFolder(h: Hierarchy, v: ViewSpec, id: string): ViewSpec {
  const cid = canonicalId(id);
  if (!isDir(h, cid) || cid === h.root) return v;
  const chain = [cid, ...ancestors(h, cid).filter((a) => a !== h.root)];
  const open = new Set(v.open);
  const closed = new Set(v.closed);
  for (const d of chain) {
    closed.delete(d);
    if (!byLevel(v.level, h.nodes.get(d)!.depth)) open.add(d);
  }
  return { level: v.level, open: sorted(open), closed: sorted(closed) };
}

/** Closes a folder. What was opened or closed under it is forgotten. */
export function closeFolder(h: Hierarchy, v: ViewSpec, id: string): ViewSpec {
  const cid = canonicalId(id);
  if (!isDir(h, cid) || cid === h.root) return v;
  const under = (x: string) => x === cid || ancestors(h, x).includes(cid);
  const open = v.open.filter((x) => !under(x));
  const closed = v.closed.filter((x) => !under(x));
  if (byLevel(v.level, h.nodes.get(cid)!.depth)) closed.push(cid);
  return { level: v.level, open: sorted(open), closed: sorted(closed) };
}

/** Opens a closed folder, closes an open one. */
export function toggleFolder(h: Hierarchy, v: ViewSpec, id: string): ViewSpec {
  return isOpen(h, v, id) ? closeFolder(h, v, id) : openFolder(h, v, id);
}

/**
 * The view that shows `id` on the plan: every folder above it opens (it shows as a card, or
 * as a box if it is an open folder). Unchanged when it already shows.
 */
export function revealView(h: Hierarchy, v: ViewSpec, id: string): ViewSpec {
  const cid = canonicalId(id);
  const n = h.nodes.get(cid);
  if (!n || n.parent === null || n.parent === h.root || isOpen(h, v, n.parent)) return v;
  return openFolder(h, v, n.parent);
}

/** A folder with something inside: opening it changes the plan. */
export function canOpenFolder(h: Hierarchy, id: string): boolean {
  const n = h.nodes.get(canonicalId(id));
  return n !== undefined && (n.kind === "dir" || n.kind === "repo") && n.children.length > 0;
}

/**
 * The first screen: level 1, with the chain of folders that clearly hold their parent's code
 * opened (a typical app is one big `src` next to config files). A folder qualifies when it
 * has at least half of its parent's files and two thirds of the files in its parent's
 * subfolders. We stop at a folder with more than `budget` children, or where no subfolder
 * dominates (that is where the parts are).
 */
export function initialView(h: Hierarchy, budget = 30): ViewSpec {
  let current = h.nodes.get(h.root)!;
  let deepest: string | null = null;
  for (let guard = 0; guard < 64; guard++) {
    const dirs = current.children
      .map((id) => h.nodes.get(id)!)
      .filter((n) => n.kind === "dir" && n.children.length > 0)
      .sort((a, b) => b.files - a.files || (a.id < b.id ? -1 : 1));
    const best = dirs[0];
    const inDirs = dirs.reduce((s, n) => s + n.files, 0);
    if (!best || best.files * 2 < current.files || best.files * 3 < inDirs * 2 || best.children.length > budget) break;
    deepest = best.id;
    current = best;
  }
  return deepest === null ? TOP_LEVEL : openFolder(h, TOP_LEVEL, deepest);
}

/** Number of cards a view puts on the plan (its frontier), from the tree alone. */
export function frontierSize(h: Hierarchy, v: ViewSpec): number {
  const open = new Set(openDirs(h, v));
  open.add(h.root);
  let n = 0;
  for (const id of open) for (const c of h.nodes.get(id)!.children) if (!open.has(c)) n += 1;
  return n;
}
