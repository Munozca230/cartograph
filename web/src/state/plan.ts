// The plan for the current state, memoized, and its layout computed by ELK in a Web Worker.
// The worker script is elkjs' own (`elk-worker.min.js`), emitted by Vite as an asset of
// web/dist, so the binary serves it like any other file: no CDN.
import ELK from "elkjs/lib/elk-api.js";
import workerUrl from "elkjs/lib/elk-worker.min.js?url";
import { findingsOnPlan, isProblem, NO_FINDINGS, type FindingsOnPlan } from "../model/findings";
import { kindsOf, type Kinds } from "../model/kind";
import { buildPlan, layoutPlan, parentOf, type ElkLike, type Plan, type PlanLayout } from "../model/plan";
import { useAppState, type AppState } from "./store";

let memo: { graph: AppState["graph"]; all: boolean; iso: boolean; ext: boolean; plan: Plan } | null = null;

/** The plan of the graph on screen (stable identity while its inputs don't change). */
export function planOf(s: AppState): Plan | null {
  if (!s.graph) return null;
  if (memo && memo.graph === s.graph && memo.all === s.showAllEdges && memo.iso === s.showIsolated && memo.ext === s.showExternals) return memo.plan;
  const plan = buildPlan(s.graph, { showAllEdges: s.showAllEdges, showIsolated: s.showIsolated, showExternals: s.showExternals });
  memo = { graph: s.graph, all: s.showAllEdges, iso: s.showIsolated, ext: s.showExternals, plan };
  return plan;
}

export function usePlan(): Plan | null {
  return useAppState(planOf);
}

let findingsMemo: { plan: Plan; findings: AppState["findings"]; out: FindingsOnPlan } | null = null;

/** Findings placed on the plan on screen (memoized on the plan and /api/findings). */
export function findingsFor(s: AppState): FindingsOnPlan {
  const plan = planOf(s);
  if (!plan || !s.findings) return NO_FINDINGS;
  if (findingsMemo && findingsMemo.plan === plan && findingsMemo.findings === s.findings) return findingsMemo.out;
  const out = findingsOnPlan(plan, s.findings.findings);
  findingsMemo = { plan, findings: s.findings, out };
  return out;
}

export function useFindings(): FindingsOnPlan {
  return useAppState(findingsFor);
}

let problemsMemo: { plan: Plan; issues: AppState["issues"]; found: FindingsOnPlan; out: Map<string, number> } | null = null;
let issuesMemo: { plan: Plan; issues: AppState["issues"]; out: Map<string, number> } | null = null;
const NO_PROBLEMS = new Map<string, number>();

/**
 * Problems per card or box of the plan: what `issuesOf` counts plus the findings that are
 * problems (errors and warnings, once per finding), each on the card or box that holds it.
 * This drives the magenta dot.
 */
export function problemsOf(s: AppState): Map<string, number> {
  const plan = planOf(s);
  if (!plan) return NO_PROBLEMS;
  const found = findingsFor(s);
  if (problemsMemo && problemsMemo.plan === plan && problemsMemo.issues === s.issues && problemsMemo.found === found) return problemsMemo.out;
  const out = new Map(issuesOf(s));
  for (const [id, list] of found.byId) {
    const n = list.filter(isProblem).length;
    if (n > 0) out.set(id, (out.get(id) ?? 0) + n);
  }
  problemsMemo = { plan, issues: s.issues, found, out };
  return out;
}

/**
 * Indexing problems per card or box: imports that don't resolve (the graph counts them
 * per node), plus diagnostics and skipped files from /api/issues, each counted on the
 * nearest card or box that holds its path (a skipped file is not in the tree).
 */
export function issuesOf(s: AppState): Map<string, number> {
  const plan = planOf(s);
  if (!plan) return NO_PROBLEMS;
  if (issuesMemo && issuesMemo.plan === plan && issuesMemo.issues === s.issues) return issuesMemo.out;
  const out = new Map<string, number>();
  const where = new Set([...plan.nodes.map((n) => n.id), ...plan.boxes.map((b) => b.id)]);
  const add = (id: string, n: number) => out.set(id, (out.get(id) ?? 0) + n);
  for (const n of plan.nodes) if (n.unresolved > 0) add(n.id, n.unresolved);
  const place = (path: string) => {
    for (let cur: string | null = `file:${path}`; cur !== null; cur = parentOf(cur)) {
      if (where.has(cur)) return add(cur, 1);
      const dir = `dir:${cur.slice(cur.indexOf(":") + 1)}`;
      if (cur.startsWith("file:") && where.has(dir)) return add(dir, 1);
    }
  };
  for (const d of s.issues?.diagnostics ?? []) place(d.path);
  for (const k of s.issues?.skipped ?? []) place(k.path);
  issuesMemo = { plan, issues: s.issues, out };
  return out;
}

export function useIssueCounts(): Map<string, number> {
  return useAppState(issuesOf);
}

export function useProblems(): Map<string, number> {
  return useAppState(problemsOf);
}

let kindsMemo: { hierarchy: AppState["hierarchy"]; out: Kinds | null } | null = null;

/** What every file and folder of the tree is (code, tests, scripts…), memoized on the tree. */
export function kindsFor(s: AppState): Kinds | null {
  if (kindsMemo && kindsMemo.hierarchy === s.hierarchy) return kindsMemo.out;
  kindsMemo = { hierarchy: s.hierarchy, out: s.hierarchy ? kindsOf(s.hierarchy) : null };
  return kindsMemo.out;
}

export function useKinds(): Kinds | null {
  return useAppState(kindsFor);
}

let elk: ElkLike | null = null;

function engine(): ElkLike {
  elk ??= new ELK({ workerUrl }) as unknown as ElkLike;
  return elk;
}

let last: PlanLayout | null = null;

/**
 * Layout for a plan. A plan with the same key as the last one (same cards, same edges
 * in the same direction, same boxes) reuses that layout: a re-index that only changes numbers doesn't move anything.
 */
export async function layoutFor(plan: Plan): Promise<PlanLayout> {
  if (last && last.key === plan.key) return last;
  const layout = await layoutPlan(engine(), plan);
  last = layout;
  return layout;
}
