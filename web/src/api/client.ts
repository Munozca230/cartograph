import type {
  EntriesResponse,
  ErrorBody,
  FileFlowsResponse,
  FlowResponse,
  FunctionSearchResponse,
  SymbolResponse,
  GraphQuery,
  GraphResponse,
  FindingsResponse,
  IssuesResponse,
  Meta,
  NodeDetail,
  PackagesResponse,
  Progress,
  SearchResponse,
  TreeResponse,
} from "./types";

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly progress: Progress | null = null,
  ) {
    super(message);
    this.name = "ApiError";
  }

  get indexing(): boolean {
    return this.status === 503 && this.code === "indexing";
  }
}

export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

/** Resolves `api/` against the document, so the app works from `/` and from the Vite dev server. */
export function defaultApiBase(): string {
  if (typeof document !== "undefined") return new URL("api/", document.baseURI).toString();
  return "/api/";
}

export class ApiClient {
  constructor(
    readonly base: string = defaultApiBase(),
    private readonly fetchImpl: FetchLike = (input, init) => fetch(input, init),
  ) {}

  /** Builds an API URL. An array value becomes a repeated parameter (`?k=a&k=b`). */
  url(path: string, params: Record<string, Param> = {}): string {
    const base = this.base.endsWith("/") ? this.base : `${this.base}/`;
    const parts: string[] = [];
    for (const [k, v] of Object.entries(params)) {
      const values = Array.isArray(v) ? v : [v];
      for (const one of values) {
        if (one === undefined || one === "") continue;
        parts.push(`${encodeURIComponent(k)}=${encodeURIComponent(String(one))}`);
      }
    }
    return `${base}${path}${parts.length ? `?${parts.join("&")}` : ""}`;
  }

  private async read<T>(res: Response): Promise<T> {
    let body: unknown;
    try {
      body = await res.json();
    } catch {
      body = null;
    }
    if (!res.ok) {
      const err = (body as ErrorBody | null)?.error;
      throw new ApiError(
        res.status,
        err?.code ?? "internal",
        err?.message ?? `HTTP ${res.status}`,
        (body as ErrorBody | null)?.progress ?? null,
      );
    }
    return body as T;
  }

  private async get<T>(path: string, params?: Record<string, Param>, signal?: AbortSignal): Promise<T> {
    return this.read<T>(await this.fetchImpl(this.url(path, params), { signal, headers: { accept: "application/json" } }));
  }

  meta(signal?: AbortSignal) {
    return this.get<Meta>("meta", undefined, signal);
  }
  tree(signal?: AbortSignal) {
    return this.get<TreeResponse>("tree", undefined, signal);
  }
  /**
   * The level travels as `depth` and the hand-made changes as one `expanded=` or
   * `collapsed=` parameter per id, each URL-encoded, so ids with commas, `(app)` or `[id]`
   * arrive intact. The URL grows with what the user opened or closed, not with the repo.
   */
  graph(q: GraphQuery, signal?: AbortSignal) {
    return this.get<GraphResponse>(
      "graph",
      { depth: q.depth === 1 ? undefined : String(q.depth), expanded: [...q.expanded].sort(), collapsed: [...q.collapsed].sort() },
      signal,
    );
  }
  node(id: string, signal?: AbortSignal) {
    return this.get<NodeDetail>("node", { id }, signal);
  }
  packages(signal?: AbortSignal) {
    return this.get<PackagesResponse>("packages", undefined, signal);
  }
  issues(limit = 500, signal?: AbortSignal) {
    return this.get<IssuesResponse>("issues", { limit }, signal);
  }
  findings(limit = 2000, signal?: AbortSignal) {
    return this.get<FindingsResponse>("findings", { limit }, signal);
  }
  search(q: string, limit = 50, signal?: AbortSignal) {
    return this.get<SearchResponse>("search", { q, limit }, signal);
  }
  /** Flow entries (main, routes, commands, Godot), test and fixture trees left out. */
  entries(signal?: AbortSignal) {
    return this.get<EntriesResponse>("entries", undefined, signal);
  }
  /** The bounded call flow from a function; `expand` opens functions whose callees were cut. */
  flow(from: string, expand: readonly string[] = [], max?: number, signal?: AbortSignal) {
    return this.get<FlowResponse>("flow", { from, max: max === undefined ? undefined : String(max), expand: [...expand].sort() }, signal);
  }
  symbol(id: string, signal?: AbortSignal) {
    return this.get<SymbolResponse>("symbol", { id }, signal);
  }
  fileFlows(id: string, signal?: AbortSignal) {
    return this.get<FileFlowsResponse>("file-flows", { id }, signal);
  }
  /** Functions and methods by name (`/api/search?kinds=sym`). */
  searchFunctions(q: string, limit = 30, signal?: AbortSignal) {
    return this.get<FunctionSearchResponse>("search", { q, limit, kinds: "sym" }, signal);
  }
  async reindex(): Promise<void> {
    await this.read<unknown>(await this.fetchImpl(this.url("reindex"), { method: "POST", headers: { accept: "application/json" } }));
  }
}

type Param = string | number | undefined | readonly string[];
