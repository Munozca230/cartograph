// Links into the map: `?view=plan&focus=dir:src` opens the plan on a folder or file,
// `?view=flows&focus=sym:src/a.rs#run` the flows at a function. Agents build them (the MCP
// tool `open_map`); the page may write them back as the selection changes, so a URL can be
// shared. Other query parameters (`debug`) are kept.
import { canonicalId, type Hierarchy } from "./hierarchy";

export type LinkView = "plan" | "flows";

export interface DeepLink {
  /** The view asked for, or null (then the focus decides, see linkView). */
  view: LinkView | null;
  /** An id (`file:`, `dir:`, `sym:`, `ext:`) or a repo path; null = none. */
  focus: string | null;
}

export const NO_LINK: DeepLink = { view: null, focus: null };

/** The link in a query string (`?view=…&focus=…`). Unknown views count as none. */
export function parseDeepLink(search: string): DeepLink {
  const q = new URLSearchParams(search);
  const v = q.get("view")?.trim().toLowerCase() ?? "";
  const view: LinkView | null = v === "plan" || v === "flows" ? v : null;
  const focus = q.get("focus")?.trim() || null;
  return { view, focus };
}

export function hasLink(link: DeepLink): boolean {
  return link.view !== null || link.focus !== null;
}

/** The view a link opens: the one it names, else the flows for a function, else the plan. */
export function linkView(link: DeepLink): LinkView {
  return link.view ?? (link.focus?.startsWith("sym:") ? "flows" : "plan");
}

export function sameLink(a: DeepLink, b: DeepLink): boolean {
  return a.view === b.view && a.focus === b.focus;
}

/** `search` with the link's parameters set (or removed when null), others kept. */
export function linkSearch(search: string, link: DeepLink): string {
  // The other parameters as they were written (`?debug` stays `debug`, not `debug=`).
  const rest = search
    .replace(/^\?/, "")
    .split("&")
    .filter((p) => p !== "" && !/^(view|focus)(=|$)/.test(p));
  const parts: string[] = [];
  if (link.view) parts.push(`view=${link.view}`);
  if (link.focus) parts.push(`focus=${encodeFocus(link.focus)}`);
  parts.push(...rest);
  return parts.length ? `?${parts.join("&")}` : "";
}

/** URI-encoded, with `:` and `/` kept readable (both are fine in a query). */
export function encodeFocus(id: string): string {
  return encodeURIComponent(id).replace(/%3A/gi, ":").replace(/%2F/gi, "/");
}

/**
 * The plan node a focus stands for: a file or folder id or path as is, a function's file,
 * a library as is (it has a card only with libraries shown), or null when the tree has no
 * such node.
 */
export function planTarget(h: Hierarchy, focus: string): string | null {
  const raw = focus.trim().replace(/\\/g, "/");
  if (raw.startsWith("ext:") || raw.startsWith("external:")) return canonicalId(raw);
  const id = canonicalId(raw);
  if (h.nodes.has(id)) return id;
  if (raw.startsWith("file:") || raw.startsWith("dir:")) return null;
  const known = (path: string): string | null => {
    const p = path.replace(/^\.\//, "").replace(/\/+$/, "");
    if (p === "" || p === ".") return h.root;
    if (!path.endsWith("/") && h.nodes.has(`file:${p}`)) return `file:${p}`;
    return h.nodes.has(`dir:${p}`) ? `dir:${p}` : null;
  };
  const body = raw.startsWith("sym:") ? raw.slice(4) : raw;
  // A function (`path#name`): its file. A `#` can be in the path too, so try each one.
  for (let i = body.indexOf("#"); i >= 0; i = body.indexOf("#", i + 1)) {
    const file = h.nodes.has(`file:${body.slice(0, i)}`) ? `file:${body.slice(0, i)}` : null;
    if (file) return file;
  }
  return raw.startsWith("sym:") ? null : known(body);
}

/**
 * What the page shows now, as a link: the plan's selection, or where the flow starts. The
 * plan with nothing selected is the page without a link.
 */
export function currentLink(s: { mode: LinkView; selected: string | null; flowFrom: { id: string } | null }): DeepLink {
  if (s.mode === "flows") return { view: "flows", focus: s.flowFrom?.id ?? null };
  const sel = s.selected;
  return sel !== null && /^(file|dir|ext):/.test(sel) ? { view: "plan", focus: sel } : NO_LINK;
}
