// Orchestrates the API, live events and plan state. React components call these actions;
// the plan subscribes to camera requests.
import { toast } from "@ui-sdk/react/toast";
import { ApiClient, ApiError } from "../api/client";
import { LiveEvents } from "../api/events";
import type { IndexFinishedEvent, Meta, ServerEvent } from "../api/types";
import { buildHierarchy, canonicalId, frontier, type Hierarchy } from "../model/hierarchy";
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
  sameView,
  viewKey,
  type Level,
  type ViewSpec,
} from "../model/view";
import { store } from "./store";

export type CameraRequest = { type: "fly"; id: string } | { type: "fit" } | { type: "zoom"; factor: number };

type CameraListener = (req: CameraRequest) => void;

/** How long the cards changed by a re-index stay highlighted. */
export const PULSE_MS = 4000;

/**
 * "Todo" on a huge repo would ask ELK to lay out tens of thousands of cards. Past this many,
 * it opens the deepest level that stays under it, and the toolbar says so.
 */
export const TODO_MAX_CARDS = 2000;

/** The API surface the controller needs (an ApiClient, or a fake in tests). */
export type ControllerApi = Pick<ApiClient, "base" | "url" | "meta" | "tree" | "graph" | "node" | "issues" | "findings" | "search" | "reindex">;

/** The level "Todo" opens on this tree: every folder, or the deepest level under the card budget. */
export function todoLevel(h: Hierarchy, max = TODO_MAX_CARDS): Level {
  if (frontierSize(h, atLevel("all")) <= max) return "all";
  let level = 1;
  while (level < 64 && frontierSize(h, atLevel(level + 1)) <= max) level += 1;
  return level;
}

/**
 * The view "Todo" opens: every folder or, on a tree too big for that, the deepest level under
 * the card budget plus every folder open now. Todo never closes a folder: a level below the
 * current one, or below a folder opened by hand, would drop what the user opened.
 */
export function todoView(h: Hierarchy, current: ViewSpec, max = TODO_MAX_CARDS): ViewSpec {
  const level = todoLevel(h, max);
  if (level === "all") return atLevel("all");
  const deepest = typeof current.level === "number" ? Math.max(level, current.level) : level;
  return resolveView(h, { level: deepest, open: openDirs(h, current), closed: [] });
}

/** The level "Todo" stopped at, when the view shows a capped "Todo" (else null). */
export function cappedTodo(level: Level, todoCap: number | null): number | null {
  if (typeof level !== "number") return null;
  // A level past 3 only comes from "Todo".
  return level === todoCap || level > 3 ? level : null;
}

/**
 * The node that stands for a path on the plan of a view: itself, or its nearest closed
 * ancestor. Works by path, so a file the index skipped (not in the tree) still finds its folder.
 */
export function representativeOf(visible: ReadonlySet<string>, path: string): string | null {
  const parts = path.split("/");
  for (let i = parts.length; i > 0; i--) {
    const p = parts.slice(0, i).join("/");
    for (const id of [`file:${p}`, `dir:${p}`]) if (visible.has(id)) return id;
  }
  return null;
}

export class Controller {
  constructor(readonly api: ControllerApi = new ApiClient()) {}

  /** The card budget of "Todo" (smaller in tests). */
  todoMax = TODO_MAX_CARDS;

  private live: LiveEvents | null = null;
  private cameraListeners = new Set<CameraListener>();
  private graphSeq = 0;
  private detailSeq = 0;
  private issuesSeq = 0;
  private detailCache = new Map<string, unknown>();
  private booted = false;
  private loading: Promise<void> | null = null;
  private followUp: Promise<void> | null = null;
  /** A fly asked for while the plan that shows its node is still on its way. */
  private pendingFly: { id: string; view: string } | null = null;
  /** The folder just opened or closed: the plan keeps it where it is on screen once redrawn. */
  private pendingAnchor: { id: string; view: string } | null = null;
  /** The view of the last plan drawn (the plan reports each drawing through takePendingFly). */
  private drawnView: string | null = null;

  onCamera(l: CameraListener): () => void {
    this.cameraListeners.add(l);
    return () => {
      this.cameraListeners.delete(l);
    };
  }

  private camera(req: CameraRequest) {
    for (const l of this.cameraListeners) l(req);
  }

  boot() {
    if (this.booted) return;
    this.booted = true;
    this.live = new LiveEvents({
      url: this.api.url("events"),
      onEvent: (e) => this.handleEvent(e),
      onStale: () => void this.loadAll({ keepView: true }),
      onState: (connection) => store.set({ connection }),
    });
    this.live.start();
    void this.start();
  }

  async start() {
    store.set({ phase: "connecting", bootError: null });
    try {
      const meta = await this.api.meta();
      store.set({ meta, reindexing: Boolean(meta.reindexing), progress: meta.progress });
      this.live?.noteGeneration(meta.generation);
      if (meta.status === "error" && meta.counts === null) {
        // The index failed and there was never a snapshot: nothing to draw yet.
        store.set({
          phase: "error",
          bootError: `El indexado falló: ${meta.error ?? "sin detalle"}. Corrige el problema y pulsa Reintentar, o reindexa desde el servidor.`,
        });
        return;
      }
      if (meta.status === "indexing" || meta.counts === null) {
        store.set({ phase: "indexing" });
        return; // index-finished will load the plan.
      }
      if (meta.indexed_at) store.set({ lastIndexedAt: Date.parse(meta.indexed_at) || Date.now() });
      await this.loadAll({ keepView: false });
    } catch (err) {
      if (err instanceof ApiError && err.indexing) {
        store.set({ phase: "indexing", progress: err.progress });
        return;
      }
      store.set({
        phase: "error",
        bootError: `No hay respuesta de cartograph en ${this.api.base}. Arranca el servidor con "cartograph serve" y vuelve a intentarlo.`,
      });
    }
  }

  handleEvent(e: ServerEvent) {
    switch (e.type) {
      case "hello":
        if (store.get().phase === "error") void this.start();
        else void this.resync(e.data.status);
        break;
      case "index-started":
        store.set({ reindexing: store.get().phase === "ready", progress: null });
        break;
      case "index-progress":
        store.set({ progress: e.data });
        break;
      case "index-finished":
        void this.onIndexFinished(e.data);
        break;
      case "index-error":
        store.set({ reindexing: false, progress: null });
        if (store.get().phase === "indexing") {
          store.set({ phase: "error", bootError: `El indexado falló: ${e.data.message}` });
          break;
        }
        toast.error({ title: "El indexado falló", description: e.data.message, duration: 8000 });
        break;
    }
  }

  /**
   * After a (re)connect, events may have been missed: take the indexing state from
   * /api/meta instead of trusting what the last events left in the store.
   */
  private async resync(status: Meta["status"]) {
    if (status !== "indexing" && !store.get().reindexing && store.get().progress === null) return;
    try {
      const meta = await this.api.meta();
      const busy = meta.status === "indexing" || Boolean(meta.reindexing);
      store.set({ meta, reindexing: Boolean(meta.reindexing), progress: busy ? meta.progress : null });
      // The first index finished while we were away: draw the plan.
      if (store.get().phase === "indexing" && meta.status === "ready") void this.loadAll({ keepView: false });
    } catch {
      store.set({ reindexing: false, progress: null });
    }
  }

  private async onIndexFinished(data: IndexFinishedEvent) {
    const wasReady = store.get().phase === "ready";
    store.set({ reindexing: false, progress: null, lastIndexedAt: Date.now() });
    try {
      store.set({ meta: await this.api.meta() });
    } catch {
      // The status dot keeps the previous meta; the plan refresh below reports errors.
    }
    await this.loadAll({ keepView: wasReady });
    if (!wasReady) return;
    const s = store.get();
    if (s.hierarchy && s.graphView && data.changed.length > 0) {
      const visible = new Set(frontier(s.hierarchy, openDirs(s.hierarchy, s.graphView)));
      const ids = new Set<string>();
      for (const path of data.changed) {
        const rep = representativeOf(visible, path);
        if (rep) ids.add(rep);
      }
      const pulse = { ids: [...ids], at: Date.now() };
      store.set({ pulse });
      // Cleared in the store, so a card drawn later (a folder opened afterwards) doesn't pulse again.
      setTimeout(() => {
        if (store.get().pulse === pulse) store.set({ pulse: null });
      }, PULSE_MS);
      const n = data.changed_total ?? data.changed.length;
      toast({
        id: "reindex",
        title: "Plano actualizado",
        description: n === 1 ? `Cambió ${data.changed[0]}` : `Cambiaron ${n} archivos`,
        duration: 4000,
      });
    }
  }

  /**
   * Loads the tree and the graph for the current (or initial) view.
   * A call that arrives while a load is in flight means newer data exists (a new
   * generation, a reconnect): it runs one more load once the current one settles,
   * instead of reusing a load that started before that data existed.
   */
  loadAll({ keepView }: { keepView: boolean }): Promise<void> {
    if (this.loading) {
      this.followUp ??= this.loading.then(() => {
        this.followUp = null;
        return this.loadAll({ keepView: true });
      });
      return this.followUp;
    }
    this.loading = (async () => {
      const first = store.get().hierarchy === null;
      if (first) store.set({ phase: "loading" });
      try {
        const tree = await this.api.tree();
        const hierarchy = buildHierarchy(tree.root);
        // Folders deleted by the re-index drop out of the view.
        const wanted = () => (keepView && store.get().hierarchy ? resolveView(hierarchy, store.get().view) : initialView(hierarchy));
        // The user may open or close folders while we wait: refetch until the graph matches
        // what is open now, at most a few times; past that, their view is fetched below.
        let view = wanted();
        let graph = await this.api.graph(graphQuery(view));
        for (let i = 0; i < 3; i++) {
          const now = wanted();
          if (sameView(now, view)) break;
          view = now;
          graph = await this.api.graph(graphQuery(view));
        }
        // Still behind the user: keep the view they made and fetch it, never put back an older one.
        const latest = wanted();
        const behind = !sameView(latest, view);
        // Re-read the selection now: one made during the refresh wins.
        const current = store.get().selected;
        // An "otras N" card (extmore:) has no node of its own: the drawer lists it from the plan.
        const selected =
          current && (hierarchy.nodes.has(current) || current.startsWith("ext:") || current.startsWith("extmore:")) ? current : null;
        this.graphSeq += 1; // drops a fetch still in flight: its tree is the old one
        this.detailCache.clear();
        const shown = behind ? latest : view;
        store.set({
          hierarchy,
          view: shown,
          expanded: openDirs(hierarchy, shown),
          graph,
          graphView: view,
          graphLoading: false,
          phase: "ready",
          selected,
          ...(keepView ? {} : { todoCap: null }),
        });
        if (behind) void this.fetchGraph(latest);
        if (selected?.startsWith("extmore:")) store.set({ detail: { id: selected, data: null, error: null, loading: false } });
        else if (selected) void this.loadDetail(selected);
        else store.set({ detail: null });
        void this.loadIssues();
      } catch (err) {
        if (err instanceof ApiError && err.indexing) {
          store.set({ phase: first ? "indexing" : store.get().phase });
        } else if (first) {
          store.set({ phase: "error", bootError: err instanceof Error ? err.message : String(err) });
        } else {
          toast.error({ title: "No se pudo actualizar el plano", description: err instanceof Error ? err.message : String(err) });
        }
      } finally {
        this.loading = null;
      }
    })();
    return this.loading;
  }

  async loadIssues() {
    // Each load is tagged; an older one that answers after a newer one is dropped.
    const seq = ++this.issuesSeq;
    const [issues, findings] = await Promise.allSettled([this.api.issues(500), this.api.findings(2000)]);
    if (seq !== this.issuesSeq) return;
    // Issues are secondary: on failure the drawer keeps what it has. Findings mark cards
    // and edges, so stale ones (from before a re-index) are cleared instead.
    if (issues.status === "fulfilled") store.set({ issues: issues.value });
    store.set({ findings: findings.status === "fulfilled" ? findings.value : null });
  }

  /**
   * Shows another view. The old graph stays on screen until the new one arrives. `anchor`
   * is the node the plan keeps in place on screen when it draws that view.
   */
  async setView(next: ViewSpec, anchor?: string) {
    const s = store.get();
    if (!s.hierarchy) return;
    const view = resolveView(s.hierarchy, next);
    if (sameView(view, s.view)) return;
    this.pendingAnchor = anchor ? { id: anchor, view: viewKey(view) } : null;
    store.set({ view, expanded: openDirs(s.hierarchy, view) });
    await this.fetchGraph(view);
  }

  /**
   * "Nivel 1 · 2 · 3": everything above the level opens, nothing else. "Todo": every folder,
   * or as much as fits (see todoView) without closing any open one.
   */
  setLevel(level: Level) {
    const s = store.get();
    const h = s.hierarchy;
    if (!h) return;
    if (level === "all") {
      const view = todoView(h, s.view, this.todoMax);
      store.set({ todoCap: view.level === "all" ? null : view.level });
      void this.setView(view);
    } else {
      store.set({ todoCap: null });
      void this.setView(atLevel(level));
    }
  }

  /** Opens a folder on the plan (and the folders above it). Other open folders stay open. */
  openFolder(id: string) {
    const h = store.get().hierarchy;
    if (h) return this.setView(openFolder(h, store.get().view, id), id);
  }

  closeFolder(id: string) {
    const h = store.get().hierarchy;
    if (h) return this.setView(closeFolder(h, store.get().view, id), id);
  }

  toggleFolder(id: string) {
    const h = store.get().hierarchy;
    if (!h) return;
    return isOpen(h, store.get().view, id) ? this.closeFolder(id) : this.openFolder(id);
  }

  /**
   * Fetches the graph for a view already in the store. If it fails, the view goes back to
   * the one whose graph is on screen, so the plan and the explorer agree and a retry fetches again.
   */
  private async fetchGraph(view: ViewSpec) {
    const seq = ++this.graphSeq;
    store.set({ graphLoading: true });
    try {
      const graph = await this.api.graph(graphQuery(view));
      if (seq !== this.graphSeq) return;
      store.set({ graph, graphView: view, graphLoading: false });
    } catch (err) {
      if (seq !== this.graphSeq) return;
      const s = store.get();
      const back = s.graphView ?? s.view;
      store.set({ view: back, expanded: s.hierarchy ? openDirs(s.hierarchy, back) : [], graphLoading: false });
      this.pendingFly = null;
      this.pendingAnchor = null;
      toast.error({ title: "No se pudo abrir la carpeta", description: err instanceof Error ? err.message : String(err) });
    }
  }

  /**
   * The node to fly to once the layout of a view is drawn, if a fly was waiting for it.
   * The plan calls this whenever it draws a new layout.
   */
  takePendingFly(view: ViewSpec | null): string | null {
    this.drawnView = view ? viewKey(view) : null;
    const p = this.pendingFly;
    if (!p || view === null || p.view !== viewKey(view)) return null;
    this.pendingFly = null;
    return p.id;
  }

  /** The node to keep in place on screen once the layout of a view is drawn, if one was asked for. */
  takePendingAnchor(view: ViewSpec | null): string | null {
    const p = this.pendingAnchor;
    if (!p || view === null || p.view !== viewKey(view)) return null;
    this.pendingAnchor = null;
    return p.id;
  }

  /**
   * Selects a node from anywhere (plan, tree, palette, detail lists, issues). If it isn't on
   * the plan, the folders above it open so it shows as a card; a fly then waits until that
   * plan is drawn.
   */
  select(rawId: string | null, { fly = false, reveal = true } = {}) {
    const id = rawId ? canonicalId(rawId) : null;
    const s = store.get();
    if (id === s.selected && !fly) return;
    this.pendingFly = null;
    store.set({ selected: id });
    if (!id) {
      store.set({ detail: null });
      return;
    }
    if (id.startsWith("extmore:")) {
      // The "otras N" package card: the drawer lists them from the plan, nothing to fetch.
      store.set({ detail: { id, data: null, error: null, loading: false } });
      return;
    }
    let target = s.view;
    if (s.hierarchy && s.hierarchy.nodes.has(id) && reveal) {
      target = resolveView(s.hierarchy, revealView(s.hierarchy, s.view, id));
      if (!sameView(target, s.view)) {
        if (fly) this.pendingFly = { id, view: viewKey(target) };
        void this.setView(target);
      }
    }
    // Only nodes of the tree have a place on the plan (an external package has a card only with libraries on).
    if (fly && !this.pendingFly && (s.hierarchy?.nodes.has(id) || (id.startsWith("ext:") && s.showExternals))) {
      const now = store.get();
      if (now.graphView && sameView(now.graphView, target) && !now.graphLoading) this.camera({ type: "fly", id });
      else this.pendingFly = { id, view: viewKey(target) };
    }
    void this.loadDetail(id);
  }

  /**
   * Selects and shows a node named by a link (`?focus=`): like a pick in the palette, but it
   * may come before the plan is drawn (right after the first load), so the fly waits for the
   * drawing of the view instead of going to a plan that isn't on screen yet. A library turns
   * the libraries on (only then does it have a card).
   */
  focus(id: string) {
    const ext = id.startsWith("ext:");
    const redraw = ext && !store.get().showExternals;
    if (ext) store.set({ showExternals: true });
    this.select(id, { fly: true });
    const s = store.get();
    if (s.selected !== id || this.pendingFly) return;
    if (redraw || this.drawnView !== viewKey(s.view)) this.pendingFly = { id, view: viewKey(s.view) };
  }

  /** Double-click or Enter on a card or box: a closed folder opens, an open one closes, a file is selected. */
  activate(id: string) {
    const s = store.get();
    if (!s.hierarchy) return;
    if (s.hierarchy.nodes.has(id) && isOpen(s.hierarchy, s.view, id) && id !== s.hierarchy.root) {
      void this.closeFolder(id);
      this.select(id, { reveal: false });
    } else if (canOpenFolder(s.hierarchy, id)) {
      void this.openFolder(id);
      this.select(id, { reveal: false });
    } else {
      this.select(id, { fly: true });
    }
  }

  hover(id: string | null) {
    store.set({ hovered: id });
  }

  setShowAllEdges(showAllEdges: boolean) {
    store.set({ showAllEdges });
  }

  setShowIsolated(showIsolated: boolean) {
    store.set({ showIsolated });
  }

  setShowExternals(showExternals: boolean) {
    store.set({ showExternals });
  }

  async loadDetail(id: string) {
    const s = store.get();
    const key = `${s.graph?.generation ?? 0}|${id}`;
    // Bump first: a cached answer must also invalidate any fetch still in flight.
    const seq = ++this.detailSeq;
    const current = () => seq === this.detailSeq && store.get().selected === id;
    const cached = this.detailCache.get(key);
    if (cached) {
      if (current()) store.set({ detail: { id, data: cached as never, error: null, loading: false } });
      return;
    }
    store.set({ detail: { id, data: s.detail?.id === id ? s.detail.data : null, error: null, loading: true } });
    try {
      const data = await this.api.node(id);
      this.detailCache.set(key, data);
      if (current()) store.set({ detail: { id, data, error: null, loading: false } });
    } catch (err) {
      if (!current()) return;
      store.set({ detail: { id, data: null, error: err instanceof Error ? err.message : String(err), loading: false } });
    }
  }

  async reindex() {
    try {
      await this.api.reindex();
    } catch {
      toast.error({ title: "No se pudo pedir el reindexado" });
    }
  }

  fitAll() {
    this.camera({ type: "fit" });
  }

  zoom(factor: number) {
    this.camera({ type: "zoom", factor });
  }
}

export const controller = new Controller();
