// A layered drawing in horizontal swimlanes: calls run left to right through columns shared by
// every lane, and each lane is a band of its own, stacked under the one before it. A lane is
// as wide and as tall as its cards need, so a flow keeps the proportions of a screen instead
// of growing one column per lane. Pure and synchronous: the same input gives the same drawing.
//
// Steps (Sugiyama): cycles broken by a DFS from the start; columns by longest path, then cards
// that call nothing moved right out of a lane's tallest column while the drawing is taller
// than the screen's shape; edges that skip columns get a waypoint in each column they cross;
// order inside each lane's part of a column by barycenter sweeps; heights by isotonic
// regression towards the cards each one is linked to, inside the lane's tallest column; then
// edges routed orthogonally through the channels between columns, each one on a track of its
// own (all the calls of a card share one trunk).
import type { Point, Rect } from "./plan";

export interface LaneNode {
  id: string;
  lane: string;
  w: number;
  h: number;
}

export interface LaneEdge {
  id: string;
  src: string;
  dst: string;
}

export interface SwimlaneOptions {
  /** Between two cards (or a card and a passing edge) of one column. */
  rowGap: number;
  /** Narrowest channel between two columns. */
  channelMin: number;
  /** Room between a column and the first track of the channel after it. */
  channelPadLeft: number;
  /** Room between the last track and the next column (the arrowheads). */
  channelPadRight: number;
  /** Between two vertical tracks of a channel. */
  track: number;
  /** Between two lanes. */
  laneGap: number;
  /** Room in a lane around its cards, and above them for its title. */
  lanePad: { side: number; top: number; bottom: number };
  /** Narrowest a lane may be (its title must fit), per lane. */
  laneMinW?: Map<string, number>;
  /** Spacing of the arrows that reach one card. */
  portGap: number;
  /** Width over height the drawing aims at (a stage's): a taller one moves sinks right. */
  aspect?: number;
}

export interface SwimlaneLayout {
  width: number;
  height: number;
  cards: Map<string, Rect>;
  /** Polyline per edge id, from its source to its target. */
  routes: Map<string, Point[]>;
  /** Lane rectangles by lane. */
  lanes: Map<string, Rect>;
}

interface Item {
  /** Card index, or -1 for a waypoint of an edge. */
  node: number;
  layer: number;
  band: number;
  h: number;
  /** Order inside its layer and band. */
  pos: number;
  y: number;
  preds: Item[];
  succs: Item[];
}

/** One step of one or more edges, between an item of a column and one of the next column. */
interface Piece {
  from: Item;
  to: Item;
  /** The call goes right to left (a cycle): drawn from `to` back to `from`. */
  back: boolean;
}

const EPS = 0.5;

/**
 * Positions `nodes` (in this order of preference) and routes `edges`. `root` starts the first
 * column; `laneOrder` stacks the lanes top to bottom (lanes missing from it go last).
 */
export function swimlanes(nodes: readonly LaneNode[], edges: readonly LaneEdge[], root: string | null, laneOrder: readonly string[], opt: SwimlaneOptions): SwimlaneLayout {
  const n = nodes.length;
  const empty: SwimlaneLayout = { width: 0, height: 0, cards: new Map(), routes: new Map(), lanes: new Map() };
  if (n === 0) return empty;
  const index = new Map(nodes.map((x, i) => [x.id, i]));
  const lanes = [...laneOrder.filter((l) => nodes.some((x) => x.lane === l))];
  for (const x of nodes) if (!lanes.includes(x.lane)) lanes.push(x.lane);
  const bandOf = nodes.map((x) => lanes.indexOf(x.lane));

  // Edges between two known, distinct cards, once each.
  const es: { id: string; s: number; d: number }[] = [];
  const seen = new Set<string>();
  for (const e of edges) {
    const s = index.get(e.src);
    const d = index.get(e.dst);
    if (s === undefined || d === undefined || s === d || seen.has(e.id)) continue;
    seen.add(e.id);
    es.push({ id: e.id, s, d });
  }
  const out: number[][] = nodes.map(() => []);
  for (let i = 0; i < es.length; i++) out[es[i]!.s]!.push(i);

  // 1. Cycles: an edge back into the DFS stack is drawn reversed. The DFS order is the
  //    first order of the cards in their columns.
  const back = new Set<number>();
  const visit: number[] = new Array<number>(n).fill(-1);
  const state = new Uint8Array(n);
  let clock = 0;
  const dfs = (start: number) => {
    const stack: [number, number][] = [[start, 0]];
    state[start] = 1;
    visit[start] = clock++;
    while (stack.length) {
      const top = stack[stack.length - 1]!;
      const [u, k] = top;
      if (k >= out[u]!.length) {
        state[u] = 2;
        stack.pop();
        continue;
      }
      top[1] = k + 1;
      const ei = out[u]![k]!;
      const v = es[ei]!.d;
      if (state[v] === 1) back.add(ei);
      else if (state[v] === 0) {
        state[v] = 1;
        visit[v] = clock++;
        stack.push([v, 0]);
      }
    }
  };
  const r0 = root !== null ? index.get(root) : undefined;
  if (r0 !== undefined) dfs(r0);
  for (let i = 0; i < n; i++) if (state[i] === 0) dfs(i);

  // 2. Columns: longest path from the sources, over the edges with cycles turned around.
  const dag = es.map((e, i) => (back.has(i) ? { a: e.d, b: e.s } : { a: e.s, b: e.d }));
  const layer = new Array<number>(n).fill(0);
  {
    const indeg = new Array<number>(n).fill(0);
    const outs: number[][] = nodes.map(() => []);
    for (const e of dag) {
      indeg[e.b]!++;
      outs[e.a]!.push(e.b);
    }
    const queue = [...Array(n).keys()].filter((i) => indeg[i] === 0).sort((a, b) => visit[a]! - visit[b]!);
    for (let qi = 0; qi < queue.length; qi++) {
      const u = queue[qi]!;
      for (const v of outs[u]!) {
        layer[v] = Math.max(layer[v]!, layer[u]! + 1);
        if (--indeg[v]! === 0) queue.push(v);
      }
    }
  }
  const layers = Math.max(...layer) + 1;
  balanceColumns(dag, layer, layers, bandOf, nodes, visit, r0 ?? -1, opt);

  // 3. Items: the cards, and a waypoint in every column an edge crosses, in its target's lane.
  //    The calls of one card into one lane share their waypoints (one line, split at the end).
  const cardItem: Item[] = nodes.map((x, i) => ({ node: i, layer: layer[i]!, band: bandOf[i]!, h: x.h, pos: visit[i]!, y: 0, preds: [], succs: [] }));
  const items: Item[] = [...cardItem];
  const pieces: Piece[] = [];
  const waypoint = new Map<string, Item>();
  const pieceOf = new Map<string, Piece>();
  const itemId = new Map<Item, number>(items.map((it, i) => [it, i]));
  const pieceKey = (from: Item, to: Item, e: number) => `${itemId.get(from)}>${itemId.get(to)}${back.has(e) ? `|e${e}` : ""}`;
  const chains: Item[][] = [];
  for (let i = 0; i < dag.length; i++) {
    const { a, b } = dag[i]!;
    const isBack = back.has(i);
    const chain: Item[] = [cardItem[a]!];
    for (let l = layer[a]! + 1; l < layer[b]!; l++) {
      const key = isBack ? `e${i}|${l}` : `${a}|${bandOf[b]}|${l}`;
      let w = waypoint.get(key);
      if (!w) {
        w = { node: -1, layer: l, band: bandOf[b]!, h: 0, pos: visit[a]! + 0.5, y: 0, preds: [], succs: [] };
        waypoint.set(key, w);
        itemId.set(w, items.length);
        items.push(w);
      }
      chain.push(w);
    }
    chain.push(cardItem[b]!);
    chains.push(chain);
    for (let k = 0; k + 1 < chain.length; k++) {
      const from = chain[k]!;
      const to = chain[k + 1]!;
      const key = pieceKey(from, to, i);
      if (pieceOf.has(key)) continue;
      const p: Piece = { from, to, back: isBack };
      pieceOf.set(key, p);
      pieces.push(p);
      from.succs.push(to);
      to.preds.push(from);
    }
  }

  // 4. Order: each (column, lane) group sorted by the barycenter of its neighbors, sweeping
  //    right then left; the order with the fewest crossings wins.
  const groups: Item[][][] = Array.from({ length: layers }, () => lanes.map(() => [] as Item[]));
  for (const it of items) groups[it.layer]![it.band]!.push(it);
  const renumber = () => {
    for (const col of groups) {
      let p = 0;
      for (const g of col) for (const it of g) it.pos = p++;
    }
  };
  for (const col of groups) for (const g of col) g.sort((a, b) => a.pos - b.pos);
  renumber();
  const crossings = () => {
    let c = 0;
    const byLayer: Piece[][] = Array.from({ length: layers }, () => []);
    for (const p of pieces) byLayer[p.from.layer]!.push(p);
    for (const ps of byLayer)
      for (let i = 0; i < ps.length; i++)
        for (let j = i + 1; j < ps.length; j++) {
          const a = ps[i]!;
          const b = ps[j]!;
          if ((a.from.pos - b.from.pos) * (a.to.pos - b.to.pos) < 0) c++;
        }
    return c;
  };
  const snapshot = () => items.map((it) => it.pos);
  let best = snapshot();
  let bestC = crossings();
  const bary = (it: Item, side: Item[]) => (side.length ? side.reduce((s, x) => s + x.pos, 0) / side.length : it.pos);
  for (let iter = 0; iter < 8 && bestC > 0; iter++) {
    const down = iter % 2 === 0;
    for (let s = 1; s < layers; s++) {
      const l = down ? s : layers - 1 - s;
      for (const g of groups[l]!) {
        const key = new Map(g.map((it) => [it, bary(it, down ? it.preds : it.succs)]));
        g.sort((a, b) => key.get(a)! - key.get(b)! || a.pos - b.pos);
      }
      renumber();
    }
    const c = crossings();
    if (c < bestC) {
      bestC = c;
      best = snapshot();
    }
  }
  items.forEach((it, i) => (it.pos = best[i]!));
  for (const col of groups) for (const g of col) g.sort((a, b) => a.pos - b.pos);

  // 5. Heights, lane by lane: each column of a lane stacked in order, every item pulled
  //    towards the items it is linked to in the same lane (isotonic regression keeps the order
  //    and the gaps).
  const gapAfter = (a: Item, b: Item) => (a.node < 0 && b.node < 0 ? opt.rowGap / 2 : opt.rowGap);
  const bandTop: number[] = [];
  const bandBottom: number[] = [];
  let top = 0;
  for (let b = 0; b < lanes.length; b++) {
    const y0 = top + opt.lanePad.top;
    const cols = groups.map((col) => col[b]!).filter((g) => g.length > 0);
    // The tallest column sets the lane's height; the others move inside it.
    let limit = y0;
    for (const g of cols) {
      let y = y0;
      for (let i = 0; i < g.length; i++) {
        if (i > 0) y += gapAfter(g[i - 1]!, g[i]!);
        g[i]!.y = y;
        y += g[i]!.h;
      }
      limit = Math.max(limit, y);
    }
    const center = (it: Item) => it.y + it.h / 2;
    const median = (cs: number[]) => {
      cs.sort((p, q) => p - q);
      const m = cs.length >> 1;
      return cs.length % 2 ? cs[m]! : (cs[m - 1]! + cs[m]!) / 2;
    };
    const align = (rounds: number) => {
      for (let round = 0; round < rounds; round++) {
        const rightward = round % 2 === 0;
        const order = rightward ? cols : [...cols].reverse();
        for (const g of order) {
          const want = g.map((it) => {
            const near = (rightward ? it.preds : it.succs).filter((x) => x.band === b);
            return near.length ? median(near.map(center)) : center(it);
          });
          place(g, want.map((c, i) => c - g[i]!.h / 2), y0, limit, gapAfter);
        }
      }
    };
    align(6);
    // A passing edge goes where its line runs, between the cards around it, not where its
    // rank put it: re-sorted by the middle of what it links, then aligned again.
    for (let pass = 0; pass < 2; pass++) {
      for (const g of cols) {
        const key = new Map(
          g.map((it) => {
            if (it.node >= 0) return [it, center(it)] as const;
            const near = [...it.preds, ...it.succs].filter((x) => x.band === b);
            return [it, near.length ? near.reduce((s, x) => s + center(x), 0) / near.length : center(it)] as const;
          }),
        );
        g.sort((p, q) => key.get(p)! - key.get(q)! || p.pos - q.pos);
        place(g, g.map((it) => key.get(it)! - it.h / 2), y0, limit, gapAfter);
      }
      align(4);
    }
    for (const g of cols) g.forEach((it, i) => (it.pos = i));
    let bottom = y0;
    for (const g of cols) for (const it of g) bottom = Math.max(bottom, it.y + it.h);
    bandTop.push(top);
    bandBottom.push(bottom + opt.lanePad.bottom);
    top = bottom + opt.lanePad.bottom + opt.laneGap;
  }

  // 6. Channels: one track per card's calls (and per waypoint), ordered to cross least.
  const colW = new Array<number>(layers).fill(0);
  for (let i = 0; i < n; i++) colW[layer[i]!] = Math.max(colW[layer[i]!]!, nodes[i]!.w);
  const mid = (it: Item) => it.y + it.h / 2;
  // Where each piece leaves and arrives: a card's calls leave from the middle of its right
  // side; the arrows into one card are spread over its left side, in the order of where they
  // come from. A call drawn backwards arrives on the right side, under the trunk.
  const portY = new Map<Piece, { s: number; t: number }>();
  for (const p of pieces) portY.set(p, { s: mid(p.from), t: mid(p.to) });
  const inPorts = new Map<Item, Piece[]>();
  for (const p of pieces) {
    if (p.to.node < 0) continue;
    const list = inPorts.get(p.to) ?? [];
    list.push(p);
    inPorts.set(p.to, list);
  }
  for (const [target, ps] of inPorts) {
    if (ps.length < 2) continue;
    ps.sort((a, b) => portY.get(a)!.s - portY.get(b)!.s || a.from.pos - b.from.pos);
    const gap = Math.min(opt.portGap, (target.h - 16) / (ps.length - 1));
    ps.forEach((p, i) => (portY.get(p)!.t = mid(target) + (i - (ps.length - 1) / 2) * gap));
  }
  for (const p of pieces) if (p.back && p.from.node >= 0) portY.get(p)!.s = mid(p.from) + Math.min(opt.portGap, p.from.h / 4);

  interface Net {
    src: number;
    targets: number[];
    lo: number;
    hi: number;
    pieces: Piece[];
    track: number;
    key: number;
  }
  const channelTracks: number[] = new Array<number>(Math.max(0, layers - 1)).fill(0);
  const netOf = new Map<Piece, Net>();
  for (let l = 0; l + 1 < layers; l++) {
    const nets = new Map<Item | Piece, Net>();
    for (const p of pieces) {
      if (p.from.layer !== l) continue;
      const py = portY.get(p)!;
      const left = py.s;
      const right = py.t;
      const k = p.back ? p : p.from;
      let net = nets.get(k);
      if (!net) {
        net = { src: left, targets: [], lo: left, hi: left, pieces: [], track: 0, key: p.from.pos };
        nets.set(k, net);
      }
      net.targets.push(right);
      net.lo = Math.min(net.lo, right);
      net.hi = Math.max(net.hi, right);
      net.pieces.push(p);
      netOf.set(p, net);
    }
    const bent = [...nets.values()].filter((x) => x.hi - x.lo > EPS);
    channelTracks[l] = assignTracks(bent);
  }

  // 7. Columns left to right, each channel as wide as its tracks.
  const colX: number[] = [];
  let x = opt.lanePad.side;
  for (let l = 0; l < layers; l++) {
    colX.push(x);
    if (l + 1 < layers) {
      const t = channelTracks[l]!;
      x += colW[l]! + Math.max(opt.channelMin, opt.channelPadLeft + opt.channelPadRight + Math.max(0, t - 1) * opt.track);
    }
  }
  const right = colX[layers - 1]! + colW[layers - 1]!;

  const cards = new Map<string, Rect>();
  for (let i = 0; i < n; i++) cards.set(nodes[i]!.id, { x: colX[layer[i]!]!, y: cardItem[i]!.y, w: nodes[i]!.w, h: nodes[i]!.h });

  // 8. Routes: each piece through its channel, joined into one polyline per edge.
  const pieceRoute = (p: Piece): Point[] => {
    const l = p.from.layer;
    const py = portY.get(p)!;
    const x0 = p.from.node >= 0 ? colX[l]! + nodes[p.from.node]!.w : colX[l]! + colW[l]!;
    const x1 = colX[l + 1]!;
    const net = netOf.get(p)!;
    if (net.hi - net.lo <= EPS) return [{ x: x0, y: py.s }, { x: x1, y: py.t }];
    const tx = colX[l]! + colW[l]! + opt.channelPadLeft + net.track * opt.track;
    return [
      { x: x0, y: py.s },
      { x: tx, y: py.s },
      { x: tx, y: py.t },
      { x: x1, y: py.t },
    ];
  };
  const routes = new Map<string, Point[]>();
  for (let i = 0; i < es.length; i++) {
    const chain = chains[i]!;
    const pts: Point[] = [];
    for (let k = 0; k + 1 < chain.length; k++) for (const q of pieceRoute(pieceOf.get(pieceKey(chain[k]!, chain[k + 1]!, i))!)) push(pts, q);
    if (back.has(i)) pts.reverse();
    if (pts.length >= 2) routes.set(es[i]!.id, simplify(pts));
  }

  // 9. Lanes: their cards' box, with room for the title.
  const laneRects = new Map<string, Rect>();
  let width = right + opt.lanePad.side;
  for (let b = 0; b < lanes.length; b++) {
    const rs = [...cards.entries()].filter(([id]) => nodes[index.get(id)!]!.lane === lanes[b]).map(([, r]) => r);
    if (rs.length === 0) continue;
    const x0 = Math.min(...rs.map((r) => r.x)) - opt.lanePad.side;
    let x1 = Math.max(...rs.map((r) => r.x + r.w)) + opt.lanePad.side;
    x1 = Math.max(x1, x0 + (opt.laneMinW?.get(lanes[b]!) ?? 0));
    width = Math.max(width, x1);
    laneRects.set(lanes[b]!, { x: x0, y: bandTop[b]!, w: x1 - x0, h: bandBottom[b]! - bandTop[b]! });
  }
  const height = bandBottom.length ? Math.max(...bandBottom) : 0;
  return { width, height, cards, routes, lanes: laneRects };
}

/**
 * Shorter lanes, while the drawing is taller than `opt.aspect` wants: a card that calls nothing
 * (a sink) may sit in any column right of its callers. One from the tallest column of the
 * tallest lane moves to the nearest column further right that takes it with room to spare
 * (the call passes the columns between on one shared waypoint), again and again. Columns are
 * measured as stacked: cards, gaps and the waypoints passing through.
 */
function balanceColumns(dag: { a: number; b: number }[], layer: number[], layers: number, bandOf: number[], nodes: readonly LaneNode[], visit: number[], root: number, opt: SwimlaneOptions) {
  if (layers < 2 || !opt.aspect) return;
  const n = nodes.length;
  const outdeg = new Array<number>(n).fill(0);
  for (const e of dag) outdeg[e.a]!++;
  const bands = Math.max(...bandOf) + 1;
  const heights = (band: number): number[] => {
    const h = new Array<number>(layers).fill(0);
    const count = new Array<number>(layers).fill(0);
    for (let i = 0; i < n; i++)
      if (bandOf[i] === band) {
        h[layer[i]!]! += nodes[i]!.h;
        count[layer[i]!]!++;
      }
    const ways = new Set<string>();
    for (const e of dag) if (bandOf[e.b] === band) for (let l = layer[e.a]! + 1; l < layer[e.b]!; l++) ways.add(`${e.a}|${l}`);
    for (const w of ways) count[Number(w.slice(w.indexOf("|") + 1))]!++;
    return h.map((x, l) => x + Math.max(0, count[l]! - 1) * opt.rowGap);
  };
  const colW = new Array<number>(layers).fill(0);
  for (let i = 0; i < n; i++) colW[layer[i]!] = Math.max(colW[layer[i]!]!, nodes[i]!.w);
  const width = colW.reduce((s, w) => s + w, 0) + (layers - 1) * opt.channelMin;
  const hs = Array.from({ length: bands }, (_, b) => heights(b));
  const tallest = (b: number) => Math.max(...hs[b]!);
  const height = () => hs.reduce((s, _, b) => s + tallest(b) + opt.lanePad.top + opt.lanePad.bottom + opt.laneGap, 0);
  // A band's columns from the tallest down: a move must make this smaller, compared in order
  // (two columns as tall as each other come down one after the other).
  const profile = (h: number[]) => [...h].sort((a, b) => b - a);
  const smaller = (a: number[], b: number[]) => {
    for (let i = 0; i < a.length; i++) if (Math.abs(a[i]! - b[i]!) > 0.5) return a[i]! < b[i]!;
    return false;
  };
  /** One sink out of one of the band's tallest columns, if that makes the band's profile smaller. */
  const shorten = (band: number): boolean => {
    const top = tallest(band);
    const was = profile(hs[band]!);
    for (let tall = 0; tall < layers; tall++) {
      if (hs[band]![tall]! < top - 0.5) continue;
      const sinks = [...Array(n).keys()].filter((i) => bandOf[i] === band && layer[i] === tall && outdeg[i] === 0 && i !== root).sort((a, b) => visit[b]! - visit[a]!);
      for (const s of sinks)
        for (let l = tall + 1; l < layers; l++) {
          layer[s] = l;
          const next = heights(band);
          if (smaller(profile(next), was)) {
            hs[band] = next;
            return true;
          }
          layer[s] = tall;
        }
    }
    return false;
  };
  for (let guard = 0; guard < 4 * n && height() * opt.aspect > width; guard++) {
    const order = [...Array(bands).keys()].sort((a, b) => tallest(b) - tallest(a) || a - b);
    if (!order.some((b) => shorten(b))) break;
  }
}

/**
 * Tops for the items of `g` (in order) as close as possible to `want` (least squares), with
 * each item at least its gap under the one before, none above `floor` and none past `limit`
 * (unless the column needs more room than that).
 */
function place(g: Item[], want: number[], floor: number, limit: number, gap: (a: Item, b: Item) => number) {
  // With offsets taken out, the order constraint is plain monotonicity: pool adjacent violators.
  const off: number[] = [];
  let o = 0;
  for (let i = 0; i < g.length; i++) {
    if (i > 0) o += g[i - 1]!.h + gap(g[i - 1]!, g[i]!);
    off.push(o);
  }
  const blocks: { sum: number; n: number }[] = [];
  for (let i = 0; i < g.length; i++) {
    blocks.push({ sum: want[i]! - off[i]!, n: 1 });
    while (blocks.length > 1) {
      const b = blocks[blocks.length - 1]!;
      const a = blocks[blocks.length - 2]!;
      if (a.sum / a.n <= b.sum / b.n) break;
      blocks.pop();
      a.sum += b.sum;
      a.n += b.n;
    }
  }
  // A column re-sorted after the lane's height was set may need more than `limit`: it starts
  // at `floor` and the lane grows to fit it.
  const ceil = Math.max(floor, limit - off[g.length - 1]! - g[g.length - 1]!.h);
  let i = 0;
  for (const b of blocks) {
    const z = Math.min(ceil, Math.max(floor, b.sum / b.n));
    for (let k = 0; k < b.n; k++, i++) g[i]!.y = Math.round(z + off[i]!);
  }
}

/**
 * Tracks for the nets of one channel: two nets whose spans overlap get different tracks, the
 * one to the left being the one that crosses the other less. Returns how many tracks it took.
 */
function assignTracks(nets: { src: number; targets: number[]; lo: number; hi: number; track: number; key: number }[]): number {
  const m = nets.length;
  if (m === 0) return 0;
  const inside = (y: number, a: { lo: number; hi: number }) => y > a.lo + EPS && y < a.hi - EPS;
  // Crossings when `a` is left of `b`: b's start crosses a's trunk, a's ends cross b's trunk.
  // An end of `a` level with b's start would run along it and read as one straight call
  // between two cards that don't call each other: worse than any number of crossings.
  const level = (y: number, z: number) => Math.abs(y - z) <= 1.5;
  const cost = (a: (typeof nets)[number], b: (typeof nets)[number]) =>
    (inside(b.src, a) ? 1 : 0) + a.targets.filter((y) => inside(y, b)).length + 1000 * a.targets.filter((y) => level(y, b.src)).length;
  // Every two nets that overlap get an order ("left of"), the one that matters most first; one
  // that would close a cycle gives way to the order already implied. Then every overlapping
  // pair is ordered and the orders have no cycle: a longest path gives distinct tracks.
  const pairs: { left: number; right: number; weight: number }[] = [];
  for (let i = 0; i < m; i++)
    for (let j = i + 1; j < m; j++) {
      const a = nets[i]!;
      const b = nets[j]!;
      if (Math.max(a.lo, b.lo) > Math.min(a.hi, b.hi) + 2) continue;
      const ab = cost(a, b);
      const ba = cost(b, a);
      const aFirst = ab < ba || (ab === ba && a.key <= b.key);
      pairs.push({ left: aFirst ? i : j, right: aFirst ? j : i, weight: Math.abs(ab - ba) });
    }
  pairs.sort((p, q) => q.weight - p.weight || p.left - q.left || p.right - q.right);
  const after: number[][] = nets.map(() => []);
  const reaches = (from: number, to: number) => {
    const seen = new Uint8Array(m);
    const stack = [from];
    seen[from] = 1;
    while (stack.length) {
      const u = stack.pop()!;
      if (u === to) return true;
      for (const v of after[u]!)
        if (!seen[v]) {
          seen[v] = 1;
          stack.push(v);
        }
    }
    return false;
  };
  for (const p of pairs) {
    if (reaches(p.right, p.left)) continue;
    after[p.left]!.push(p.right);
  }
  // Longest path from the left.
  const indeg = new Array<number>(m).fill(0);
  for (const list of after) for (const v of list) indeg[v]!++;
  const queue = [...Array(m).keys()].filter((i) => indeg[i] === 0);
  for (const net of nets) net.track = 0;
  for (let qi = 0; qi < queue.length; qi++) {
    const u = queue[qi]!;
    for (const v of after[u]!) {
      nets[v]!.track = Math.max(nets[v]!.track, nets[u]!.track + 1);
      if (--indeg[v]! === 0) queue.push(v);
    }
  }
  return Math.max(...nets.map((x) => x.track)) + 1;
}

function push(pts: Point[], p: Point) {
  const last = pts[pts.length - 1];
  if (!last || Math.abs(last.x - p.x) > 0.01 || Math.abs(last.y - p.y) > 0.01) pts.push({ x: p.x, y: p.y });
}

/** Drops the middle point of three in a line. */
function simplify(pts: Point[]): Point[] {
  const out: Point[] = [];
  for (const p of pts) {
    while (out.length >= 2) {
      const a = out[out.length - 2]!;
      const b = out[out.length - 1]!;
      const straight = (Math.abs(a.x - b.x) < 0.01 && Math.abs(b.x - p.x) < 0.01) || (Math.abs(a.y - b.y) < 0.01 && Math.abs(b.y - p.y) < 0.01);
      if (!straight) break;
      out.pop();
    }
    out.push(p);
  }
  return out;
}
