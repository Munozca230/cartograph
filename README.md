# cartograph

cartograph builds an architecture map of a code repository: folders, files, imports,
symbols, calls, entry points and git history, kept in a local SQLite index that follows
the code as it changes. You can read that map in a local web UI, query it from the command
line, enforce architecture rules with it in CI, and serve it over MCP so that coding agents
answer structural questions ("who calls this?", "what breaks if I change this file?")
in one call instead of dozens of greps and file reads. It is a single binary, runs
offline, and writes nothing inside the analyzed repository unless you let it.

## Features

- **Index for 10 languages.** TypeScript and TSX, JavaScript, Python, Rust, Go, Java, C#, C,
  C++ and GDScript, plus Godot scenes, resources and `project.godot`, parsed with
  tree-sitter. Imports resolve through tsconfig paths, package manifests and workspaces,
  Python packages, Cargo crates, Go modules, Java and C# namespaces, and include paths.
  Each edge carries a confidence (exact, resolved, heuristic), and what does not resolve
  is listed with a reason. Besides what `.gitignore` ignores, the walk leaves out data and
  tool folders (captures, logs, backups, caches, hidden tool folders at the top) unless
  code imports them, copies of the repository (its worktrees), and minified bundles, each
  with a reason. A content-hash extraction cache makes re-indexing incremental.
- **Web UI.** `cartograph serve` opens a local map on `127.0.0.1` that updates live as
  files change:
  - **Plano**: the repository as a layered plan. Folders and files are cards, open folders
    are nested boxes and dependencies point down. The few edges that have to point up to
    close a cycle are drawn in magenta.
  - **Flujos**: call flows from the entry points (CLI commands, HTTP routes, `main` and
    `package.json` `main`, the scripts an `index.html` loads, Electron `ipcMain`
    handlers, Godot main scenes and autoloads) or from any function, expandable card by
    card.
- **Findings and rules.** Layer violations, file and folder cycles, fan-out limits, god
  modules, hubs, stable-dependency violations, orphans, deep chains and hidden coupling.
  Rules live in an optional `cartograph.toml`:
  - `cartograph check` exits 1 on a violation, for CI;
  - `cartograph check --base main` fails only on what the working tree introduces;
  - `cartograph diff [base]` shows what a branch changes in the architecture (findings,
    dependencies between modules, unresolved imports);
  - `cartograph infer` proposes layers from the dependency graph.
- **Bridges.** Links no import states, kept apart from imports: an Electron renderer
  reaching a main-process handler through the preload and an IPC channel (channels read
  through constants and handler wrappers), an HTTP client call reaching the Spring or
  Express route that serves it (paths read through constants, client wrappers and axios
  instances), and in a workspace a dependency on the package another repository builds.
  `callers`, `deps`, `impact`, `path` and the map follow them; channels without a handler,
  calls no route serves, unused routes and undeclared or skewed dependencies are
  findings. See [docs/bridges.md](docs/bridges.md).
- **Spring and slices.** Spring beans and injection points read from the annotations
  (`javax.*` and `jakarta.*` alike): `callers` of an interface method lists where its
  type is injected and which beans each place receives (a list gets them all; one bean by
  qualifier, `@Primary` or name), and `path` from a view to an HTTP client goes on past
  the client to the controller, the service bean, the repository and the collection or
  table it writes (`→ collection orders (Mongo, via Order)`). See
  [docs/spring.md](docs/spring.md).
- **String keys and tests to run.** `find` of a string key (an action or step name, a
  channel, a registry key) traces it across repositories: where it is defined, what runs
  it, which catalogs list it and where it is missing. `impact` ranks the tests to run by
  closeness, marks the slow ones and gives the command per build (Maven, Gradle,
  vitest/jest, pytest, go, cargo). See [docs/mcp.md](docs/mcp.md#string-keys).
- **Git layer.** Per-file churn, last change, owners, co-change and hotspots, read with
  gix (no `git` binary needed) and updated incrementally.
- **MCP server.** `cartograph mcp <repo>` is a stdio MCP server for any MCP client or
  coding agent. Its tools are `overview`, `explain`, `deps`, `impact`, `find`, `path`,
  `symbols`, `callers`, `hierarchy`, `bridges`, `entries`, `findings`, `check`, `diff`, `hotspots`,
  `annotate` and `open_map`. Each answer is compact text within a token budget, and the
  index stays fresh while the agent edits. Notes left with `annotate` can live in a
  versioned `cartograph-notes.toml` next to the code. When the user asks to see the architecture or
  a flow, `open_map` opens the web map in the browser, focused on the folder, file or
  function at hand, served from inside the MCP server (no `cartograph serve` needed).
- **Evals and stats.** `cartograph eval` runs a suite of agent questions through the real
  MCP server and checks the answers (`evals/cases.toml`). `cartograph stats` summarizes
  tool usage (calls, latency, errors, answer size) without recording arguments or paths.

## Install

### With npm

The npm package [`@munozca230/cartograph`](https://www.npmjs.com/package/@munozca230/cartograph)
runs the prebuilt binary for your platform (Windows x86_64, Linux x86_64 with glibc, macOS
arm64 and x86_64). It needs Node.js 18 or newer, and nothing else.

Install it globally, which puts `cartograph` on your `PATH`:

```sh
npm i -g @munozca230/cartograph
cartograph --help
```

Then register `cartograph mcp .` as a stdio MCP server in your MCP client, for the
repository the client starts in. The server starts from the installed binary: no network
access and no npx at each start.

```json
{
  "mcpServers": {
    "cartograph": { "command": "cartograph", "args": ["mcp", "."] }
  }
}
```

For clients whose `mcp add` command takes `--` before the server's command line:

```sh
<client> mcp add cartograph -- cartograph mcp .
```

To try it without installing, run `npx -y @munozca230/cartograph --help`. A server
registered as `npx -y @munozca230/cartograph mcp .` works too, but npx checks the registry
at every start.

**Windows.**

- PowerShell drops a bare `--` when it calls a native program, so the client never sees it
  and takes `cartograph mcp .` as its own options. Quote it:

  ```powershell
  <client> mcp add cartograph '--' cartograph mcp .
  ```

- On native Windows `npx` and the global `cartograph` are `.cmd` scripts, which a client
  that starts the server without a shell cannot run. Start them through `cmd /c`:
  `"command": "cmd"`, `"args": ["/c", "cartograph", "mcp", "."]` (or
  `["/c", "npx", "-y", "@munozca230/cartograph", "mcp", "."]`).

npm installs only the platform package that matches your system
(`@munozca230/cartograph-<platform>-<arch>`, an optional dependency), so do not install
with `--omit=optional`.

### From a release archive

Download the archive for your platform from the releases page (Windows x86_64, Linux
x86_64, macOS arm64 and x86_64) and check it against its `.sha256`. Then put `cartograph`
(`cartograph.exe` on Windows) on your `PATH`. The web UI is embedded in the binary. The
binaries are not signed, so macOS Gatekeeper and Windows SmartScreen warn on the first
run.

### From source

Requirements: Rust 1.90 or newer, Node.js with npm (to build the web UI), and a C
compiler (the bundled SQLite and the tree-sitter grammars are compiled from C).

```sh
git clone https://github.com/Munozca230/cartograph
cd cartograph
npm --prefix web ci
npm --prefix web run build       # web/dist, embedded into the binary at compile time
cargo build --release            # target/release/cartograph
```

Without `web/dist` the binary still builds and works; `serve` then shows a page on how
to build the UI. `cartograph doctor` reports what the binary can do in a given repository.

## Quick start

```sh
cd path/to/repo
cartograph index                       # writes .cartograph/graph.db
cartograph serve --open                # web UI on http://127.0.0.1:7878, live
cartograph query overview              # the map, as text
cartograph query impact src/model.rs   # what depends on it, which tests to run
cartograph query callers parse_config  # who calls a function
cartograph check                       # exit 1 on a rule violation
cartograph diff main                   # what this branch changes in the architecture
cartograph mcp                         # MCP server on stdio
```

Every command takes the repository as an argument (`-C <repo>` for `query` and `diff`)
and defaults to the current directory. Add `.cartograph/` to `.gitignore`, or pass
`--data-dir DIR` to keep the index outside the repository so that nothing is written
inside it. `cartograph <command> --help` lists every option.

To use the MCP server, register it in your client as a stdio server: `cartograph mcp .`
after a global npm install (see [With npm](#with-npm)), or the path of the binary:

```json
{
  "mcpServers": {
    "cartograph": { "command": "/path/to/cartograph", "args": ["mcp", "/path/to/repo"] }
  }
}
```

[docs/mcp.md](docs/mcp.md) covers the tools, a recommended agent workflow, and the
server's freshness and robustness guarantees.

## Several repositories

A system split over several repositories maps as one: put a `cartograph-workspace.toml`
listing them in the folder that holds them (`cartograph workspace init` proposes it), then
index, serve, query, check or serve MCP on that folder. Each repository becomes a
top-level folder of the map named after it (`api:src/main.ts` is accepted for
`api/src/main.ts`), imports resolve across repositories, each keeps its own git history
and its own `cartograph.toml` rules, and `diff`/`check --base` compare each one with its own
base. See [docs/workspace.md](docs/workspace.md).

## Configuration and docs

- [`cartograph.toml`](docs/cartograph-toml.md): layers, rules (`max_fan_out`,
  `no_cycles`), finding thresholds and ignored paths. It is optional; `cartograph infer`
  writes a first one.
- [docs/mcp.md](docs/mcp.md): the MCP server, its tools, evals and usage stats.
- [docs/diff.md](docs/diff.md): `diff` and `check --base`.
- [docs/workspace.md](docs/workspace.md): several repositories as one map.
- [docs/bridges.md](docs/bridges.md): IPC channels, HTTP routes and packages between files
  and repositories.
- [docs/spring.md](docs/spring.md): Spring beans, injection points and slices down to the
  data.
- [docs/api-f2.md](docs/api-f2.md): the HTTP API behind the web UI.
- [docs/release.md](docs/release.md): building release archives and publishing to npm.
- [web/README.md](web/README.md) and [web/DESIGN.md](web/DESIGN.md): the web UI.

The web UI is in Spanish.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.

The binary bundles third-party code under its own licenses, including elkjs (EPL-2.0) and
two OFL-1.1 fonts; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
