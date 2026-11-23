// Workspace members on the plan: each member is a top-level folder named after it, drawn
// with its own hue and its branch, so the repositories read apart at a glance.

import type { Meta, WorkspaceMember } from "../api/types";

export interface MemberCard extends WorkspaceMember {
  /** Hue (degrees) of the member: spread evenly around the wheel, in member order. */
  hue: number;
}

/** The members of a workspace index by the path of their folder (empty for a single repo). */
export function membersByPath(meta: Pick<Meta, "workspace"> | null): Map<string, MemberCard> {
  const list = meta?.workspace?.members ?? [];
  const out = new Map<string, MemberCard>();
  list.forEach((m, i) => out.set(m.name, { ...m, hue: Math.round((i * 360) / list.length + 200) % 360 }));
  return out;
}

/** What the badge of a member's card says: its branch, a short head when detached. */
export function memberBadge(m: WorkspaceMember): string {
  if (m.git === "no_git") return "sin git";
  if (m.detached && m.head) return m.head.slice(0, 8);
  return m.branch ?? "";
}

/** The tooltip line of a member's card. */
export function memberLine(m: WorkspaceMember): string {
  const where = m.path === m.name ? "" : ` · carpeta ${m.path}`;
  const badge = memberBadge(m);
  return `Repositorio ${m.name}${where}${badge ? ` · ${m.git === "no_git" ? badge : `rama ${badge}`}` : ""}`;
}
