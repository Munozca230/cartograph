// Direction of dependencies on the plan. The plan draws every dependency pointing down,
// so a cycle can only be drawn by letting some edges point up. Those are the edges that go
// "contra la corriente" (against the flow): the lightest set whose reversal leaves no cycle.
// Layout and coloring both use `upstreamEdges`, so what is magenta is exactly what points up.

export interface WeightedEdge {
  src: string;
  dst: string;
  weight: number;
}

/** Key of a directed pair. NUL can't appear in a path, so the key is unambiguous. */
export function edgeKey(src: string, dst: string): string {
  return `${src}\u0000${dst}`;
}

const byId = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);

function sortedEdges<E extends WeightedEdge>(edges: readonly E[]): E[] {
  return [...edges].sort((a, b) => byId(a.src, b.src) || byId(a.dst, b.dst));
}

/**
 * Strongly connected components with more than one node (Tarjan, iterative).
 * Deterministic: each component is sorted, and components are sorted by their first id.
 */
export function cycles(ids: readonly string[], edges: readonly WeightedEdge[]): string[][] {
  const nodes = [...new Set(ids)].sort(byId);
  const adj = new Map<string, string[]>(nodes.map((id) => [id, []]));
  for (const e of sortedEdges(edges)) {
    if (e.src !== e.dst && adj.has(e.src) && adj.has(e.dst)) adj.get(e.src)!.push(e.dst);
  }
  let counter = 0;
  const index = new Map<string, number>();
  const low = new Map<string, number>();
  const onStack = new Set<string>();
  const stack: string[] = [];
  const out: string[][] = [];
  for (const start of nodes) {
    if (index.has(start)) continue;
    const work: { v: string; i: number }[] = [{ v: start, i: 0 }];
    index.set(start, counter);
    low.set(start, counter);
    counter += 1;
    stack.push(start);
    onStack.add(start);
    while (work.length) {
      const frame = work[work.length - 1]!;
      const next = adj.get(frame.v)!;
      if (frame.i < next.length) {
        const w = next[frame.i]!;
        frame.i += 1;
        if (!index.has(w)) {
          index.set(w, counter);
          low.set(w, counter);
          counter += 1;
          stack.push(w);
          onStack.add(w);
          work.push({ v: w, i: 0 });
        } else if (onStack.has(w)) {
          low.set(frame.v, Math.min(low.get(frame.v)!, index.get(w)!));
        }
        continue;
      }
      work.pop();
      const parent = work[work.length - 1];
      if (parent) low.set(parent.v, Math.min(low.get(parent.v)!, low.get(frame.v)!));
      if (low.get(frame.v) === index.get(frame.v)) {
        const comp: string[] = [];
        let w: string;
        do {
          w = stack.pop()!;
          onStack.delete(w);
          comp.push(w);
        } while (w !== frame.v);
        if (comp.length > 1) out.push(comp.sort(byId));
      }
    }
  }
  return out.sort((a, b) => byId(a[0]!, b[0]!));
}

/**
 * Weighted greedy feedback arc set (Eades, Lin and Smyth), run inside each cycle only, so
 * every returned edge lies on a cycle. Reversing the returned edges leaves the graph acyclic.
 * Ties break by id, so the answer doesn't depend on the order of the input.
 */
export function feedbackArcs(ids: readonly string[], edges: readonly WeightedEdge[]): Set<string> {
  const result = new Set<string>();
  const all = sortedEdges(edges).filter((e) => e.src !== e.dst);
  for (const comp of cycles(ids, all)) {
    const members = new Set(comp);
    const inner = all.filter((e) => members.has(e.src) && members.has(e.dst));
    const out = new Map<string, Map<string, number>>(comp.map((id) => [id, new Map()]));
    const inn = new Map<string, Map<string, number>>(comp.map((id) => [id, new Map()]));
    for (const e of inner) {
      out.get(e.src)!.set(e.dst, (out.get(e.src)!.get(e.dst) ?? 0) + e.weight);
      inn.get(e.dst)!.set(e.src, (inn.get(e.dst)!.get(e.src) ?? 0) + e.weight);
    }
    const alive = new Set(comp);
    const live = (m: Map<string, number>) => {
      let s = 0;
      for (const [k, w] of m) if (alive.has(k)) s += w;
      return s;
    };
    const head: string[] = [];
    const tail: string[] = [];
    while (alive.size) {
      let changed = true;
      while (changed) {
        changed = false;
        for (const v of comp) {
          if (alive.has(v) && live(out.get(v)!) === 0) {
            tail.unshift(v);
            alive.delete(v);
            changed = true;
          }
        }
        for (const v of comp) {
          if (alive.has(v) && live(inn.get(v)!) === 0) {
            head.push(v);
            alive.delete(v);
            changed = true;
          }
        }
      }
      if (!alive.size) break;
      let best: string | null = null;
      let bestDelta = -Infinity;
      for (const v of comp) {
        if (!alive.has(v)) continue;
        const delta = live(out.get(v)!) - live(inn.get(v)!);
        if (delta > bestDelta) {
          bestDelta = delta;
          best = v;
        }
      }
      head.push(best!);
      alive.delete(best!);
    }
    const pos = new Map([...head, ...tail].map((v, i) => [v, i]));
    for (const e of inner) if (pos.get(e.src)! > pos.get(e.dst)!) result.add(edgeKey(e.src, e.dst));
  }
  return result;
}

/** Where an edge sits in the nesting of open folders: the box holding both ends, and the two children of that box that hold each end. */
export interface Lifted {
  /** The innermost box holding both ends (`ROOT` for the plan itself). */
  container: string;
  src: string;
  dst: string;
}

export const ROOT = "";

/**
 * `chain(id)` lists the open boxes that hold `id`, outermost first, and ends with `id`
 * itself. Two ends of an edge are never one inside the other (they are cards of one
 * frontier), so their chains part somewhere: the pair there is the edge as seen from the
 * box that holds both.
 */
export function liftEdge(chain: (id: string) => readonly string[], src: string, dst: string): Lifted {
  const a = chain(src);
  const b = chain(dst);
  let i = 0;
  while (i < a.length - 1 && i < b.length - 1 && a[i] === b[i]) i += 1;
  return { container: i === 0 ? ROOT : a[i - 1]!, src: a[i]!, dst: b[i]! };
}

/**
 * The edges drawn "contra la corriente" on a plan with any number of nested open boxes.
 *
 * Boxes are solid: nothing outside a box can sit between two cards inside it. So the order
 * is decided box by box: in each box, its children (cards and inner boxes, each as one node)
 * with the edges lifted onto them, and an edge points up exactly when its lifted pair does
 * in the box that holds both ends. Reversing those edges leaves every box acyclic, so the
 * layered layout can draw every other edge downward. With no box open this is the plain
 * feedback arc set of the view.
 */
export function upstreamEdges(edges: readonly WeightedEdge[], chain: (id: string) => readonly string[]): Set<string> {
  const list = sortedEdges(edges).filter((e) => e.src !== e.dst);
  const boxes = new Map<string, Map<string, WeightedEdge>>();
  const lifted = list.map((e) => {
    const l = liftEdge(chain, e.src, e.dst);
    let q = boxes.get(l.container);
    if (!q) boxes.set(l.container, (q = new Map()));
    const k = edgeKey(l.src, l.dst);
    const prev = q.get(k);
    if (prev) prev.weight += e.weight;
    else q.set(k, { src: l.src, dst: l.dst, weight: e.weight });
    return { e, l };
  });
  const flagged = new Map<string, Set<string>>();
  for (const [box, q] of boxes) {
    const qs = [...q.values()];
    flagged.set(box, feedbackArcs([...new Set(qs.flatMap((x) => [x.src, x.dst]))], qs));
  }
  const result = new Set<string>();
  for (const { e, l } of lifted) if (flagged.get(l.container)!.has(edgeKey(l.src, l.dst))) result.add(edgeKey(e.src, e.dst));
  return result;
}
