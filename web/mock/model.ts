// In-memory index that answers the cartograph API from a raw fixture.
// It follows docs/api-f2.md including the backend clarifications:
// edge weight = distinct file pairs, `repo:` root, `dir:` alias, wrapped responses.
import type {
  Confidence,
  Counts,
  DirDetail,
  ExtDetail,
  FileDetail,
  FlatNode,
  GraphEdge,
  GraphExternal,
  GraphNode,
  GraphResponse,
  IssuesResponse,
  PackageInfo,
  SearchResult,
  TreeNode,
} from "../src/api/types";

export type RawTarget =
  | { kind: "file"; path: string; confidence: Confidence }
  | { kind: "ext"; ext: string }
  | { kind: "unresolved"; reason: string; detail: string };

export interface RawImport {
  line: number;
  specifier: string;
  to: RawTarget;
}

export interface RawFile {
  path: string;
  lang: string;
  loc: number;
  imports: RawImport[];
}

export interface RawRepo {
  name: string;
  root: string;
  files: RawFile[];
  skipped: { path: string; reason: string; detail: string }[];
  diagnostics: { path: string; detail: string }[];
  packages: PackageInfo[];
}

/** `depth` and `collapsed` of /api/graph (`expanded` is passed apart). */
export interface ViewOptions {
  depth?: number | "all";
  collapsed?: readonly string[];
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: "not_found" | "bad_request" | "indexing" | "internal",
    message: string,
  ) {
    super(message);
  }
}

interface DirInfo {
  path: string;
  dirs: string[];
  files: string[];
}

const CONF_RANK: Record<Confidence, number> = { exact: 3, resolved: 2, heuristic: 1 };

export function parentPath(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? "" : path.slice(0, i);
}

export function dirId(path: string): string {
  return path === "" ? "repo:" : `dir:${path}`;
}

function baseName(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? path : path.slice(i + 1);
}

/** Normalizes an incoming id: `dir:` (empty path) is an alias of `repo:`, and store prefixes are accepted. */
export function normalizeId(id: string): string {
  if (id === "dir:" || id === "dir") return "repo:";
  if (id.startsWith("package:")) return `pkg:${id.slice(8)}`;
  if (id.startsWith("external:")) return `ext:${id.slice(9)}`;
  return id;
}

function extName(ext: string): string {
  const i = ext.indexOf(":");
  return i < 0 ? ext : ext.slice(i + 1);
}

export class MockIndex {
  readonly files = new Map<string, RawFile>();
  readonly dirs = new Map<string, DirInfo>();
  /** Distinct file pairs src -> dst with the best confidence among their imports. */
  readonly pairs = new Map<string, Map<string, Confidence>>();
  /** Distinct (file, ext) pairs. */
  readonly extPairs = new Map<string, Set<string>>();
  private readonly dirMetrics = new Map<string, TreeNode>();

  constructor(readonly repo: RawRepo) {
    this.dirs.set("", { path: "", dirs: [], files: [] });
    for (const file of repo.files) {
      this.files.set(file.path, file);
      this.ensureDir(parentPath(file.path)).files.push(file.path);
    }
    for (const dir of this.dirs.values()) {
      dir.dirs.sort();
      dir.files.sort();
    }
    for (const file of repo.files) {
      for (const imp of file.imports) {
        if (imp.to.kind === "file" && this.files.has(imp.to.path) && imp.to.path !== file.path) {
          let out = this.pairs.get(file.path);
          if (!out) this.pairs.set(file.path, (out = new Map()));
          const prev = out.get(imp.to.path);
          if (!prev || CONF_RANK[imp.to.confidence] > CONF_RANK[prev]) {
            out.set(imp.to.path, imp.to.confidence);
          }
        } else if (imp.to.kind === "ext") {
          let set = this.extPairs.get(file.path);
          if (!set) this.extPairs.set(file.path, (set = new Set()));
          set.add(imp.to.ext);
        }
      }
    }
    // Dir fan-in/out: for a pair src -> dst, every dir that holds dst but not src gains src as
    // an importer from outside, and every dir that holds src but not dst gains dst as imported.
    const chain = (p: string) => {
      const out: string[] = [];
      for (let d = parentPath(p); ; d = parentPath(d)) {
        out.push(d);
        if (d === "") break;
      }
      return out;
    };
    const addTo = (m: Map<string, Set<string>>, dir: string, file: string) => {
      let s = m.get(dir);
      if (!s) m.set(dir, (s = new Set()));
      s.add(file);
    };
    for (const [src, dsts] of this.pairs) {
      const srcChain = chain(src);
      const srcSet = new Set(srcChain);
      for (const dst of dsts.keys()) {
        this.fileFanIn.set(dst, (this.fileFanIn.get(dst) ?? 0) + 1);
        const dstChain = chain(dst);
        const dstSet = new Set(dstChain);
        for (const d of dstChain) if (!srcSet.has(d)) addTo(this.dirImporters, d, src);
        for (const d of srcChain) if (!dstSet.has(d)) addTo(this.dirImported, d, dst);
      }
    }
    this.computeDirMetrics("");
  }

  private readonly fileFanIn = new Map<string, number>();
  private readonly dirImporters = new Map<string, Set<string>>();
  private readonly dirImported = new Map<string, Set<string>>();

  private ensureDir(path: string): DirInfo {
    let dir = this.dirs.get(path);
    if (dir) return dir;
    dir = { path, dirs: [], files: [] };
    this.dirs.set(path, dir);
    if (path !== "") this.ensureDir(parentPath(path)).dirs.push(path);
    return dir;
  }

  private isInside(filePath: string, dirPath: string): boolean {
    return dirPath === "" || filePath.startsWith(`${dirPath}/`);
  }

  private computeDirMetrics(path: string): TreeNode {
    const dir = this.dirs.get(path)!;
    let files = 0;
    let loc = 0;
    let unresolved = 0;
    const langs: Record<string, number> = {};
    for (const child of dir.dirs) {
      const m = this.computeDirMetrics(child);
      files += m.files ?? 0;
      loc += m.loc;
      unresolved += m.unresolved;
      for (const [k, v] of Object.entries(m.langs ?? {})) langs[k] = (langs[k] ?? 0) + v;
    }
    for (const f of dir.files) {
      const file = this.files.get(f)!;
      files += 1;
      loc += file.loc;
      unresolved += file.imports.filter((i) => i.to.kind === "unresolved").length;
      langs[file.lang] = (langs[file.lang] ?? 0) + 1;
    }
    const importersOutside = this.dirImporters.get(path) ?? new Set<string>();
    const importedOutside = this.dirImported.get(path) ?? new Set<string>();
    const node: TreeNode = {
      id: dirId(path),
      kind: path === "" ? "repo" : "dir",
      name: path === "" ? this.repo.name : baseName(path),
      path,
      loc,
      files,
      fan_in: path === "" ? 0 : importersOutside.size,
      fan_out: path === "" ? 0 : importedOutside.size,
      unresolved,
      langs,
    };
    this.dirMetrics.set(path, node);
    return node;
  }

  fileNode(path: string): TreeNode {
    const file = this.files.get(path)!;
    const fanIn = this.fileFanIn.get(path) ?? 0;
    return {
      id: `file:${path}`,
      kind: "file",
      name: baseName(path),
      path,
      lang: file.lang,
      loc: file.loc,
      fan_in: fanIn,
      fan_out: this.pairs.get(path)?.size ?? 0,
      unresolved: file.imports.filter((i) => i.to.kind === "unresolved").length,
    };
  }

  private dirPathFromId(id: string): string | null {
    const n = normalizeId(id);
    if (n === "repo:") return "";
    if (n.startsWith("dir:")) {
      const p = n.slice(4);
      return this.dirs.has(p) ? p : null;
    }
    return null;
  }

  /** Whether an id (in any accepted form) names an existing directory. */
  hasDir(id: string): boolean {
    return this.dirPathFromId(id) !== null;
  }

  tree(rootId = "repo:", depth?: number): TreeNode {
    const n = normalizeId(rootId);
    if (n.startsWith("file:")) {
      if (this.files.has(n.slice(5))) throw new ApiError(400, "bad_request", "root es un archivo");
      throw new ApiError(404, "not_found", `no existe ${rootId}`);
    }
    const path = this.dirPathFromId(n);
    if (path === null) throw new ApiError(404, "not_found", `no existe ${rootId}`);
    const build = (p: string, level: number): TreeNode => {
      const base = { ...this.dirMetrics.get(p)! };
      const dir = this.dirs.get(p)!;
      const hasChildren = dir.dirs.length + dir.files.length > 0;
      if (depth !== undefined && level >= depth) {
        if (hasChildren) base.truncated = true;
        return base;
      }
      base.children = [
        ...dir.dirs.map((d) => build(d, level + 1)),
        ...dir.files.map((f) => this.fileNode(f)),
      ];
      return base;
    };
    return build(path, 0);
  }

  /**
   * The visible frontier (see /api/graph): a dir is open when its parent is open, it isn't
   * collapsed, and it lies above `depth` (top-level dirs are level 1) or is expanded.
   */
  frontier(expanded: readonly string[], view: ViewOptions = {}): { nodes: string[]; ignored: string[]; open: Set<string> } {
    const depth = view.depth ?? 1;
    const collapsed = view.collapsed ?? [];
    const requested = new Set<string>();
    for (const id of expanded) {
      const p = this.dirPathFromId(id);
      if (p !== null) requested.add(p);
    }
    const closed = new Set<string>();
    const badCollapsed: string[] = [];
    for (const id of collapsed) {
      const p = this.dirPathFromId(id);
      if (p === null || p === "") badCollapsed.push(id);
      else closed.add(p);
    }
    const open = new Set<string>();
    const nodes: string[] = [];
    const walk = (p: string, level: number) => {
      open.add(p);
      const dir = this.dirs.get(p)!;
      for (const d of dir.dirs) {
        const byDepth = depth === "all" || level + 1 < depth;
        if (!closed.has(d) && (byDepth || requested.has(d))) walk(d, level + 1);
        else nodes.push(dirId(d));
      }
      for (const f of dir.files) nodes.push(`file:${f}`);
    };
    walk("", 0);
    const unknown = expanded.filter((id) => this.dirPathFromId(id) === null);
    const shut = expanded.filter((id) => {
      const p = this.dirPathFromId(id);
      return p !== null && !open.has(p);
    });
    const ignored = [...new Set([...unknown, ...badCollapsed, ...shut])];
    return { nodes, ignored, open };
  }

  /** Maps a file path to the frontier node that contains it. */
  private representative(path: string, frontierSet: Set<string>): string | null {
    const fileId = `file:${path}`;
    if (frontierSet.has(fileId)) return fileId;
    let p = parentPath(path);
    for (;;) {
      const id = dirId(p);
      if (frontierSet.has(id)) return id;
      if (p === "") return null;
      p = parentPath(p);
    }
  }

  private rollup(frontierNodes: string[]): { edges: GraphEdge[]; externals: GraphExternal[] } {
    const frontierSet = new Set(frontierNodes);
    const rep = new Map<string, string | null>();
    const repOf = (p: string) => {
      let r = rep.get(p);
      if (r === undefined) rep.set(p, (r = this.representative(p, frontierSet)));
      return r;
    };
    const edges = new Map<string, GraphEdge>();
    for (const [src, dsts] of this.pairs) {
      const rs = repOf(src);
      if (!rs) continue;
      for (const [dst, conf] of dsts) {
        const rd = repOf(dst);
        if (!rd || rd === rs) continue;
        const key = `${rs}\u0000${rd}`;
        let e = edges.get(key);
        if (!e) {
          e = { src: rs, dst: rd, weight: 0, confidence: { exact: 0, resolved: 0, heuristic: 0 } };
          edges.set(key, e);
        }
        e.weight += 1;
        e.confidence[conf] += 1;
      }
    }
    const externals = new Map<string, GraphExternal>();
    for (const [src, exts] of this.extPairs) {
      const rs = repOf(src);
      if (!rs) continue;
      for (const ext of exts) {
        const key = `${rs}\u0000${ext}`;
        const e = externals.get(key);
        if (e) e.weight += 1;
        else externals.set(key, { src: rs, ext, weight: 1 });
      }
    }
    const order = (a: { src: string }, b: { src: string }) => (a.src < b.src ? -1 : a.src > b.src ? 1 : 0);
    return {
      edges: [...edges.values()].sort((a, b) => order(a, b) || (a.dst < b.dst ? -1 : 1)),
      externals: [...externals.values()].sort((a, b) => order(a, b) || (a.ext < b.ext ? -1 : 1)),
    };
  }

  graph(expanded: readonly string[], view: ViewOptions = {}): Omit<GraphResponse, "generation"> {
    const { nodes: ids, ignored } = this.frontier(expanded, view);
    const { edges, externals } = this.rollup(ids);
    const fanIn = new Map<string, number>();
    const fanOut = new Map<string, number>();
    for (const e of edges) {
      fanIn.set(e.dst, (fanIn.get(e.dst) ?? 0) + e.weight);
      fanOut.set(e.src, (fanOut.get(e.src) ?? 0) + e.weight);
    }
    const nodes: GraphNode[] = ids.map((id) => {
      if (id.startsWith("file:")) {
        const f = this.fileNode(id.slice(5));
        return {
          id,
          kind: "file",
          name: f.name,
          path: f.path,
          parent: dirId(parentPath(f.path)),
          loc: f.loc,
          files: 1,
          langs: { [f.lang!]: 1 },
          lang: f.lang!,
          fan_in: fanIn.get(id) ?? 0,
          fan_out: fanOut.get(id) ?? 0,
          unresolved: f.unresolved,
          expandable: false,
        };
      }
      const path = id.slice(4);
      const m = this.dirMetrics.get(path)!;
      const dir = this.dirs.get(path)!;
      return {
        id,
        kind: "dir",
        name: m.name,
        path,
        parent: dirId(parentPath(path)),
        loc: m.loc,
        files: m.files ?? 0,
        langs: m.langs ?? {},
        fan_in: fanIn.get(id) ?? 0,
        fan_out: fanOut.get(id) ?? 0,
        unresolved: m.unresolved,
        expandable: dir.dirs.length + dir.files.length > 0,
      };
    });
    return { nodes, edges, externals, ignored, hidden_namespace_imports: 0 };
  }

  /** Number of file -> file pairs whose endpoints fall in distinct frontier nodes (the rollup invariant). */
  crossingPairs(expanded: readonly string[], view: ViewOptions = {}): number {
    const { nodes } = this.frontier(expanded, view);
    const set = new Set(nodes);
    let n = 0;
    for (const [src, dsts] of this.pairs) {
      for (const dst of dsts.keys()) {
        const a = this.representative(src, set);
        const b = this.representative(dst, set);
        if (a && b && a !== b) n += 1;
      }
    }
    return n;
  }

  node(rawId: string): Omit<FileDetail, "generation"> | Omit<DirDetail, "generation"> | Omit<ExtDetail, "generation"> | { node: PackageInfo } {
    const id = normalizeId(rawId);
    if (id.startsWith("file:")) {
      const path = id.slice(5);
      const file = this.files.get(path);
      if (!file) throw new ApiError(404, "not_found", `no existe ${rawId}`);
      const importers: FileDetail["importers"] = [];
      for (const other of this.files.values()) {
        for (const imp of other.imports) {
          if (imp.to.kind === "file" && imp.to.path === path && other.path !== path) {
            importers.push({ source: `file:${other.path}`, line: imp.line, confidence: imp.to.confidence });
          }
        }
      }
      importers.sort((a, b) => (a.source < b.source ? -1 : a.source > b.source ? 1 : a.line - b.line));
      const node = this.fileNode(path) as FlatNode & { kind: "file" };
      return {
        node,
        imports: file.imports
          .filter((i) => i.to.kind === "file")
          .map((i) => {
            const to = i.to as Extract<RawTarget, { kind: "file" }>;
            return { target: `file:${to.path}`, specifier: i.specifier, line: i.line, confidence: to.confidence };
          }),
        importers,
        externals: file.imports
          .filter((i) => i.to.kind === "ext")
          .map((i) => ({ ext: `ext:${(i.to as { ext: string }).ext}`, specifier: i.specifier, line: i.line })),
        unresolved: file.imports
          .filter((i) => i.to.kind === "unresolved")
          .map((i) => {
            const to = i.to as Extract<RawTarget, { kind: "unresolved" }>;
            return { line: i.line, specifier: i.specifier, reason: to.reason, detail: to.detail };
          }),
        package: this.repo.packages[0]?.id ?? null,
      };
    }
    if (id.startsWith("ext:")) {
      const ext = id.slice(4);
      const importers: ExtDetail["importers"] = [];
      for (const file of this.files.values()) {
        for (const imp of file.imports) {
          if (imp.to.kind === "ext" && imp.to.ext === ext) {
            importers.push({ source: `file:${file.path}`, specifier: imp.specifier, line: imp.line });
          }
        }
      }
      if (importers.length === 0) throw new ApiError(404, "not_found", `no existe ${rawId}`);
      return { node: { id, kind: "ext", name: extName(ext) }, importers };
    }
    if (id.startsWith("pkg:")) {
      const pkg = this.repo.packages.find((p) => p.id === id);
      if (!pkg) throw new ApiError(404, "not_found", `no existe ${rawId}`);
      return { node: pkg };
    }
    const path = this.dirPathFromId(id);
    if (path === null) throw new ApiError(404, "not_found", `no existe ${rawId}`);
    const m = this.dirMetrics.get(path)!;
    const dir = this.dirs.get(path)!;
    const children = [
      ...dir.dirs.map((d) => {
        const c = this.dirMetrics.get(d)!;
        return { id: c.id, kind: "dir" as const, name: c.name, loc: c.loc, files: c.files ?? 0 };
      }),
      ...dir.files.map((f) => {
        const c = this.fileNode(f);
        return { id: c.id, kind: "file" as const, name: c.name, loc: c.loc, files: 1 };
      }),
    ]
      .sort((a, b) => b.loc - a.loc || (a.id < b.id ? -1 : 1))
      .slice(0, 10);
    // "Sibling level": the frontier that results from expanding every ancestor of the dir.
    const ancestors: string[] = [];
    for (let p = parentPath(path); ; p = parentPath(p)) {
      ancestors.push(dirId(p));
      if (p === "") break;
    }
    let depsOut: { id: string; weight: number }[] = [];
    let depsIn: { id: string; weight: number }[] = [];
    if (path !== "") {
      const { nodes } = this.frontier(ancestors);
      const { edges } = this.rollup(nodes);
      const self = dirId(path);
      depsOut = edges.filter((e) => e.src === self).map((e) => ({ id: e.dst, weight: e.weight }));
      depsIn = edges.filter((e) => e.dst === self).map((e) => ({ id: e.src, weight: e.weight }));
    }
    const byWeight = (a: { weight: number; id: string }, b: { weight: number; id: string }) =>
      b.weight - a.weight || (a.id < b.id ? -1 : 1);
    const extCounts = new Map<string, number>();
    for (const [src, exts] of this.extPairs) {
      if (!this.isInside(src, path)) continue;
      for (const e of exts) extCounts.set(e, (extCounts.get(e) ?? 0) + 1);
    }
    const flat: TreeNode = { ...m };
    delete flat.children;
    return {
      node: flat as FlatNode & { kind: "dir" | "repo" },
      children,
      deps_out: depsOut.sort(byWeight).slice(0, 10),
      deps_in: depsIn.sort(byWeight).slice(0, 10),
      externals: [...extCounts]
        .map(([ext, weight]) => ({ ext: `ext:${ext}`, weight }))
        .sort((a, b) => b.weight - a.weight || (a.ext < b.ext ? -1 : 1))
        .slice(0, 10),
    };
  }

  issues(limit = 500): Omit<IssuesResponse, "generation"> {
    const unresolved: IssuesResponse["unresolved"] = [];
    const byReason: Record<string, number> = {};
    const paths = [...this.files.keys()].sort();
    for (const p of paths) {
      for (const imp of this.files.get(p)!.imports) {
        if (imp.to.kind !== "unresolved") continue;
        unresolved.push({ file: p, line: imp.line, specifier: imp.specifier, reason: imp.to.reason, detail: imp.to.detail });
        byReason[imp.to.reason] = (byReason[imp.to.reason] ?? 0) + 1;
      }
    }
    const skipped = [...this.repo.skipped].sort((a, b) => (a.path < b.path ? -1 : 1));
    const cap = Math.min(Math.max(limit, 0), 100000);
    return {
      unresolved: unresolved.slice(0, cap),
      by_reason: byReason,
      diagnostics: this.repo.diagnostics,
      skipped: skipped.slice(0, cap),
      truncated: unresolved.length > cap || skipped.length > cap,
      total: { unresolved: unresolved.length, skipped: skipped.length },
    };
  }

  search(q: string, limit = 50): SearchResult[] {
    const query = q.trim().toLowerCase();
    if (!query) return [];
    const out: SearchResult[] = [];
    const consider = (id: string, kind: SearchResult["kind"], path: string, name: string) => {
      const score = fuzzyScore(query, path.toLowerCase(), name.toLowerCase());
      if (score > 0) out.push({ id, kind, path, name, score });
    };
    for (const p of this.dirs.keys()) if (p) consider(dirId(p), "dir", p, baseName(p));
    for (const p of this.files.keys()) consider(`file:${p}`, "file", p, baseName(p));
    const exts = new Set<string>();
    for (const s of this.extPairs.values()) for (const e of s) exts.add(e);
    for (const e of exts) consider(`ext:${e}`, "ext", e, extName(e));
    out.sort((a, b) => b.score - a.score || a.path.length - b.path.length || (a.path < b.path ? -1 : 1));
    return out.slice(0, Math.min(Math.max(limit, 1), 500));
  }

  counts(): Counts {
    let imports = 0;
    let resolved = 0;
    let external = 0;
    let unresolved = 0;
    let loc = 0;
    for (const f of this.files.values()) {
      loc += f.loc;
      for (const i of f.imports) {
        imports += 1;
        if (i.to.kind === "file") resolved += 1;
        else if (i.to.kind === "ext") external += 1;
        else unresolved += 1;
      }
    }
    return {
      files: this.files.size,
      dirs: this.dirs.size - 1,
      packages: this.repo.packages.length,
      loc,
      imports,
      resolved,
      external,
      unresolved,
      skipped: this.repo.skipped.length,
      diagnostics: this.repo.diagnostics.length,
    };
  }

  languages(): { lang: string; files: number; loc: number }[] {
    const m = new Map<string, { lang: string; files: number; loc: number }>();
    for (const f of this.files.values()) {
      const e = m.get(f.lang) ?? { lang: f.lang, files: 0, loc: 0 };
      e.files += 1;
      e.loc += f.loc;
      m.set(f.lang, e);
    }
    return [...m.values()].sort((a, b) => b.files - a.files);
  }
}

/** Subsequence match with bonuses for contiguous runs and for matching the base name. 0 = no match. */
export function fuzzyScore(query: string, path: string, name: string): number {
  const inName = name.indexOf(query);
  if (inName >= 0) return 100 + (inName === 0 ? 40 : 0) + (query.length / name.length) * 30;
  const inPath = path.indexOf(query);
  if (inPath >= 0) return 60 + (query.length / path.length) * 20;
  let qi = 0;
  let run = 0;
  let score = 0;
  for (let i = 0; i < path.length && qi < query.length; i++) {
    if (path[i] === query[qi]) {
      qi += 1;
      run += 1;
      score += 1 + run;
    } else {
      run = 0;
    }
  }
  if (qi < query.length) return 0;
  return Math.min(55, score);
}
