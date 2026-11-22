import { describe, expect, it } from "vitest";
import { saasFixture } from "../mock/fixtures/saas";
import { MockIndex } from "../mock/model";
import type { TreeNode } from "../src/api/types";
import { buildHierarchy, frontier } from "../src/model/hierarchy";
import {
  atLevel,
  canOpenFolder,
  closeFolder,
  frontierSize,
  graphQuery,
  initialView,
  isOpen,
  openDirs,
  openFolder,
  resolveView,
  revealView,
  toggleFolder,
  TOP_LEVEL,
  viewKey,
  type ViewSpec,
} from "../src/model/view";
import { todoLevel } from "../src/state/controller";

const index = new MockIndex(saasFixture());
const h = buildHierarchy(index.tree());

/** What the mock server returns for a view, as /api/graph would get it. */
const serverNodes = (v: ViewSpec) => {
  const q = graphQuery(v);
  return index.graph(q.expanded, { depth: q.depth, collapsed: q.collapsed }).nodes.map((n) => n.id);
};

/** A tree from { path: files } for dirs; every dir gets one file per counted file. */
function tree(dirs: Record<string, number>, rootFiles = 0): TreeNode {
  const node = (path: string, kind: TreeNode["kind"], files: number): TreeNode => ({
    id: kind === "repo" ? "repo:" : `${kind}:${path}`,
    kind,
    name: path.slice(path.lastIndexOf("/") + 1) || "repo",
    path,
    loc: files * 10,
    files,
    fan_in: 0,
    fan_out: 0,
    unresolved: 0,
    children: kind === "file" ? undefined : [],
  });
  const root = node("", "repo", 0);
  const byPath = new Map<string, TreeNode>([["", root]]);
  for (const path of Object.keys(dirs).sort()) {
    const parent = byPath.get(path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "")!;
    const d = node(path, "dir", 0);
    parent.children!.push(d);
    byPath.set(path, d);
  }
  for (const [path, n] of Object.entries(dirs)) {
    for (let i = 0; i < n; i++) byPath.get(path)!.children!.push(node(`${path}/f${i}.ts`, "file", 1));
  }
  for (let i = 0; i < rootFiles; i++) root.children!.push(node(`r${i}.json`, "file", 1));
  const count = (t: TreeNode): number => (t.kind === "file" ? 1 : (t.files = (t.children ?? []).reduce((s, c) => s + count(c), 0)));
  count(root);
  return root;
}

describe("levels and hand-made changes", () => {
  it("opens everything above the level, and the frontier matches the server's", () => {
    for (const level of [1, 2, 3, 4, "all"] as const) {
      const v = atLevel(level);
      expect(frontier(h, openDirs(h, v)), `level ${level}`).toEqual(serverNodes(v));
    }
    expect(openDirs(h, TOP_LEVEL)).toEqual([]);
    expect(openDirs(h, atLevel(2))).toContain("dir:src");
    expect(openDirs(h, atLevel(2))).not.toContain("dir:src/lib");
    expect(frontier(h, openDirs(h, atLevel("all"))).every((id) => id.startsWith("file:"))).toBe(true);
  });

  it("keeps sibling folders open: opening one never closes another", () => {
    let v = openFolder(h, TOP_LEVEL, "dir:src/lib");
    expect(v.open).toEqual(["dir:src", "dir:src/lib"]);
    v = openFolder(h, v, "dir:src/app");
    expect(openDirs(h, v)).toEqual(expect.arrayContaining(["dir:src", "dir:src/lib", "dir:src/app"]));
    expect(frontier(h, openDirs(h, v))).toEqual(serverNodes(v));
  });

  it("closes a folder inside the level with `collapsed`, and forgets what was opened under it", () => {
    let v = openFolder(h, atLevel(2), "dir:src/lib/db");
    expect(v.open).toEqual(["dir:src/lib", "dir:src/lib/db"]);
    v = closeFolder(h, v, "dir:src");
    expect(v).toEqual({ level: 2, open: [], closed: ["dir:src"] });
    expect(isOpen(h, v, "dir:src")).toBe(false);
    expect(frontier(h, openDirs(h, v))).toContain("dir:src");
    expect(frontier(h, openDirs(h, v))).toEqual(serverNodes(v));
    // Opening it again takes it out of `collapsed`.
    expect(toggleFolder(h, v, "dir:src")).toEqual(atLevel(2));
  });

  it("reveals a deep file by opening the folders above it, and leaves the rest open", () => {
    const file = "file:src/app/(app)/projects/[id]/page.tsx";
    const start = openFolder(h, TOP_LEVEL, "dir:src/lib");
    const v = revealView(h, start, file);
    expect(v.open).toEqual(["dir:src", "dir:src/app", "dir:src/app/(app)", "dir:src/app/(app)/projects", "dir:src/app/(app)/projects/[id]", "dir:src/lib"]);
    expect(frontier(h, openDirs(h, v))).toContain(file);
    // Already on the plan (a card, or an open box): nothing changes.
    expect(revealView(h, v, "dir:src/components")).toBe(v);
    expect(revealView(h, v, "dir:src/lib")).toBe(v);
    expect(revealView(h, v, "dir:src")).toBe(v);
  });

  it("drops folders a re-index deleted, and overrides the level already covers", () => {
    const v = resolveView(h, { level: 2, open: ["dir:src", "dir:src/lib", "dir:gone"], closed: ["dir:src/lib/db", "dir:also-gone"] });
    // dir:src is open by the level; dir:src/lib/db lies below it, so closing it says nothing.
    expect(v).toEqual({ level: 2, open: ["dir:src/lib"], closed: [] });
    expect(viewKey(v)).toBe(viewKey({ level: 2, open: ["dir:src/lib"], closed: [] }));
  });

  it("sends the level as depth and only the hand-made changes as ids", () => {
    const v = closeFolder(h, openFolder(h, atLevel(2), "dir:src/lib"), "dir:tests");
    expect(graphQuery(v)).toEqual({ depth: 2, expanded: ["dir:src/lib"], collapsed: ["dir:tests"] });
    expect(graphQuery(atLevel("all"))).toEqual({ depth: "all", expanded: [], collapsed: [] });
  });

  it("counts the cards of a view from the tree, and caps Todo on a huge tree", () => {
    expect(frontierSize(h, TOP_LEVEL)).toBe(serverNodes(TOP_LEVEL).length);
    expect(frontierSize(h, atLevel("all"))).toBe(serverNodes(atLevel("all")).length);
    expect(todoLevel(h)).toBe("all");
    const capped = todoLevel(h, 40);
    expect(typeof capped).toBe("number");
    expect(frontierSize(h, atLevel(capped))).toBeLessThanOrEqual(40);
    expect(frontierSize(h, atLevel((capped as number) + 1))).toBeGreaterThan(40);
  });
});

describe("canOpenFolder", () => {
  it("is true only for folders with something inside", () => {
    const t = buildHierarchy(tree({ code: 2, empty: 0 }));
    expect(canOpenFolder(t, "dir:code")).toBe(true);
    expect(canOpenFolder(t, "dir:empty")).toBe(false);
    expect(canOpenFolder(t, "file:code/f0.ts")).toBe(false);
    expect(canOpenFolder(t, "dir:missing")).toBe(false);
  });
});

describe("first screen", () => {
  it("is level 1 with the folder that holds the code of a typical app opened", () => {
    const v = initialView(h);
    expect(v).toEqual({ level: 1, open: ["dir:src"], closed: [] });
    const q = graphQuery(v);
    const g = index.graph(q.expanded, { depth: q.depth });
    const ranked = [...g.nodes].sort((a, b) => b.fan_in - a.fan_in).map((n) => n.id);
    expect(ranked[0]).toBe("dir:src/lib");
  });

  it("follows a chain of dominant folders and stops where the parts are", () => {
    // packages/app holds nearly everything; inside it, ui and test are comparable.
    const t = tree({ packages: 0, "packages/app": 0, "packages/app/ui": 40, "packages/app/test": 50, "packages/app/core": 10, docs: 3 }, 12);
    expect(initialView(buildHierarchy(t)).open).toEqual(["dir:packages", "dir:packages/app"]);
  });

  it("stays at the top level when no folder dominates", () => {
    const t = tree({ api: 30, web: 28, shared: 10 }, 2);
    expect(initialView(buildHierarchy(t))).toEqual(TOP_LEVEL);
  });
});
