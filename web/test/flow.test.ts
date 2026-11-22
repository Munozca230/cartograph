import { describe, expect, it } from "vitest";
import { cycles, edgeKey, feedbackArcs, liftEdge, ROOT, upstreamEdges, type WeightedEdge } from "../src/model/flow";

const E = (src: string, dst: string, weight = 1): WeightedEdge => ({ src, dst, weight });

/** True if the graph has no cycle once the `flipped` edges are reversed. */
function acyclicAfter(ids: string[], edges: WeightedEdge[], flipped: Set<string>): boolean {
  const oriented = edges.map((e) => (flipped.has(edgeKey(e.src, e.dst)) ? E(e.dst, e.src, e.weight) : e));
  return cycles(ids, oriented).length === 0;
}

function shuffle<T>(xs: T[], seed: number): T[] {
  const out = [...xs];
  let s = seed;
  for (let i = out.length - 1; i > 0; i--) {
    s = (s * 1103515245 + 12345) % 2147483648;
    const j = s % (i + 1);
    [out[i], out[j]] = [out[j]!, out[i]!];
  }
  return out;
}

// Six folders in one cycle, a two-way pair inside it, and nodes outside any cycle.
const ids = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "entry", "leaf"];
const edges = [
  E("entry", "beta", 5),
  E("entry", "alpha", 8),
  E("beta", "alpha", 25),
  E("alpha", "beta", 15),
  E("alpha", "gamma", 10),
  E("gamma", "delta", 4),
  E("delta", "beta", 2),
  E("alpha", "epsilon", 3),
  E("epsilon", "alpha", 2),
  E("epsilon", "zeta", 6),
  E("zeta", "alpha", 1),
  E("alpha", "leaf", 7),
];

describe("contra la corriente", () => {
  it("breaks every cycle by flipping only edges that lie on one", () => {
    const flagged = feedbackArcs(ids, edges);
    expect(flagged.size).toBeGreaterThan(0);
    expect(acyclicAfter(ids, edges, flagged)).toBe(true);
    const comps = cycles(ids, edges);
    for (const k of flagged) {
      const [s, d] = k.split("\u0000");
      expect(comps.some((c) => c.includes(s!) && c.includes(d!))).toBe(true);
    }
    // Edges outside any cycle are never flagged.
    expect(flagged.has(edgeKey("entry", "alpha"))).toBe(false);
    expect(flagged.has(edgeKey("alpha", "leaf"))).toBe(false);
  });

  it("flips the lighter direction of a two-way pair", () => {
    const flagged = feedbackArcs(["a", "b"], [E("a", "b", 30), E("b", "a", 20)]);
    expect([...flagged]).toEqual([edgeKey("b", "a")]);
  });

  it("does not depend on the order of the input", () => {
    const want = [...feedbackArcs(ids, edges)].sort();
    for (const seed of [1, 7, 42]) {
      expect([...feedbackArcs(shuffle(ids, seed), shuffle(edges, seed + 1))].sort()).toEqual(want);
    }
    expect(cycles(shuffle(ids, 3), shuffle(edges, 4))).toEqual(cycles(ids, edges));
  });

  it("treats the open folder as a solid box: a node can't sit between two of its cards", () => {
    // a and b are inside the box; x is outside. a → x → b is no flat cycle, but the box and x
    // depend on each other, so one of the two crossing edges has to point up.
    const inBox = (id: string) => (id === "a" || id === "b" ? ["box", id] : [id]);
    const flagged = upstreamEdges([E("a", "x", 5), E("x", "b", 2)], inBox);
    expect([...flagged]).toEqual([edgeKey("x", "b")]);
    // Without a box there is nothing to flip.
    expect(upstreamEdges([E("a", "x", 5), E("x", "b", 2)], (id) => [id]).size).toBe(0);
  });

  it("decides each nested box on its own children", () => {
    // outer holds inner and c; inner holds a and b. a → c → b closes a cycle only at the
    // level of outer (inner ⇄ c); inside inner, a and b don't touch.
    const chain = (id: string) => (id === "a" || id === "b" ? ["outer", "inner", id] : id === "c" ? ["outer", "c"] : [id]);
    const es = [E("a", "c", 4), E("c", "b", 1), E("z", "a", 3)];
    expect([...upstreamEdges(es, chain)]).toEqual([edgeKey("c", "b")]);
    expect(liftEdge(chain, "a", "c")).toEqual({ container: "outer", src: "inner", dst: "c" });
    expect(liftEdge(chain, "z", "a")).toEqual({ container: ROOT, src: "z", dst: "outer" });
    expect(liftEdge(chain, "a", "b")).toEqual({ container: "inner", src: "a", dst: "b" });
  });

  it("leaves an acyclic view alone", () => {
    expect(feedbackArcs(["a", "b", "c"], [E("a", "b"), E("b", "c"), E("a", "c")]).size).toBe(0);
  });
});
