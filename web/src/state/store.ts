// A tiny external store for React (useSyncExternalStore), with selector subscriptions.
import { useSyncExternalStore } from "react";
import type { ConnectionState } from "../api/events";
import type {
  EntriesResponse,
  FileFlowsResponse,
  FindingsResponse,
  FlowEntry,
  FlowResponse,
  GraphResponse,
  IssuesResponse,
  Meta,
  NodeDetail,
  Progress,
  SymbolResponse,
} from "../api/types";
import type { Hierarchy } from "../model/hierarchy";
import { TOP_LEVEL, type ViewSpec } from "../model/view";

export type Phase = "connecting" | "indexing" | "loading" | "ready" | "error";

export interface DetailState {
  id: string;
  data: NodeDetail | null;
  error: string | null;
  loading: boolean;
}

export interface AppState {
  phase: Phase;
  bootError: string | null;
  meta: Meta | null;
  connection: ConnectionState;
  progress: Progress | null;
  reindexing: boolean;
  lastIndexedAt: number | null;
  hierarchy: Hierarchy | null;
  graph: GraphResponse | null;
  /** The view `graph` was fetched for (lags `view` while a folder opens). */
  graphView: ViewSpec | null;
  graphLoading: boolean;
  /** What the plan opens: a level plus folders opened or closed by hand. */
  view: ViewSpec;
  /**
   * The level "Todo" stopped at on a repo too big to open whole (the toolbar shows
   * "Todo · hasta el nivel N"), or null when the level was picked by hand.
   */
  todoCap: number | null;
  /** The folders `view` opens, in tree order (the explorer's open rows). */
  expanded: string[];
  /** Show the light edges a dense view hides. */
  showAllEdges: boolean;
  /** Draw nodes without any dependency in this view as cards too (faded). */
  showIsolated: boolean;
  /** Draw external packages as cards in the bottom layer. */
  showExternals: boolean;
  explorerOpen: boolean;
  selected: string | null;
  hovered: string | null;
  /** Cards that changed in the last re-index, for a short highlight. */
  pulse: { ids: string[]; at: number } | null;
  detail: DetailState | null;
  issues: IssuesResponse | null;
  /** Findings of /api/findings (cycles, layer violations, orphans…), loaded with the issues. */
  findings: FindingsResponse | null;
  paletteOpen: boolean;
  issuesOpen: boolean;
  /** Which view fills the window: the plan (Plano) or the call flows (Flujos). Both stay mounted. */
  mode: ViewMode;
  /** Flow entries of /api/entries (null until the Flujos view first asks). */
  flowEntries: EntriesResponse | null;
  flowEntriesError: string | null;
  /** Where the flow on screen starts, and the entry it was opened from (for its title). */
  flowFrom: { id: string; entry: FlowEntry | null } | null;
  /** Functions opened past the flow's limits. */
  flowExpand: string[];
  flow: FlowResponse | null;
  flowLoading: boolean;
  flowError: string | null;
  /** The function selected in Flujos (its own selection: the plan's is for files and folders). */
  flowSelected: string | null;
  flowCard: Loaded<SymbolResponse> | null;
  /** "Ver flujos" of the file selected on the plan. */
  fileFlows: Loaded<FileFlowsResponse> | null;
}

export type ViewMode = "plan" | "flows";

export interface Loaded<T> {
  id: string;
  data: T | null;
  error: string | null;
  loading: boolean;
}

export const initialState: AppState = {
  phase: "connecting",
  bootError: null,
  meta: null,
  connection: "connecting",
  progress: null,
  reindexing: false,
  lastIndexedAt: null,
  hierarchy: null,
  graph: null,
  graphView: null,
  graphLoading: false,
  view: TOP_LEVEL,
  todoCap: null,
  expanded: [],
  showAllEdges: false,
  showIsolated: false,
  showExternals: false,
  explorerOpen: false,
  selected: null,
  hovered: null,
  pulse: null,
  detail: null,
  issues: null,
  findings: null,
  paletteOpen: false,
  issuesOpen: false,
  mode: "plan",
  flowEntries: null,
  flowEntriesError: null,
  flowFrom: null,
  flowExpand: [],
  flow: null,
  flowLoading: false,
  flowError: null,
  flowSelected: null,
  flowCard: null,
  fileFlows: null,
};

type Listener = () => void;

export class Store {
  private state: AppState = initialState;
  private listeners = new Set<Listener>();

  get = (): AppState => this.state;

  set = (patch: Partial<AppState> | ((s: AppState) => Partial<AppState>)) => {
    const next = typeof patch === "function" ? patch(this.state) : patch;
    let changed = false;
    for (const k of Object.keys(next) as (keyof AppState)[]) {
      if (this.state[k] !== next[k]) {
        changed = true;
        break;
      }
    }
    if (!changed) return;
    this.state = { ...this.state, ...next };
    for (const l of this.listeners) l();
  };

  subscribe = (l: Listener) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };
}

export const store = new Store();

export function useAppState<T>(selector: (s: AppState) => T): T {
  return useSyncExternalStore(store.subscribe, () => selector(store.get()), () => selector(initialState));
}
