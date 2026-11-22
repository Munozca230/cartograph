# cartograph for agents: the MCP server

`cartograph mcp [repo]` is a stdio [MCP](https://modelcontextprotocol.io) server that
answers structural questions about a repository from its cartograph index: where things
are, what a file or folder does, who imports or calls what, what an edit can break, which
tests to run, and which architecture rules hold. It wraps `cartograph-query` directly (the
same functions as `cartograph query <tool>`), in the same binary.

It works with any MCP client or coding agent that can start a stdio server: see
[Register the server](#register-the-server) and the
[recommended agent workflow](#recommended-agent-workflow).

## Tools

Every answer is compact text within a token budget. Long lists end in `+N more`; nothing
is dropped silently. `--structured` adds the same data as JSON in `structuredContent`; it
is off by default because some clients show only the structured JSON to the model,
*instead of* the text, which reads worse and costs about twice the tokens.

| Tool | Use it instead of… | Arguments (defaults) | Budget |
|---|---|---|---|
| `overview` | listing directories, reading READMEs | `path` (repo), `depth` 1-4 (1) | 1.5k tokens |
| `explain` | opening a file to learn its role and neighbours | `node` | 1k |
| `deps` | grepping import statements | `node`, `dir` out/in/both (out), `depth` 1-3 (1), `level` file/folder (file) | 1.5k |
| `impact` | guessing what an edit breaks and which tests to run | `nodes` (list) | 1.5k |
| `find` | glob/grep for a name | `query`, `kind` file/dir/ext/sym (all but sym) | 0.8k |
| `path` | tracing why A depends on B by hand | `from`, `to` | 0.8k |
| `symbols` | reading a file for its outline | `node` | 1k |
| `callers` | grepping a function name before changing it | `symbol` | 1k |
| `hierarchy` | grepping `extends` / `implements` / `impl … for` | `symbol` (a type, or a method of one) | 0.8k |
| `entries` | searching for main, routes, commands | `kind`, `within` | 1k |
| `findings` | reviewing structure by eye | `kind` (list), `severity`, `within`, `violations` | 1.5k |
| `check` | — (rules of `cartograph.toml`) | `base` (none): a git revision; then only violations the change introduces fail | 1k |
| `diff` | reading `git diff` to review a branch | `base` (merge-base with the default branch) | 1.5k |
| `hotspots` | `git log` statistics | `within` (whole repo) | 1k |
| `annotate` | — (leaves a note for later sessions) | `node`, `text` (≤ 600 chars; empty deletes) | — |
| `open_map` | asking the user to run `cartograph serve` | `focus` (whole repo), `view` plan/flows (flows for a function or method, else plan), `open_browser` (true) | — |

Ids are stable and every answer uses them: `file:src/a.rs`, `dir:src`, `repo:`,
`sym:src/a.rs#name`, libraries as `external:npm:react` (or `npm:react`). Inputs also
accept plain repo paths (`src/a.rs`, `src/`), absolute paths inside the repo (made
relative by the server), and `path#name` / `Class.method` / a bare name for symbols.

`explain` shows the note `annotate` left on the node. Notes live in
`<data dir>/annotations.json`, keyed by node id, with the content hash of the node when
they were written (for a folder, a hash over every file under it). When the hash changes
the note shows as `note (stale: …)`. `cartograph query explain` shows them too.

The server instructions (sent at initialization) say when to prefer cartograph over grep,
so a client that defers tool schemas behind a tool search still knows when to reach for
the tools. Every tool but `annotate` and `open_map` is marked read-only.

### `open_map`: the visual map

When the user asks to see the architecture, a diagram, a flow or the map, `open_map`
opens the web UI of `cartograph serve` focused on what is being discussed, and returns its
URL with a one-line description of what it shows.

- `focus` takes any id the other tools accept. A folder or file is selected on the plan
  (Plano) with its parent folders open; a function or method opens the call flows
  (Flujos) at it; a class or another type opens the plan on its file; a library opens the
  plan with the libraries shown. `view` overrides the choice (`plan` on a function shows
  its file; `flows` on a file shows its module-level calls; a folder has no flow). An
  unknown id is an error with suggestions, as in the other tools.
- The first call starts the map inside the MCP server, on `127.0.0.1` and a port the OS
  picks, serving the repo and data dir the server already indexes. It has no indexer or
  watcher of its own: each index the server makes is pushed to the open page, which
  refreshes live, and the page's reindex button asks the server for a run. Later calls
  reuse the same map; it stops when the MCP server does (stdin closed). The Host and
  Origin checks of `cartograph serve` apply.
- With `open_browser` (the default) the URL is opened with the platform's opener. When that
  fails the answer says so and still carries the URL to give the user.
- The URL is a deep link: `?view=plan&focus=dir:src/app` or
  `?view=flows&focus=sym:src/auth.ts%23login`. The page applies it once its first plan is
  loaded, and from then on keeps the URL in step with the selection, so it can be shared
  while the session runs.

### Errors are data

| Situation | Answer |
|---|---|
| unknown id | `isError: true`, "did you mean: a, b, c?" from fuzzy matches, or a pointer to `find` |
| ambiguous symbol name | `isError: true` with the candidate ids |
| bad enum value (`dir: "up"`) | `isError: true` with the accepted values |
| wrong type, missing field | `isError: true`, "failed to deserialize parameters: missing field `node`" |
| unknown tool | JSON-RPC error -32602 |
| `diff` / `check(base)`: unknown revision, no git repository | `isError: true` with git's reason |
| no index yet | not an error: "cartograph is indexing … 37 % (1234 of 3300 files read). Call again in a few seconds" (with `--structured`: `status = "indexing"`) |
| index failed | the indexer's error and what to do |
| query panicked or timed out | `isError: true`, "cartograph <tool> failed: …; the server is still running" (code `internal` or `timeout`) |

## Register the server

`cartograph mcp <repo>` speaks MCP over stdio. Register it in your MCP client's
configuration as a stdio server; most clients accept the shapes below (some call the
top-level key `servers` instead of `mcpServers`).

With npx, nothing has to be installed first beyond Node.js 18 or newer: npx downloads the
npm package [`@munozca230/cartograph`](https://www.npmjs.com/package/@munozca230/cartograph),
which runs the prebuilt binary for the platform (stdio passes through it untouched):

```json
{
  "mcpServers": {
    "cartograph": {
      "command": "npx",
      "args": ["-y", "@munozca230/cartograph", "mcp", "."]
    }
  }
}
```

- `-y` skips npx's install prompt, which would otherwise wait on the stdin the protocol
  uses. Pin a version with `@munozca230/cartograph@<version>`.
- On Windows `npx` is a `.cmd` script; a client that starts the command without a shell
  needs `"command": "cmd"` and `"args": ["/c", "npx", "-y", "@munozca230/cartograph", "mcp", "."]`.
- After `npm i -g @munozca230/cartograph`, `"command": "cartograph"` with
  `"args": ["mcp", "."]` skips npx's registry check at every start.

With a binary from a release archive or built from source:

```json
{
  "mcpServers": {
    "cartograph": {
      "command": "/path/to/cartograph",
      "args": ["mcp", "/path/to/repo"]
    }
  }
}
```

- `command` is the `cartograph` binary (an absolute path is safest; on Windows use
  forward slashes or escaped backslashes). If the binary moves, update the path.
- The last argument is the repository. A client that starts servers in the project's
  directory can pass `"."`.
- The index goes to `<repo>/.cartograph/` (add it to `.gitignore`). Pass
  `"--data-dir", "/some/dir"` to keep it outside the repo; nothing is then written inside
  it.
- Other options: `--config FILE` (rules file instead of `<repo>/cartograph.toml`),
  `--no-watch`, `--structured`, `--tool-timeout` (30 s), `--fresh-wait-ms` (8000),
  `--max-file-bytes`, `--no-stats` (see [Usage stats](#usage-stats)).

The server indexes in the background at startup, so it can be registered before the repo
has an index. Without an MCP client, the same answers come from the CLI:
`cartograph query <overview|explain|deps|impact|find|path|symbols|callers|hierarchy|entries|findings|hotspots> …`,
`cartograph check` and `cartograph diff`.

## Recommended agent workflow

Instructions you can give a coding agent (in its rules or prompt file) so it works from
the map instead of from grep:

1. **Orient**: `overview(path)` for a folder you do not know yet (depth 1-4).
2. **Locate**: `find(query)` instead of glob/grep for a name (`kind: "sym"` for
   functions, classes, methods and types, with path:line). `explain(node)` instead of
   opening a file to learn its role, its imports, who imports it and which tests cover it.
3. **Before editing a file**: `impact(nodes: [files])` lists what depends on it
   transitively and the test files to run. Run those tests after the edit.
4. **Before changing a function, method or type**: `callers(symbol)`. Update every caller
   it lists; confirm the `heuristic` ones by reading them.
5. **Dependencies**: `deps(node, dir: "in" | "out")`; `path(from, to)` explains why A
   depends on B (each step with its import line).
6. **Before finishing**: `check` (the rules of `cartograph.toml`; with `base: "main"`
   only what the change introduces fails) and `findings(within: <folder you changed>)` for
   new cycles or violations. `diff` shows what the change does to the architecture.
7. **Leave knowledge behind**: something non-obvious about a file or folder (its purpose,
   an invariant, a trap) goes into `annotate(node, text)` (at most 600 characters).
   `explain` shows it in later sessions and marks it stale when the code changes.
8. **Show the user**: to show the user the architecture or a flow, call `open_map`
   (`focus` on the folder, file or function being discussed).

Reading the answers:

- Lists end in `+N more` when cut: narrow the argument (a subfolder, `within`) instead of
  reading everything.
- `indexing N %`: the first index is being built; call again in a few seconds.
- A first line `(index refresh ... still running ...)`: the answer may miss the latest
  edit; call again if that edit matters.
- Imports through reflection, string-built paths or code generation are not in the graph;
  when an answer looks incomplete, confirm with grep or by reading the code.

## Robustness guarantees

- **stdout is only protocol.** Every log line goes to stderr through `tracing` (no colour
  codes when stderr is not a terminal). The integration tests read stdout line by line and
  fail on anything that is not JSON-RPC. Garbage or truncated input lines are ignored;
  the next valid request is answered.
- **Starts without an index.** The handshake answers at once; the first index runs in the
  background. A request waits up to `--fresh-wait-ms` (8 s) for it, then answers
  "indexing N %". With an index from an earlier session, requests answer from it
  immediately (first line: "index from an earlier session; refreshing") while a new index
  runs.
- **Each request is isolated.** Queries run on blocking threads; a panic becomes a tool
  error (the server stays up), each call has a timeout (`--tool-timeout`, 30 s), and at
  most 4 queries run at once, which bounds memory. A timed-out query keeps its slot until
  it really ends, so runaway queries cannot pile up.
- **Fresh answers.**
  - Files a request names (`node`, `nodes`, `symbol`, `from`/`to`; `path#name` counts as
    `path`) are checked before answering: size, then BLAKE3 against the index; a new file
    counts if it appeared after the last index started. A changed one triggers a re-index
    and the request waits for it (up to `--fresh-wait-ms`).
  - The watcher (the same filter as `cartograph serve`: `.gitignore`, `.ignore`,
    `.git/info/exclude`, the data dir) re-indexes after any other change, debounced
    300 ms. While such a run is pending, requests wait for it (bounded).
  - Git history never delays the file graph: an index is served with the history stored
    so far, then the history is brought up to date (one more generation if it changed).
    A commit that changes no working-tree file (seen through `HEAD`, `logs/HEAD`, the
    branch refs, `packed-refs` and `shallow`, also outside the root in a linked worktree)
    refreshes the history alone, no file is read again, and requests do not wait for it.
  - Build output does not trigger it: 40 s of `cargo test` rewriting this repo's `target/`
    caused no re-index (only the startup one).
  - If the wait runs out, the answer comes anyway with a first line saying the refresh is
    still running and may miss recent changes.
- **Shuts down cleanly.** When the client closes stdin the server cancels a running index
  at its next check and exits: 10-30 ms when idle. During an index it may take longer
  (the test allows 5 s; the runtime waits at most 500 ms for blocked threads). A client
  that kills the process soon after closing stdin is safe: the index is written in one
  transaction.
- **Concurrent use.** SQLite in WAL with `busy_timeout`: the MCP server, `serve` and the
  CLI can share an index. Two MCP servers on one repo both work (only the first holds the
  lock). `diff` and `check --base` never write the index (they build both sides in memory),
  so they never race with a server.

Tests: `crates/cartograph-cli/tests/mcp_handshake.rs` (handshake over real stdio, every
tool, errors, start without an index, close during indexing, freshness of a named file
without the watcher, notes across restarts) and `mcp_soak.rs` (600 mixed requests in
batches of 50 in flight, a quarter with bad arguments or unknown tools, garbage lines,
files changing under the watcher; checks one answer per id, budgets, no panic text, and
memory).

## Files in the data dir

cartograph keeps exactly these in a data dir (`.cartograph/` by default, or `--data-dir`):

| Path | What |
|---|---|
| `graph.db`, `graph.db-wal`, `graph.db-shm`, `graph.db-journal` | the index (SQLite and its side files) |
| `mcp.lock` | held locked by a running MCP server |
| `stats.jsonl`, `stats.1.jsonl`, `stats.lock` | usage stats, its rotation and the lock appends take |
| `annotations.json`, `annotations.json.tmp` | the notes of `annotate`, written through the `.tmp` |
| `.cartograph-base/` | cached base snapshots of `diff` and `check --base`, and their temporary trees |

With the data dir as the repo root, exactly these are never indexed nor watched
(`cartograph_index::DATA_FILES` and `DATA_DIRS`); otherwise the whole data dir is left out.
Base snapshots used to live in `base/`. In a data dir other than the repo root (the default
`.cartograph/`), the next `diff` or `check --base` deletes what an older version left
there (`<commit>.json` files that read as snapshots, their `.tmp`, `t<pid>-<n>/` temporary
trees) and nothing else. With the data dir as the repo root nothing in `base/` is deleted:
a `base/` holding only those is left out of the index and the watcher, and one holding
anything else is a source folder, indexed with everything in it.

## Usage stats

The server appends one JSON line per tool call to `<data dir>/stats.jsonl`. Appends from
all processes take
`<data dir>/stats.lock`; at 2 MiB the log rotates once to `stats.1.jsonl` (see
[Files in the data dir](#files-in-the-data-dir)). A record holds:

- the tool name (a tool the server does not have is recorded as `unknown`);
- the duration in ms, from the request to the answer (freshness wait included);
- the size of the text the client got, in characters and estimated tokens (chars / 4,
  as the budgets count them);
- whether the answer left items out: it contains `+N more` or `(truncated)`;
- the status (`ok`, `error`, `not_ready` for "indexing") and the error kind:
  `unknown_node`, `bad_argument`, `ambiguous`, `bad_params` (arguments that do not
  deserialize), `internal`, `timeout`, `unknown_tool`;
- a random 8-hex session id per process, and a Unix timestamp.

Nothing else: no arguments, paths, symbol names, messages or answer text, so the file can
be shared. At 2 MiB the file becomes `stats.1.jsonl` (replacing the previous one), so the
log keeps the last ~10-25k records in at most 4 MiB. Writes are one `write_all` of a
whole line in append mode; a failed write is logged at debug level and never fails or
delays a call. `cartograph mcp --no-stats` or `CARTOGRAPH_NO_STATS=1` turns it off.
Records written by other writers of older versions are read but left out of the summary.

```
cartograph stats [repo] [--data-dir DIR] [--days N] [--json]
```

Sample, from a scripted session on this repository (59 calls, a quarter of them with a
typo, a wrong type or an ambiguous name on purpose):

```
59 tool calls in 1 MCP sessions, 2026-11-21 → 2026-11-21

tool        calls errors  p50 ms  p95 ms  avg tok  max tok budget   over  trunc
callers         9      3       3      19      395      473   1000      0    50%
explain         9      6       4      23      256      256   1000      0   100%
deps            6      3       1       2      122      124   1500      0     0%
find            6      0       9      46      172      303    800      0    50%
impact          6      0       3       4      522      616   1500      0     0%
overview        4      0       3     323      561      601   1500      0   100%
check           3      0       1       1       54       57   1000      0     0%
entries         3      0       4       4      322      325   1000      0   100%
findings        3      0       1       2      321      323   1500      0     0%
hotspots        3      0       2       2       37       40   1000      0     0%
path            3      0       3       3      155      157    800      0     0%
symbols         3      0       2       2      126      128   1000      0     0%
unknown         1      1       1       1        0        0      -      -     0%
(tokens = chars/4 of the text sent; over = answers above the budget; trunc = answers that left items out, "+N more")

errors: ambiguous 3, bad_argument 3, bad_params 3, unknown_node 3, unknown_tool 1
```

(Debug build; the 323 ms overview is the first call after the server's startup index.)

**Not an MCP tool.** The summary is for the person tuning cartograph (which tools agents
use, which errors they hit, whether answers get cut), not for an agent answering a
question about the code. As a tool it would cost every session a name and a description
in the tool list, and invite the agent to spend calls on itself. `cartograph stats` is
enough.

Tests: `crates/cartograph-cli/tests/stats.rs` (one line per call, errors of every kind
included, no argument or path in the file, no re-index caused by writing the log inside
the repo).

## Evals

`cartograph eval [CASES]` (default `evals/cases.toml`; `cargo run -p cartograph-cli --
eval` from the repo) runs a suite of agent questions through the real MCP server and
checks the text the agent would read. No model is involved: every check is a substring.

```toml
[[case]]
id = "callers-index-to-store"
repo = "self"
question = "Before changing index_to_store: who calls it in production code?"
tool = "callers"
args = { symbol = "index_to_store" }
contains = ["crates/cartograph-cli/src/main.rs", "crates/cartograph-mcp/src/engine.rs"]
# also: not_contains = [...], in_order = true, error = "unknown_node", budget = 1000
```

A case fails when a fact is missing (or out of order), a forbidden one appears, the tool
errs unexpectedly (or with another code), or the answer is over its token budget (the
tool's contract by default). Options: `--filter <id part>`, `-a` prints the answers of
failing cases, `-A` of every case, `--json`, `--data-dir <dir>` (where the temporary
indexes go instead of the system temp dir; they are removed afterwards). Exit 1 when a
case fails.

A case that needs several calls lists them instead of `tool` and `args`; a call may go to
another repo of the suite. The checks and the budget then apply to all the answers
together (the default budget is the sum of the tools' contracts). `needs` tags a case with
the features it depends on, for instance one a tool does not have yet: a tool that does
not exist is a failed call, so such a case fails until the feature lands.

```toml
[[case]]
id = "client-of-a-route"
repo = "server"
question = "Which client code calls the route that creates an order?"
needs = ["http-bridge"]
contains = ["src/orders/OrderController.java", "src/api/orders.ts"]

[[case.calls]]
tool = "callers"
args = { symbol = "OrderController.create" }

[[case.calls]]
tool = "find"
args = { query = "orders" }
repo = "client"
```

The report gives per case pass or fail, the number of calls, the tokens of the answers
against the budget, and the time; with `needs`, also the passes per tag (`-` counts the
cases without one). `--json` carries the same fields (`calls`, `tokens`, `budget`,
`needs`, `by_need`).

Repos of a suite, each served by its own `cartograph mcp --no-watch` with the index in a
temp data dir (nothing is written into a repo). Paths are relative to the cases file, or
absolute, so a cases file can live anywhere and measure any repository on disk:

- `path`: served in place, read-only (also any other repository: `path =
  '/work/other-repo'`; in TOML, write a Windows path in single quotes or with forward
  slashes). The `self` repo is this working tree; its cases assert
  architectural invariants (which crate uses which, what a core file is, where the binary
  starts), never counts or line numbers, so ordinary edits do not break them, and they
  still catch an indexer regression on today's code.
- `fixture`: copied to a temp dir outside any git repository first (the synthetic
  fixtures of `tests/fixtures`; exact facts such as rule violations live here).
- `commits` and `worktree`: a git repository built at run time with fixed dates and
  people, for `diff`, `check(base)`, `hotspots` and history in `explain`. Nothing depends
  on this repository's history, branch or clone depth.

Before the first call the runner indexes the repo (files and git history) and waits until
the server has replaced that index with its own startup run, so no answer carries a
freshness note and the history is complete.

The suite is not part of `cargo test` (only the runner is, in
`crates/cartograph-cli/tests/eval_runner.rs`): a case that fails because a tool is wrong is
a finding to fix, and it stays visible instead of being weakened to keep the build green.
CI runs it in a job that may fail without failing the workflow.

### Results (2026-11-21, 34 cases: 34 pass)

Every answer is within its budget. Two cases on the `inherit-ts` fixture cover inheritance:
`callers` of a base method lists the calls made through subclasses that inherit it (and
not those of a class whose base is a library), and `hierarchy` lists a class's supertype
chain and subtypes. The first run (26 pass, 6 fail) found the defects below;
each is fixed in the tool, no case was changed:

| Case | Defect (fixed) |
|---|---|
| `deps-cli-main-uses-mcp`, `path-cli-to-mcp`, `impact-engine-reaches-cli` | Rust: a crate path used in an expression without a `use` (`cartograph_mcp::run_stdio(opts)`, `cartograph_server::run_blocking(…)`) creates no import edge. `callers` sees the call, but `deps`, `path` and `impact` say the CLI does not depend on `cartograph-mcp/src/lib.rs`, so changing `engine.rs` "breaks" nothing outside its crate. |
| `callers-index-to-store` | `callers` orders files by call count and lists 8: six test files with many calls come first and the production callers (`cartograph-cli/src/main.rs`, `cartograph-mcp/src/engine.rs`) end up in "+7 more files". |
| `callers-guard-run` | A method called through a field (`self.guard.run(…)`) does not resolve: "no resolved call sites". |
| `entries-main-no-fixtures` | `entries kind=main` listed 18 `main` functions of `tests/fixtures/…` next to the 3 real ones, while `overview` leaves fixtures out. `entries` now leaves out (and counts) entries in test, fixture and example trees below the queried folder; `include_tests` lists them. |

Fixes: Rust paths written in code without a `use` (`krate::f()`, `crate::m::T`) are import
dependencies; `callers` lists production callers first and sums up test callers; calls
through a typed field (`self.guard.run()`) resolve; and a crate root its own modules
`use crate::…` from stays an entry point, so `impact` of the MCP engine names
`crates/cartograph-cli/src/main.rs`.

## Timings

This repository (≈590 files, 68k LOC; release build, Windows 11, warm disk cache):

| What | Time |
|---|---|
| spawn → `initialize` response | 20-40 ms |
| first answer with no index (waits for the first index) | 1.3 s |
| first answer with an index from an earlier session | 38 ms (then marked "refreshing") |
| typical tool call (impact, callers, explain, path, deps) | 1-2 ms round trip; `find kind=sym` 11 ms; overview 2 ms (the first one after a cold index ≈ 40 ms) |
| `cartograph index` cold / warm (extraction cache) | 0.8 s / 0.32-0.41 s |
| soak, 600 mixed requests, 50 in flight (debug build) | 1.1 s total, ≈1.9 ms per request; RSS 28 MB flat |

A larger TypeScript/Java desktop app (≈1,300 indexed files, 20k calls; read-only, data
dir elsewhere): first index 2-8 s depending on the disk cache, during which calls answer
"indexing"; afterwards 300 mixed calls with 25 in flight (15 % with bad arguments) took
0.3 s, medians 5-27 ms with queueing, sequential calls 0-3 ms; RSS 36-60 MB; exit 27 ms
after stdin closed.

## Limits

- Freshness checks cover the files a request *names*. A change to another file reaches
  the index through the watcher, 300 ms after the write plus the re-index time; a call in
  that window can answer from the previous index (its result does not say so unless a run
  is already pending). Folders named as arguments are not hashed (the watcher covers
  them).
- Re-indexing is whole-repo (with the extraction cache, so unchanged files are not parsed
  again). On a very large repo a refresh takes seconds; requests then answer after
  `--fresh-wait-ms` with the stale-note.
- What the model sees when a result has both `content` and `structuredContent` is up to
  the client; some clients show only the structured JSON to the model. Hence text only by
  default; `--structured` is for clients that want the data.
- `diff` and `check(base)` build the base from git (no checkout; see [diff.md](diff.md)).
  The first call for a commit indexes it (seconds on a large repo, inside the tool timeout);
  later calls load it from `<data dir>/.cartograph-base/`. An unknown revision or a folder outside git
  is a tool error with the reason.
- The answers are only as good as the index. Dogfooding found that `impact` lists tests
  that import the file, not integration tests that run the built binary. (`callers` used
  to miss calls written through a path, such as `cartograph_index::index_to_store(…)` from
  the CLI, server and MCP crates; those resolve now.)
- `callers` and `hierarchy` see the calls and supertypes the indexer can resolve without
  type inference. A method a class inherits is found through its supertypes (base
  classes, interfaces with default methods, Rust traits with default methods and
  supertraits, Go embedded types, the script a GDScript file extends), for `this.m()`,
  `super.m()`, `self.field.m()` and `Type.m()`; a call through a local variable of
  unknown type is not. A base outside the repo (a library class) ends the lookup, so a
  method that may come from it stays unresolved rather than guessed; `Deref`, TS interface
  method signatures (not symbols) and other `partial` parts of a C# class are not
  followed. `callers` of a base method lists the calls made through subtypes that inherit
  it, not the calls to an override (`hierarchy` of the method lists the overriding
  subtypes).
- Notes are per node id: renaming a file orphans its note.
