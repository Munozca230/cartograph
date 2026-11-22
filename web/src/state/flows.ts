// The "Flujos" view's actions: entries, the flow on screen, its selection and the jumps
// between Flujos and Plano. Its selection is its own (`flowSelected`): the plan's controller
// drops any selection the tree doesn't know on every re-index, and functions aren't in the tree.
// A new generation (seen as a new graph in the store) fetches again what is on screen, with
// the same expansions; the view keeps its camera.
import { ApiClient } from "../api/client";
import type { FlowEntry } from "../api/types";
import { controller } from "./controller";
import { store, type ViewMode } from "./store";

export type FlowsApi = Pick<ApiClient, "entries" | "flow" | "symbol" | "fileFlows" | "searchFunctions">;

/**
 * Functions a flow shows before "+N" (the server allows up to 300): enough for the first
 * levels of a command, few enough to read at the default zoom. A click opens what was cut.
 */
export const FLOW_NODES = 30;

export type FlowCameraRequest = { type: "fit" } | { type: "zoom"; factor: number };

const message = (err: unknown) => (err instanceof Error ? err.message : String(err));

export class FlowsController {
  constructor(readonly api: FlowsApi = new ApiClient()) {}

  private entriesSeq = 0;
  private flowSeq = 0;
  private cardSeq = 0;
  private fileSeq = 0;
  private generation: number | null = null;
  private unwatch: (() => void) | null = null;
  private cameraListeners = new Set<(r: FlowCameraRequest) => void>();
  /** The function whose callees were just opened: the view keeps it in place once redrawn. */
  private pendingAnchor: string | null = null;

  /** Follow re-indexes: a new graph generation re-fetches what Flujos shows. */
  watch() {
    if (this.unwatch) return;
    this.generation = store.get().graph?.generation ?? null;
    this.unwatch = store.subscribe(() => {
      const g = store.get().graph?.generation ?? null;
      if (g === this.generation) return;
      const first = this.generation === null;
      this.generation = g;
      if (!first) void this.refresh();
    });
  }

  onCamera(l: (r: FlowCameraRequest) => void): () => void {
    this.cameraListeners.add(l);
    return () => {
      this.cameraListeners.delete(l);
    };
  }

  fit() {
    for (const l of this.cameraListeners) l({ type: "fit" });
  }

  zoom(factor: number) {
    for (const l of this.cameraListeners) l({ type: "zoom", factor });
  }

  takePendingAnchor(): string | null {
    const a = this.pendingAnchor;
    this.pendingAnchor = null;
    return a;
  }

  /** Plano or Flujos. The first visit to Flujos loads the entries and opens the first one. */
  setMode(mode: ViewMode) {
    store.set({ mode });
    if (mode === "flows" && store.get().flowEntries === null) void this.loadEntries({ openFirst: true });
  }

  async loadEntries({ openFirst = false } = {}) {
    const seq = ++this.entriesSeq;
    try {
      const entries = await this.api.entries();
      if (seq !== this.entriesSeq) return;
      store.set({ flowEntries: entries, flowEntriesError: null });
      const first = entries.entries[0];
      if (openFirst && first && store.get().flowFrom === null) void this.open(first.root, first);
    } catch (err) {
      if (seq !== this.entriesSeq) return;
      store.set({ flowEntriesError: message(err) });
    }
  }

  /** Shows the flow that starts at `id` (a function, or `file:` for module-level code), selected. */
  async open(id: string, entry: FlowEntry | null = null) {
    // Another start is fitted: a card expanded in the last flow has nothing to anchor.
    this.pendingAnchor = null;
    store.set({ mode: "flows", flowFrom: { id, entry }, flowExpand: [], flowSelected: id, flowError: null });
    void this.loadCard(id);
    await this.loadFlow();
  }

  async loadFlow() {
    const s = store.get();
    if (!s.flowFrom) return;
    const seq = ++this.flowSeq;
    store.set({ flowLoading: true });
    try {
      const flow = await this.api.flow(s.flowFrom.id, s.flowExpand, FLOW_NODES);
      if (seq !== this.flowSeq) return;
      store.set({ flow, flowLoading: false, flowError: null });
      // The selection left the flow (a re-index removed it): nothing selected.
      const sel = store.get().flowSelected;
      if (sel && flow.indexed && !flow.nodes.some((n) => n.id === sel)) this.select(null);
    } catch (err) {
      if (seq !== this.flowSeq) return;
      store.set({ flowLoading: false, flowError: message(err) });
    }
  }

  /** Selects a card (null clears). A card whose callees were cut also opens them. */
  select(id: string | null, { expand = true } = {}) {
    store.set({ flowSelected: id });
    if (!id) {
      ++this.cardSeq;
      store.set({ flowCard: null });
      return;
    }
    void this.loadCard(id);
    const n = store.get().flow?.nodes.find((x) => x.id === id);
    if (expand && n && n.more > 0 && !n.open) this.expand(id);
  }

  /** Opens a function's callees past the flow's limits; it stays where it is on screen. */
  expand(id: string) {
    const s = store.get();
    if (s.flowExpand.includes(id)) return;
    this.pendingAnchor = id;
    store.set({ flowExpand: [...s.flowExpand, id] });
    void this.loadFlow();
  }

  async loadCard(id: string) {
    const seq = ++this.cardSeq;
    const prev = store.get().flowCard;
    store.set({ flowCard: { id, data: prev?.id === id ? prev.data : null, error: null, loading: true } });
    try {
      const data = await this.api.symbol(id);
      if (seq !== this.cardSeq) return;
      store.set({ flowCard: { id, data, error: null, loading: false } });
    } catch (err) {
      if (seq !== this.cardSeq) return;
      store.set({ flowCard: { id, data: null, error: message(err), loading: false } });
    }
  }

  /** Double click on a card: its file on the plan, selected and on screen (the plan keeps its zoom). */
  jumpToPlan(file: string) {
    store.set({ mode: "plan" });
    controller.select(`file:${file}`, { fly: true });
  }

  /** "Ver flujos" of a file selected on the plan. */
  async loadFileFlows(fileId: string) {
    const seq = ++this.fileSeq;
    store.set({ fileFlows: { id: fileId, data: null, error: null, loading: true } });
    try {
      const data = await this.api.fileFlows(fileId);
      if (seq !== this.fileSeq) return;
      store.set({ fileFlows: { id: fileId, data, error: null, loading: false } });
    } catch (err) {
      if (seq !== this.fileSeq) return;
      store.set({ fileFlows: { id: fileId, data: null, error: message(err), loading: false } });
    }
  }

  closeFileFlows() {
    ++this.fileSeq;
    store.set({ fileFlows: null });
  }

  /** A new generation: fetch again whatever Flujos has loaded, with the same expansions. */
  async refresh() {
    const s = store.get();
    const jobs: Promise<unknown>[] = [];
    if (s.flowEntries !== null || s.flowEntriesError !== null) jobs.push(this.loadEntries());
    if (s.flowFrom) jobs.push(this.loadFlow());
    if (s.flowSelected) jobs.push(this.loadCard(s.flowSelected));
    if (s.fileFlows) jobs.push(this.loadFileFlows(s.fileFlows.id));
    await Promise.allSettled(jobs);
  }
}

export const flows = new FlowsController();
