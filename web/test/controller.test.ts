import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { saasFixture } from "../mock/fixtures/saas";
import { MockIndex } from "../mock/model";
import type { GraphQuery, Meta, TreeNode } from "../src/api/types";
import { atLevel, frontierSize, initialView } from "../src/model/view";
import { buildHierarchy } from "../src/model/hierarchy";
import { cappedTodo, Controller, todoLevel, type CameraRequest, type ControllerApi } from "../src/state/controller";
import { initialState, store } from "../src/state/store";

type Method = "meta" | "tree" | "graph" | "node" | "issues" | "findings";

/** Fake API over the mock index. Responses carry the generation at call time; methods in `hold` wait for `release()`. */
class FakeApi {
  readonly index = new MockIndex(saasFixture());
  generation = 1;
  reindexing = false;
  hold = new Set<Method>();
  private queue: (() => void)[] = [];
  readonly calls: { method: Method; arg?: unknown }[] = [];
  readonly base = "/api/";

  private respond<T>(method: Method, arg: unknown, make: () => T): Promise<T> {
    this.calls.push({ method, arg });
    const value = make();
    if (!this.hold.has(method)) return Promise.resolve(value);
    return new Promise((resolve) => this.queue.push(() => resolve(value)));
  }

  /** Resolves held calls in the order they were made. */
  async release() {
    while (this.queue.length) {
      this.queue.shift()!();
      await flush();
    }
  }

  url = (path: string) => `/api/${path}`;
  meta = () =>
    this.respond("meta", undefined, (): Meta => ({
      generation: this.generation,
      status: "ready",
      progress: this.reindexing ? { done: 1, total: 2 } : null,
      reindexing: this.reindexing,
      repo: { name: "tallerpro", root: "/x" },
      indexed_at: "2026-11-20T00:00:00Z",
      index_ms: 1,
      counts: this.index.counts(),
      languages: [],
      watching: true,
    }));
  /** A dir the next trees leave out, as if a re-index found it deleted. */
  dropDir: string | null = null;
  tree = () =>
    this.respond("tree", undefined, () => {
      const drop = (n: TreeNode): TreeNode => ({ ...n, children: n.children?.filter((c) => c.id !== this.dropDir).map(drop) });
      return { generation: this.generation, root: drop(this.index.tree()) };
    });
  /** Makes /api/graph fail. */
  failGraph = false;
  /** Called on every /api/graph request, after it is recorded (to act as the user mid-request). */
  onGraph: ((q: GraphQuery) => void) | null = null;
  graph = (q: GraphQuery) => {
    let p;
    if (this.failGraph) {
      this.calls.push({ method: "graph", arg: q });
      p = Promise.reject(new Error("graph down"));
    } else {
      p = this.respond("graph", q, () => ({ generation: this.generation, ...this.index.graph(q.expanded, { depth: q.depth, collapsed: q.collapsed }) }));
    }
    this.onGraph?.(q);
    return p;
  };
  node = (id: string) => this.respond("node", id, () => ({ generation: this.generation, ...this.index.node(id) }) as never);
  issues = () => this.respond("issues", undefined, () => ({ generation: this.generation, ...this.index.issues() }));
  findings = () =>
    this.respond("findings", undefined, () => ({
      generation: this.generation,
      findings: [],
      summary: { total: 0, violations: 0, by_severity: {}, by_kind: {} },
      config: { origin: null, error: null },
      total: 0,
      truncated: false,
    }));
  search = async () => ({ generation: this.generation, results: [] });
  reindex = async () => {};
}

async function flush() {
  for (let i = 0; i < 10; i++) await Promise.resolve();
}

function setup() {
  const api = new FakeApi();
  const c = new Controller(api as unknown as ControllerApi);
  return { api, c };
}

beforeEach(() => store.set(initialState));
afterEach(() => vi.useRealTimers());

const graphCalls = (api: FakeApi) => api.calls.filter((call) => call.method === "graph");

const lastGraph = (api: FakeApi) => graphCalls(api).at(-1)?.arg as GraphQuery | undefined;
const START = { level: 1, open: ["dir:src"], closed: [] };

describe("controller", () => {
  it("runs one more load when a new generation lands while a load is in flight", async () => {
    const { api, c } = setup();
    api.hold.add("tree");
    const first = c.loadAll({ keepView: false });
    api.generation = 2; // index-finished arrives while the first load waits for its tree
    const second = c.loadAll({ keepView: true });
    api.hold.clear();
    await api.release();
    await Promise.all([first, second]);
    await flush();
    // The tree of the first load predates generation 2, so it must be fetched again.
    expect(api.calls.filter((call) => call.method === "tree")).toHaveLength(2);
    expect(store.get().graph?.generation).toBe(2);
  });

  it("starts at level 1 with the main folder open", async () => {
    const { api, c } = setup();
    await c.loadAll({ keepView: false });
    expect(store.get().view).toEqual(START);
    expect(store.get().view).toEqual(initialView(buildHierarchy(api.index.tree())));
    expect(lastGraph(api)).toEqual({ depth: 1, expanded: ["dir:src"], collapsed: [] });
  });

  it("keeps a folder opened and a selection made while the plan refreshes", async () => {
    const { api, c } = setup();
    await c.loadAll({ keepView: false });
    api.hold.add("graph");
    api.generation = 2;
    const load = c.loadAll({ keepView: true });
    await flush();
    // The refresh is waiting for the graph of src; the user opens src/app meanwhile.
    void c.openFolder("dir:src/app");
    c.select("dir:src/app/(app)", { reveal: false });
    api.hold.clear();
    await api.release();
    await load;
    await flush();
    const s = store.get();
    expect(s.view.open).toEqual(["dir:src", "dir:src/app"]);
    expect(s.expanded).toEqual(["dir:src", "dir:src/app"]);
    expect(s.selected).toBe("dir:src/app/(app)");
    expect(s.graph?.nodes.some((n) => n.parent === "dir:src/app")).toBe(true);
    expect(s.graph?.nodes.some((n) => n.parent === "dir:src/lib")).toBe(false);
    expect(s.graph?.generation).toBe(2);
  });

  it("opening a folder keeps its siblings open, and closing one keeps the rest", async () => {
    const { api, c } = setup();
    await c.loadAll({ keepView: false });
    await c.openFolder("dir:src/lib");
    await c.openFolder("dir:src/app");
    expect(store.get().view.open).toEqual(["dir:src", "dir:src/app", "dir:src/lib"]);
    expect(store.get().graph?.nodes.some((n) => n.parent === "dir:src/lib")).toBe(true);
    expect(store.get().graph?.nodes.some((n) => n.parent === "dir:src/app")).toBe(true);
    await c.toggleFolder("dir:src/lib");
    expect(store.get().view.open).toEqual(["dir:src", "dir:src/app"]);
    expect(lastGraph(api)?.expanded).toEqual(["dir:src", "dir:src/app"]);
  });

  it("sets a level with depth alone, and Todo opens every folder", async () => {
    const { api, c } = setup();
    await c.loadAll({ keepView: false });
    c.setLevel("all");
    await flush();
    expect(store.get().view).toEqual(atLevel("all"));
    expect(lastGraph(api)).toEqual({ depth: "all", expanded: [], collapsed: [] });
    expect(store.get().graph?.nodes.every((n) => n.kind === "file")).toBe(true);
    // Closing a folder inside the level sends it as `collapsed`.
    await c.closeFolder("dir:src");
    expect(lastGraph(api)).toEqual({ depth: "all", expanded: [], collapsed: ["dir:src"] });
    c.setLevel(1);
    await flush();
    expect(lastGraph(api)).toEqual({ depth: 1, expanded: [], collapsed: [] });
  });

  it("Todo on a tree too big to open whole keeps every open folder and says where it stopped", async () => {
    const { api, c } = setup();
    await c.loadAll({ keepView: false });
    const h = store.get().hierarchy!;
    // A budget that level 2 already exceeds: Todo stops at level 1.
    c.todoMax = frontierSize(h, atLevel(2)) - 1;
    expect(todoLevel(h, c.todoMax)).toBe(1);
    c.setLevel("all");
    await flush();
    // src, opened by the first screen, stays open.
    expect(store.get().view).toEqual(START);
    expect(store.get().todoCap).toBe(1);
    expect(cappedTodo(store.get().view.level, store.get().todoCap)).toBe(1);
    // From level 3, Todo doesn't go back down either.
    c.setLevel(3);
    await flush();
    expect(store.get().todoCap).toBeNull();
    expect(cappedTodo(store.get().view.level, store.get().todoCap)).toBeNull();
    const at3 = store.get().expanded;
    c.setLevel("all");
    await flush();
    expect(store.get().view.level).toBe(3);
    expect(store.get().expanded).toEqual(at3);
    expect(store.get().todoCap).toBe(3);
    expect(lastGraph(api)).toEqual({ depth: 3, expanded: [], collapsed: [] });
    // A folder opened by hand on top of a level survives Todo.
    c.setLevel(1);
    await flush();
    await c.openFolder("dir:src/lib/db");
    const before = store.get().expanded;
    c.setLevel("all");
    await flush();
    for (const id of before) expect(store.get().expanded).toContain(id);
    expect(cappedTodo(store.get().view.level, store.get().todoCap)).toBe(1);
  });

  it("keeps the selection of an \"otras N\" package card across a re-index", async () => {
    const { api, c } = setup();
    await c.loadAll({ keepView: false });
    c.select("extmore:npm");
    api.generation = 2;
    await c.loadAll({ keepView: true });
    await flush();
    expect(store.get().selected).toBe("extmore:npm");
    expect(store.get().detail).toEqual({ id: "extmore:npm", data: null, error: null, loading: false });
    expect(api.calls.some((call) => call.method === "node" && call.arg === "extmore:npm")).toBe(false);
  });

  it("drops the folders a re-index deleted, keeping the rest of the view", async () => {
    const { api, c } = setup();
    await c.loadAll({ keepView: false });
    await c.openFolder("dir:src/lib/db/queries");
    expect(store.get().expanded).toEqual(["dir:src", "dir:src/lib", "dir:src/lib/db", "dir:src/lib/db/queries"]);
    api.dropDir = "dir:src/lib/db";
    api.generation = 2;
    await c.loadAll({ keepView: true });
    expect(store.get().view.open).toEqual(["dir:src", "dir:src/lib"]);
    expect(store.get().expanded).toEqual(["dir:src", "dir:src/lib"]);
    expect(lastGraph(api)?.expanded).toEqual(["dir:src", "dir:src/lib"]);
  });

  it("selecting a node that isn't on the plan opens the folders above it only", async () => {
    const { api, c } = setup();
    await c.loadAll({ keepView: false });
    c.select("file:src/lib/db/schema.ts");
    await flush();
    expect(store.get().view.open).toEqual(["dir:src", "dir:src/lib", "dir:src/lib/db"]);
    expect(lastGraph(api)?.expanded).toEqual(["dir:src", "dir:src/lib", "dir:src/lib/db"]);
    const before = graphCalls(api).length;
    c.select("dir:src/app"); // already a card
    await flush();
    expect(graphCalls(api)).toHaveLength(before);
  });

  it("never shows the detail of a node that is no longer selected", async () => {
    const { api, c } = setup();
    await c.loadAll({ keepView: false });
    c.select("dir:src/lib/db"); // fetched and cached
    await flush();
    api.hold.add("node");
    c.select("dir:src/lib/auth"); // pending
    c.select("dir:src/lib/db"); // served from the cache
    api.hold.clear();
    await api.release();
    await flush();
    expect(store.get().selected).toBe("dir:src/lib/db");
    expect(store.get().detail?.id).toBe("dir:src/lib/db");
  });

  it("keeps the newest issues and findings when an older load answers last, and clears findings that fail", async () => {
    const { api, c } = setup();
    await c.loadAll({ keepView: false });
    api.hold.add("issues");
    api.hold.add("findings");
    const first = c.loadIssues(); // generation 1, answers last
    api.hold.clear();
    api.generation = 2;
    await c.loadIssues();
    expect(store.get().findings?.generation).toBe(2);
    await api.release();
    await first;
    expect(store.get().findings?.generation).toBe(2);
    expect(store.get().issues?.generation).toBe(2);
    // A failed load leaves no stale findings marking the plan.
    api.findings = () => Promise.reject(new Error("findings down"));
    await c.loadIssues();
    expect(store.get().findings).toBeNull();
  });

  it("clears a stuck re-index indicator when the stream reconnects", async () => {
    const { api, c } = setup();
    await c.loadAll({ keepView: false });
    store.set({ reindexing: true, progress: { done: 3, total: 9 } });
    api.reindexing = false;
    c.handleEvent({ type: "hello", data: { generation: api.generation, status: "ready" } });
    await flush();
    expect(store.get().reindexing).toBe(false);
    expect(store.get().progress).toBeNull();
  });

  it("selecting an open folder keeps it open", async () => {
    const { api, c } = setup();
    await c.loadAll({ keepView: false });
    await c.openFolder("dir:src/lib/db");
    const before = graphCalls(api).length;
    c.select("dir:src", { fly: true }); // explorer, palette or a box name
    await flush();
    c.select("dir:src/lib");
    await flush();
    expect(store.get().expanded).toEqual(["dir:src", "dir:src/lib", "dir:src/lib/db"]);
    expect(graphCalls(api)).toHaveLength(before);
  });

  it("double click closes an open box and opens a closed folder", async () => {
    const { c } = setup();
    await c.loadAll({ keepView: false });
    c.activate("dir:src/lib");
    await flush();
    expect(store.get().view.open).toEqual(["dir:src", "dir:src/lib"]);
    c.activate("dir:src");
    await flush();
    expect(store.get().view.open).toEqual([]);
    expect(store.get().selected).toBe("dir:src");
  });

  it("goes back to the view on screen when opening a folder fails, so a retry fetches again", async () => {
    const { api, c } = setup();
    await c.loadAll({ keepView: false });
    api.failGraph = true;
    await c.openFolder("dir:src/app");
    let s = store.get();
    expect(s.view).toEqual(START);
    expect(s.expanded).toEqual(["dir:src"]);
    expect(s.graphView).toEqual(START);
    expect(s.graphLoading).toBe(false);
    api.failGraph = false;
    await c.openFolder("dir:src/app");
    s = store.get();
    expect(lastGraph(api)?.expanded).toEqual(["dir:src", "dir:src/app"]);
    expect(s.view.open).toEqual(["dir:src", "dir:src/app"]);
    expect(s.graphView?.open).toEqual(["dir:src", "dir:src/app"]);
  });

  it("a refresh never overwrites a view the user keeps changing while it loads", async () => {
    const { api, c } = setup();
    await c.loadAll({ keepView: false });
    api.generation = 2;
    // On each graph request of the refresh the user picks another level, one more time than
    // it refetches: the last one (level 3) lands while the refresh fetches level 2.
    const levels = [2, 3, 2, 3];
    let inside = false;
    api.onGraph = () => {
      if (inside || levels.length === 0) return;
      inside = true;
      c.setLevel(levels.shift()!);
      inside = false;
    };
    await c.loadAll({ keepView: true });
    await flush();
    api.onGraph = null;
    await flush();
    const s = store.get();
    expect(s.view).toEqual(atLevel(3));
    expect(s.graphView).toEqual(atLevel(3));
    expect(s.graphLoading).toBe(false);
    expect(s.graph?.nodes.some((n) => n.parent === "dir:src/lib")).toBe(true);
    expect(s.graph?.generation).toBe(2);
  });

  it("flies to a node outside the plan once the plan that shows it is drawn", async () => {
    const { c } = setup();
    const requests: CameraRequest[] = [];
    c.onCamera((r) => requests.push(r));
    await c.loadAll({ keepView: false });
    c.select("file:src/lib/db/schema.ts", { fly: true });
    // The old plan has no such card: nothing to fly to yet.
    expect(requests).toEqual([]);
    await flush();
    const shown = store.get().graphView;
    expect(shown?.open).toEqual(["dir:src", "dir:src/lib", "dir:src/lib/db"]);
    expect(c.takePendingFly(START)).toBeNull(); // a layout of another view doesn't take it
    expect(c.takePendingFly(shown)).toBe("file:src/lib/db/schema.ts");
    expect(c.takePendingFly(shown)).toBeNull();
    // A node already on the plan flies right away.
    c.select("dir:src/app", { fly: true });
    expect(requests).toEqual([{ type: "fly", id: "dir:src/app" }]);
  });

  it("names the folder opened or closed as the node the plan keeps in place, for that view only", async () => {
    const { c } = setup();
    await c.loadAll({ keepView: false });
    void c.openFolder("dir:src/lib");
    await flush();
    const opened = store.get().graphView;
    expect(opened?.open).toContain("dir:src/lib");
    expect(c.takePendingAnchor(START)).toBeNull(); // a layout of another view doesn't take it
    expect(c.takePendingAnchor(opened)).toBe("dir:src/lib");
    expect(c.takePendingAnchor(opened)).toBeNull();
    void c.closeFolder("dir:src/lib");
    await flush();
    expect(c.takePendingAnchor(store.get().graphView)).toBe("dir:src/lib");
    // A level change names none: the plan keeps the selection or the card at the center.
    c.setLevel(2);
    await flush();
    expect(c.takePendingAnchor(store.get().graphView)).toBeNull();
  });

  it("drops the folder to keep in place when it can't be opened", async () => {
    const { api, c } = setup();
    await c.loadAll({ keepView: false });
    api.failGraph = true;
    void c.openFolder("dir:src/lib");
    await flush();
    expect(c.takePendingAnchor({ level: 1, open: ["dir:src", "dir:src/lib"], closed: [] })).toBeNull();
  });

  it("doesn't move the camera for a package with libraries off", async () => {
    const { c } = setup();
    const requests: CameraRequest[] = [];
    c.onCamera((r) => requests.push(r));
    await c.loadAll({ keepView: false });
    c.select("ext:npm:drizzle-orm", { fly: true });
    await flush();
    expect(requests).toEqual([]);
    expect(c.takePendingFly(store.get().view)).toBeNull();
    expect(store.get().view).toEqual(START);
    store.set({ showExternals: true });
    c.select("ext:npm:next", { fly: true });
    expect(requests).toEqual([{ type: "fly", id: "ext:npm:next" }]);
  });

  it("drops the pending fly when the folder can't be opened or another node is selected", async () => {
    const { api, c } = setup();
    await c.loadAll({ keepView: false });
    const target = { level: 1, open: ["dir:src", "dir:src/lib", "dir:src/lib/db"], closed: [] };
    api.failGraph = true;
    c.select("file:src/lib/db/schema.ts", { fly: true });
    await flush();
    expect(c.takePendingFly(target)).toBeNull();
    api.failGraph = false;
    c.select("file:src/lib/db/schema.ts", { fly: true });
    c.select("dir:src/app");
    await flush();
    expect(c.takePendingFly(target)).toBeNull();
  });

  it("clears the reindex highlight after 4 s, so cards drawn later don't pulse again", async () => {
    vi.useFakeTimers();
    const { api, c } = setup();
    await c.loadAll({ keepView: false });
    api.generation = 2;
    const done = { generation: 2, ms: 1, changed: ["src/lib/db/schema.ts"], changed_total: 1, counts: api.index.counts() } as never;
    c.handleEvent({ type: "index-finished", data: done });
    await flush();
    await flush();
    expect(store.get().pulse?.ids).toEqual(["dir:src/lib"]);
    vi.advanceTimersByTime(3999);
    expect(store.get().pulse).not.toBeNull();
    vi.advanceTimersByTime(1);
    expect(store.get().pulse).toBeNull();
  });
});
