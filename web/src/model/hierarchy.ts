// Client-side view of the directory hierarchy and of the frontier semantics of /api/graph.
// The server decides the frontier; the client needs the same rules to know which node
// stands for a file that isn't visible and which folders a view opens (view.ts).
import type { TreeNode } from "../api/types";

export interface HNode {
  id: string;
  kind: TreeNode["kind"];
  name: string;
  path: string;
  parent: string | null;
  depth: number;
  children: string[];
  loc: number;
  files: number;
  fanIn: number;
  fanOut: number;
  unresolved: number;
  lang?: string;
  langs?: Record<string, number>;
}

export interface Hierarchy {
  root: string;
  nodes: Map<string, HNode>;
}

export const ROOT_ID = "repo:";

/** Normalizes ids the server accepts as aliases. */
export function canonicalId(id: string): string {
  if (id === "dir:" || id === "dir") return ROOT_ID;
  if (id.startsWith("package:")) return `pkg:${id.slice(8)}`;
  if (id.startsWith("external:")) return `ext:${id.slice(9)}`;
  return id;
}

export function buildHierarchy(root: TreeNode): Hierarchy {
  const nodes = new Map<string, HNode>();
  const visit = (n: TreeNode, parent: string | null, depth: number) => {
    const id = canonicalId(n.id);
    const h: HNode = {
      id,
      kind: n.kind,
      name: n.name,
      path: n.path,
      parent,
      depth,
      children: [],
      loc: n.loc,
      files: n.files ?? (n.kind === "file" ? 1 : 0),
      fanIn: n.fan_in,
      fanOut: n.fan_out,
      unresolved: n.unresolved,
      ...(n.lang ? { lang: n.lang } : {}),
      ...(n.langs ? { langs: n.langs } : {}),
    };
    nodes.set(id, h);
    for (const c of n.children ?? []) {
      h.children.push(canonicalId(c.id));
      visit(c, id, depth + 1);
    }
  };
  visit(root, null, 0);
  return { root: canonicalId(root.id), nodes };
}

export function isDir(h: Hierarchy, id: string): boolean {
  const k = h.nodes.get(id)?.kind;
  return k === "dir" || k === "repo";
}

/** Ancestors from the parent up to the root (root last). */
export function ancestors(h: Hierarchy, id: string): string[] {
  const out: string[] = [];
  let cur = h.nodes.get(id)?.parent ?? null;
  while (cur) {
    out.push(cur);
    cur = h.nodes.get(cur)?.parent ?? null;
  }
  return out;
}

/** Drops ids the server would ignore: unknown, not dirs, or whose parent is not expanded. Sorted. */
export function normalizeExpanded(h: Hierarchy, expanded: Iterable<string>): string[] {
  const set = new Set([...expanded].map(canonicalId).filter((id) => id !== ROOT_ID && isDir(h, id)));
  const ok = (id: string): boolean => {
    const parent = h.nodes.get(id)!.parent;
    return parent === h.root || (parent !== null && set.has(parent) && ok(parent));
  };
  return [...set].filter(ok).sort();
}

/** The frontier for an expanded set, in tree order (mirrors /api/graph). */
export function frontier(h: Hierarchy, expanded: Iterable<string>): string[] {
  const open = new Set(normalizeExpanded(h, expanded));
  open.add(h.root);
  const out: string[] = [];
  const walk = (id: string) => {
    for (const c of h.nodes.get(id)!.children) {
      if (open.has(c)) walk(c);
      else out.push(c);
    }
  };
  walk(h.root);
  return out;
}


/** Id for a path reported by /api/issues (a path, or already an id). */
export function idForIssuePath(h: Hierarchy, path: string): string {
  if (/^(file|dir|repo|ext|pkg|package|external):/.test(path)) return canonicalId(path);
  if (h.nodes.has(`file:${path}`)) return `file:${path}`;
  if (h.nodes.has(`dir:${path}`)) return `dir:${path}`;
  return `file:${path}`;
}
