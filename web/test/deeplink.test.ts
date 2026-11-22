import { beforeEach, describe, expect, it } from "vitest";
import { saasFixture } from "../mock/fixtures/saas";
import { MockIndex } from "../mock/model";
import type { GraphQuery } from "../src/api/types";
import { buildHierarchy } from "../src/model/hierarchy";
import { currentLink, encodeFocus, linkSearch, linkView, parseDeepLink, planTarget } from "../src/model/deeplink";
import { Controller, type ControllerApi } from "../src/state/controller";
import { DeepLinks, type LinkDeps, type LinkWindow } from "../src/state/deeplink";
import { initialState, store } from "../src/state/store";

const index = new MockIndex(saasFixture());
const h = buildHierarchy(index.tree());

async function flush() {
  for (let i = 0; i < 10; i++) await Promise.resolve();
}

describe("deep link parsing", () => {
  it("reads the view and the focus, decoded", () => {
    expect(parseDeepLink("?view=plan&focus=dir:src/app")).toEqual({ view: "plan", focus: "dir:src/app" });
    expect(parseDeepLink("?view=flows&focus=sym:src/a.rs%23Auth.login")).toEqual({ view: "flows", focus: "sym:src/a.rs#Auth.login" });
    expect(parseDeepLink("?focus=file%3Asrc%2Fa%2Bb.ts&debug")).toEqual({ view: null, focus: "file:src/a+b.ts" });
    expect(parseDeepLink("?view=graph&focus=")).toEqual({ view: null, focus: null });
    expect(parseDeepLink("")).toEqual({ view: null, focus: null });
  });

  it("opens the flows for a function and the plan for anything else", () => {
    expect(linkView({ view: null, focus: "sym:src/a.rs#run" })).toBe("flows");
    expect(linkView({ view: null, focus: "file:src/a.rs" })).toBe("plan");
    expect(linkView({ view: "plan", focus: "sym:src/a.rs#run" })).toBe("plan");
    expect(linkView({ view: null, focus: null })).toBe("plan");
  });

  it("writes a link back, keeping the other parameters", () => {
    expect(linkSearch("?debug", { view: "flows", focus: "sym:src/a.rs#run" })).toBe("?view=flows&focus=sym:src/a.rs%23run&debug");
    expect(linkSearch("?view=plan&focus=dir:src", { view: null, focus: null })).toBe("");
    expect(encodeFocus("file:src/a b&c.ts")).toBe("file:src/a%20b%26c.ts");
    // What is written reads back the same.
    const link = { view: "plan" as const, focus: "dir:src/app/(app)/[id]" };
    expect(parseDeepLink(linkSearch("", link))).toEqual(link);
  });

  it("finds the plan node of a focus: ids, paths, a function's file, libraries", () => {
    expect(planTarget(h, "dir:src/lib")).toBe("dir:src/lib");
    expect(planTarget(h, "src/lib/")).toBe("dir:src/lib");
    expect(planTarget(h, "./src/lib/db/schema.ts")).toBe("file:src/lib/db/schema.ts");
    expect(planTarget(h, "sym:src/lib/db/schema.ts#users")).toBe("file:src/lib/db/schema.ts");
    expect(planTarget(h, "src/lib/db/schema.ts#users")).toBe("file:src/lib/db/schema.ts");
    expect(planTarget(h, "dir:")).toBe("repo:");
    expect(planTarget(h, "external:npm:next")).toBe("ext:npm:next");
    expect(planTarget(h, "dir:src/nope")).toBeNull();
    expect(planTarget(h, "sym:src/nope.ts#x")).toBeNull();
    expect(planTarget(h, "nope/at/all")).toBeNull();
  });

  it("turns what the page shows into a link", () => {
    const at = (mode: "plan" | "flows", selected: string | null, from: string | null = null) =>
      currentLink({ mode, selected, flowFrom: from ? { id: from } : null });
    expect(at("plan", "dir:src/lib")).toEqual({ view: "plan", focus: "dir:src/lib" });
    expect(at("plan", null)).toEqual({ view: null, focus: null });
    expect(at("plan", "repo:")).toEqual({ view: null, focus: null });
    expect(at("plan", "extmore:npm")).toEqual({ view: null, focus: null });
    expect(at("flows", "dir:src", "sym:src/a.rs#run")).toEqual({ view: "flows", focus: "sym:src/a.rs#run" });
  });
});

/** The API the plan controller needs, over the mock index. */
function planApi(): ControllerApi {
  const g = 1;
  return {
    base: "/api/",
    url: (p: string) => `/api/${p}`,
    meta: async () => ({}) as never,
    tree: async () => ({ generation: g, root: index.tree() }),
    graph: async (q: GraphQuery) => ({ generation: g, ...index.graph(q.expanded, { depth: q.depth, collapsed: q.collapsed }) }),
    node: async (id: string) => ({ generation: g, ...index.node(id) }) as never,
    issues: async () => ({ generation: g, ...index.issues() }),
    findings: async () => ({ generation: g, findings: [], summary: { total: 0, violations: 0, by_severity: {}, by_kind: {} }, config: { origin: null, error: null }, total: 0, truncated: false }),
    search: async () => ({ generation: g, results: [] }),
    reindex: async () => {},
  } as unknown as ControllerApi;
}

/** A window whose URL the links read and write. */
function fakeWindow(search: string) {
  const pop: (() => void)[] = [];
  const win: LinkWindow & { urls: string[]; pop: () => void } = {
    location: { search, hash: "", pathname: "/" },
    history: {
      replaceState: (_d, _u, url) => {
        win.urls.push(url ?? "");
        win.location.search = (url ?? "").replace(/^\/|#.*$/g, "");
      },
    },
    addEventListener: (_t, l) => pop.push(l),
    urls: [],
    pop: () => pop.forEach((l) => l()),
  };
  return win;
}

function setup(search: string) {
  const plan = new Controller(planApi());
  const notices: string[] = [];
  const opened: string[] = [];
  const deps: LinkDeps = {
    plan,
    flows: {
      open: async (id: string) => {
        opened.push(id);
        store.set({ mode: "flows", flowFrom: { id, entry: null }, flowError: id.includes("nope") ? "unknown node" : null });
      },
      setMode: (mode) => store.set({ mode }),
      loadEntries: async () => {
        opened.push("first entry");
      },
    },
    notify: (title, description) => notices.push(`${title}: ${description}`),
  };
  const win = fakeWindow(search);
  return { plan, deps, win, notices, opened, links: new DeepLinks(win, deps) };
}

describe("deep links on the page", () => {
  beforeEach(() => store.set(initialState));

  it("waits for the first plan, opens the folders above the focus and flies once it is drawn", async () => {
    const { plan, win, links } = setup("?view=plan&focus=file:src/lib/db/schema.ts&debug");
    links.start();
    expect(store.get().selected).toBeNull(); // nothing loaded yet: nothing to select
    await plan.loadAll({ keepView: false });
    await flush();
    const s = store.get();
    expect(s.mode).toBe("plan");
    expect(s.selected).toBe("file:src/lib/db/schema.ts");
    expect(s.view.open).toEqual(["dir:src", "dir:src/lib", "dir:src/lib/db"]);
    // The fly waits for the drawing of that view (the camera never jumps on an old plan).
    expect(plan.takePendingFly(s.graphView)).toBe("file:src/lib/db/schema.ts");
    // The URL now follows the selection, other parameters kept.
    expect(win.location.search).toBe("?view=plan&focus=file:src/lib/db/schema.ts&debug");
    plan.select("dir:src/app");
    expect(win.location.search).toBe("?view=plan&focus=dir:src/app&debug");
  });

  it("flies on the first drawing when the focus is already on the plan", async () => {
    const { plan, links } = setup("?focus=dir:src/lib");
    await plan.loadAll({ keepView: false });
    links.start();
    await flush();
    expect(store.get().selected).toBe("dir:src/lib");
    expect(plan.takePendingFly(store.get().graphView)).toBe("dir:src/lib");
  });

  it("says so when the plan has no such node, and selects nothing", async () => {
    const { plan, links, notices } = setup("?view=plan&focus=dir:src/nope");
    await plan.loadAll({ keepView: false });
    links.start();
    await flush();
    expect(store.get().selected).toBeNull();
    expect(notices).toHaveLength(1);
    expect(notices[0]).toContain("dir:src/nope");
  });

  it("opens the flows at a function; an unknown one falls back to the first entry with a notice", async () => {
    const { plan, links, opened, notices, win } = setup("?view=flows&focus=sym:src/a.ts%23run");
    await plan.loadAll({ keepView: false });
    links.start();
    await flush();
    expect(store.get().mode).toBe("flows");
    expect(opened).toEqual(["sym:src/a.ts#run"]);
    expect(win.location.search).toBe("?view=flows&focus=sym:src/a.ts%23run");

    // Back/forward to another link applies it too.
    win.location.search = "?view=flows&focus=sym:src/nope.ts%23x";
    win.pop();
    await flush();
    expect(opened).toEqual(["sym:src/a.ts#run", "sym:src/nope.ts#x", "first entry"]);
    expect(notices).toHaveLength(1);
    expect(store.get().flowFrom).toBeNull();
  });

  it("leaves a page without a link alone", async () => {
    const { plan, links, win } = setup("?debug");
    await plan.loadAll({ keepView: false });
    links.start();
    await flush();
    expect(store.get().selected).toBeNull();
    expect(win.urls).toEqual([]);
  });
});
