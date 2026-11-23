// Types for the cartograph HTTP API (docs/api-f2.md, including "Aclaraciones (backend)").
// The mock in ../../mock produces exactly these shapes.

export type Confidence = "exact" | "resolved" | "heuristic";

export interface ConfidenceCounts {
  exact: number;
  resolved: number;
  heuristic: number;
}

export interface Progress {
  done: number;
  total: number;
}

export interface Counts {
  files: number;
  dirs: number;
  packages: number;
  loc: number;
  imports: number;
  resolved: number;
  external: number;
  unresolved: number;
  skipped: number;
  diagnostics: number;
  /** Findings that are problems (error or warning). Absent from older servers. */
  findings?: number;
  /** Informational findings (bottlenecks, Stable Dependencies, deep chains…). */
  findings_info?: number;
}

export interface Meta {
  generation: number;
  status: "indexing" | "ready" | "error";
  progress: Progress | null;
  /** True while a re-index runs after the first one finished. */
  reindexing?: boolean;
  error?: string;
  repo: { name: string; root: string };
  indexed_at: string | null;
  index_ms: number | null;
  counts: Counts | null;
  languages: { lang: string; files: number; loc: number }[];
  watching: boolean;
  /** A workspace index: its members (absent for a single repo). */
  workspace?: { members: WorkspaceMember[] };
}

/** One repository of a workspace: a top-level folder of the map named after it. */
export interface WorkspaceMember {
  name: string;
  /** Its folder as the workspace file writes it. */
  path: string;
  root: string;
  /** State of its last git pass (null before the first one). */
  git: "ok" | "no_git" | "no_commits" | "error" | null;
  branch: string | null;
  head: string | null;
  detached: boolean;
}

export type TreeKind = "repo" | "dir" | "file";

export interface TreeNode {
  id: string;
  kind: TreeKind;
  name: string;
  path: string;
  loc: number;
  files?: number;
  fan_in: number;
  fan_out: number;
  unresolved: number;
  langs?: Record<string, number>;
  lang?: string;
  children?: TreeNode[];
  truncated?: boolean;
}

export interface TreeResponse {
  generation: number;
  root: TreeNode;
}

export interface GraphNode {
  id: string;
  kind: "dir" | "file";
  name: string;
  path: string;
  parent: string;
  loc: number;
  files: number;
  langs: Record<string, number>;
  lang?: string;
  fan_in: number;
  fan_out: number;
  unresolved: number;
  expandable: boolean;
}

export interface GraphEdge {
  src: string;
  dst: string;
  weight: number;
  confidence: ConfidenceCounts;
}

export interface GraphExternal {
  src: string;
  ext: string;
  weight: number;
}

/** What /api/graph opens: a depth (1 = nothing, `all` = every folder) plus overrides. */
export interface GraphQuery {
  depth: number | "all";
  expanded: readonly string[];
  collapsed: readonly string[];
}

export interface GraphResponse {
  generation: number;
  nodes: GraphNode[];
  edges: GraphEdge[];
  externals: GraphExternal[];
  ignored: string[];
  hidden_namespace_imports?: number;
}

/** Fields of a node as in /api/tree, without children. */
export type FlatNode = Omit<TreeNode, "children">;

export interface FileDetail {
  generation: number;
  node: FlatNode & { kind: "file" };
  imports: { target: string; specifier: string; line: number; confidence: Confidence }[];
  importers: { source: string; line: number; confidence: Confidence }[];
  externals: { ext: string; specifier: string; line: number }[];
  unresolved: { line: number; specifier: string; reason: string; detail: string }[];
  package: string | null;
}

export interface DirDetail {
  generation: number;
  node: FlatNode & { kind: "dir" | "repo" };
  children: { id: string; kind: "dir" | "file"; name: string; loc: number; files: number }[];
  deps_out: { id: string; weight: number }[];
  deps_in: { id: string; weight: number }[];
  externals: { ext: string; weight: number }[];
}

export interface ExtDetail {
  generation: number;
  node: { id: string; kind: "ext"; name: string; [key: string]: unknown };
  importers: { source: string; specifier: string; line: number }[];
}

export interface PackageInfo {
  id: string;
  name: string;
  kind: string;
  dir: string;
  manifest: string;
  notes: string | null;
}

export interface PackageDetail {
  generation: number;
  node: PackageInfo;
}

export type NodeDetail = FileDetail | DirDetail | ExtDetail | PackageDetail;

export interface PackagesResponse {
  generation: number;
  packages: PackageInfo[];
}

export interface UnresolvedIssue {
  file: string;
  line: number;
  specifier: string;
  reason: string;
  detail: string;
}

export interface IssuesResponse {
  generation: number;
  unresolved: UnresolvedIssue[];
  by_reason: Record<string, number>;
  diagnostics: { path: string; detail: string }[];
  skipped: { path: string; reason: string; detail: string }[];
  truncated: boolean;
  total: { unresolved: number; skipped: number };
}

export type Severity = "error" | "warning" | "info";

/** A file → file dependency that proves a finding. */
export interface FindingEdge {
  src: string;
  dst: string;
  /** Line of the import in `src`. */
  line?: number;
  /** An import of the target's folder (Go package, Java wildcard…). */
  via_dir?: boolean;
  /** Only types come through it (TypeScript `import type`…): no load at run time. */
  type_only?: boolean;
}

/** One finding of /api/findings (docs/api-f2.md). */
export interface Finding {
  /** Stable: kind plus its main location (`cycle:file:src/a.ts`). */
  id: string;
  kind: "layer_violation" | "fan_out_limit" | "cycle" | "folder_cycle" | "god_module" | "hub" | "sdp" | "orphan" | "deep_chain" | "hidden_coupling";
  severity: Severity;
  /** Breaks a rule of cartograph.toml. */
  violation: boolean;
  /** Short title, in English (the UI words its own). */
  title: string;
  /** Location ids (`file:`, `dir:`, `repo:`). */
  nodes: string[];
  nodes_more?: number;
  /** At most 48 edges that prove it; when `edges_more` > 0 they are only a sample. */
  edges: FindingEdge[];
  edges_more?: number;
  evidence: string[];
  evidence_more?: number;
  metrics: Record<string, number>;
}

export interface FindingsResponse {
  generation: number;
  findings: Finding[];
  summary: { total: number; violations: number; by_severity: Record<string, number>; by_kind: Record<string, number> };
  config: { origin: string | null; error: string | null };
  total: number;
  truncated: boolean;
}

export interface SearchResult {
  id: string;
  kind: "file" | "dir" | "ext";
  path: string;
  name: string;
  score: number;
}

export interface SearchResponse {
  generation: number;
  results: SearchResult[];
}

// ---------------------------------------------------------------- flows (docs/api-f2.md, items 20-25)

export type EntryKind = "cli" | "http" | "main" | "ipc" | "godot_main_scene" | "godot_autoload";

export interface FlowEntry {
  kind: EntryKind;
  name: string;
  file: string;
  line: number;
  target: string | null;
  /** Where its flow starts: a `sym:` id, or `file:<path>` for module-level code. */
  root: string;
}

export interface EntriesResponse {
  generation: number;
  /** False on an index written before the symbol layer existed: re-index. */
  indexed: boolean;
  total: number;
  hidden: number;
  truncated: boolean;
  entries: FlowEntry[];
}

export type FindingMark = "error" | "warning";

export interface FlowNodeData {
  id: string;
  name: string;
  kind: string;
  file: string;
  line: number;
  end_line: number;
  callers: number;
  depth: number;
  callees: number;
  more: number;
  /** Opened with `expand`: a `more` left is the flow's cap, not a cut a click can open. */
  open?: boolean;
  finding?: FindingMark;
  lane: string;
}

export interface FlowEdgeData {
  src: string;
  dst: string;
  confidence: Confidence;
  line: number;
  calls: number;
}

export interface FlowResponse {
  generation: number;
  indexed: boolean;
  root: string;
  depth: number;
  max_nodes: number;
  nodes: FlowNodeData[];
  edges: FlowEdgeData[];
  lanes: { id: string; label: string }[];
  truncated: boolean;
}

export interface CallerSite {
  file: string;
  line: number;
  caller: string | null;
  caller_name: string | null;
  confidence: Confidence;
}

export interface SymbolResponse {
  generation: number;
  indexed: boolean;
  /** `end_line` of a `file` symbol is its count of non-blank lines (lines of code). */
  symbol: { id: string; name: string; kind: string; file: string; line: number; end_line: number; exported: boolean; lane: string };
  callers: { production: CallerSite[]; production_total: number; production_files: number; test_sites: number; test_files: number };
  callees: { id: string; name: string; file: string; line: number; confidence: Confidence; calls: number; lane: string }[];
  callees_total: number;
  callee_lanes: number;
  git: {
    commits: number;
    recent: number;
    window_days: number | null;
    last_time: number;
    authors: { name: string; commits: number }[];
    authors_total: number;
  } | null;
  finding: FindingMark | null;
  findings: string[];
  entries: FlowEntry[];
}

export interface FileFlowsResponse {
  generation: number;
  indexed: boolean;
  file: string;
  functions: { id: string; name: string; kind: string; line: number; callers: number; callees: number }[];
  functions_total: number;
  entries: FlowEntry[];
  partial: boolean;
}

export interface FunctionResult {
  id: string;
  kind: "sym";
  path: string;
  name: string;
  line: number;
  symbol_kind: string;
  callers: number;
}

export interface FunctionSearchResponse {
  generation: number;
  results: FunctionResult[];
}

export interface ErrorBody {
  generation?: number;
  error: { code: "not_found" | "bad_request" | "indexing" | "internal"; message: string };
  progress?: Progress | null;
}

// Server-sent events.
export interface HelloEvent {
  generation: number;
  status: Meta["status"];
}
export interface IndexStartedEvent {
  reason: "startup" | "watch" | "manual";
  changed: string[];
  changed_total?: number;
}
export type IndexProgressEvent = Progress;
export interface IndexFinishedEvent {
  generation: number;
  ms: number;
  changed: string[];
  changed_total?: number;
  counts: Counts;
}
export interface IndexErrorEvent {
  message: string;
}

export type ServerEvent =
  | { type: "hello"; data: HelloEvent }
  | { type: "index-started"; data: IndexStartedEvent }
  | { type: "index-progress"; data: IndexProgressEvent }
  | { type: "index-finished"; data: IndexFinishedEvent }
  | { type: "index-error"; data: IndexErrorEvent };

export const SERVER_EVENT_TYPES = [
  "hello",
  "index-started",
  "index-progress",
  "index-finished",
  "index-error",
] as const;
