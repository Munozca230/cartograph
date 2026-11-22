# Precise calls with SCIP (N3)

cartograph finds symbols and calls with tree-sitter (N2). Most calls resolve by language
rules (`resolved`), some only by a unique name (`heuristic`), and some not at all. A
[SCIP](https://github.com/scip-code/scip) index, written by a compiler-backed indexer,
knows exactly which definition every reference points to. When one is present, cartograph
reads it and, for the files it covers:

- every call to a symbol the index defines comes from SCIP and is `exact`; tree-sitter
  calls to such a symbol that SCIP does not confirm are dropped (no duplicates, no false
  edges from a guess). A call to a symbol the index does not define (a file outside the
  indexed project, as in a monorepo with one index per package) keeps its tree-sitter
  confidence;
- definitions tree-sitter missed (an object literal's method, a generated accessor) are
  added as symbols marked `(scip)`;
- a reference to a symbol of another file adds a `uses` dependency between the two files
  when no import already links them (a barrel re-export, a type reached through a
  method, a Java class of the same package); findings treat it like an import;
- a reference to a library's symbol adds the file → library dependency when the file has
  no import of it, mapped by package name (`npm`, `cargo`, `pip`/`python`, `maven`,
  `gomod`, `nuget`) to the libraries the import resolvers found.

Files the index does not cover keep their tree-sitter data. cartograph never runs an
indexer and never downloads one: you run it, cartograph only reads the file.

## Use it

1. Run the indexer for your language (below) at the root of the project. It writes
   `index.scip`.
2. Put `index.scip` at the root of the repository, or pass it explicitly:

   ```sh
   cartograph index                       # picks up ./index.scip
   cartograph index --scip web/index.scip --scip api/index.scip
   cartograph index --no-scip             # ignore every SCIP index
   ```

   `--scip` is repeatable (one index per package of a monorepo) and remembered: later
   re-indexes without the flag (`cartograph index`, `serve`, `mcp`) keep using those
   files while they exist. `--no-scip` forgets them. `cartograph serve` and
   `cartograph mcp` always pick up `<repo>/index.scip`.
3. `cartograph index` prints what it matched:

   ```text
   scip index.scip: 412 documents, 409 match files under web/
   scip: 409 files precise · symbols 3120 matched, 41 added · calls 2210 confirmed (1630 upgraded), 380 added, 95 dropped · 12 file and 3 library dependencies added
   ```

   `overview` shows one `precise calls (scip)` line, `symbols` and `explain` say
   `precise (scip)` for covered files and mark added symbols `(scip)`, and `callers`
   marks each call site that comes from SCIP `[exact, scip]`.

Add `index.scip` to `.gitignore`: it is a build artifact.

### Paths

Document paths in a SCIP index are relative to the project root the indexer ran in. That
root does not have to be the repository root: cartograph maps the paths through the
indexer's `project_root` when it is a folder of this machine, else through the folder of
the `.scip` file, the repo root, or the trailing folders of `project_root` (an index
built in CI under `/ci/work/checkout/web` maps to `web/`), whichever matches the most
files. Documents that match no indexed file are counted and listed in the summary.

### Staleness

SCIP data points at lines. A file edited after `index.scip` was written is left to
tree-sitter (counted as "changed since it was written") until you regenerate the index.
`cartograph doctor` warns when `index.scip` is older than files it covers; it never fails
because of it. Under `cartograph serve` or `cartograph mcp`, writing a new
`<repo>/index.scip` re-indexes, even when it is git-ignored; files given with `--scip`
are read again on the next re-index.

### Cost

The index is decoded one document at a time (the protobuf decoder never holds more than
one document), into a compact form: definitions and references of global symbols with
their lines, which stays in memory for the merge. That form is cached in the graph
database (when under 256 MiB). An unchanged `index.scip`
(same size and modification time, or same content hash after a `touch` or a checkout) is
never decoded again; only the merge runs on a re-index. Files over 4 GiB, documents over
512 MiB, and corrupt or foreign files are reported as a warning, and indexing goes on
without them.

## Produce an index.scip

Commands as documented by each indexer at the time of writing; check its README for the
current flags. Run them from the project root (the folder with `tsconfig.json`,
`Cargo.toml`, `pom.xml`, `pyproject.toml`, `compile_commands.json` or the `.sln`).

| Language | Indexer | Typical command |
|---|---|---|
| TypeScript, JavaScript | [scip-typescript](https://github.com/sourcegraph/scip-typescript) | `npm install -g @sourcegraph/scip-typescript` then `scip-typescript index` (`--infer-tsconfig` for a JavaScript project without `tsconfig.json`; `--yarn-workspaces` or `--pnpm-workspaces` for a monorepo) |
| Rust | [rust-analyzer](https://rust-analyzer.github.io/) | `rustup component add rust-analyzer` then `rust-analyzer scip .` |
| Java, Kotlin, Scala | [scip-java](https://github.com/sourcegraph/scip-java) | `scip-java index` (builds the project with Maven, Gradle, sbt or Bazel; also published as the `sourcegraph/scip-java` container image) |
| Python | [scip-python](https://github.com/sourcegraph/scip-python) | `npm install -g @sourcegraph/scip-python` then `scip-python index . --project-name <name>` (in an environment with the project's dependencies installed, so library symbols resolve) |
| C, C++ | [scip-clang](https://github.com/sourcegraph/scip-clang) | `scip-clang --compdb-path build/compile_commands.json` (needs a compilation database, e.g. CMake's `-DCMAKE_EXPORT_COMPILE_COMMANDS=ON`) |
| C#, Visual Basic | [scip-dotnet](https://github.com/sourcegraph/scip-dotnet) | `dotnet tool install --global scip-dotnet` then `scip-dotnet index` |
| Go | [scip-go](https://github.com/sourcegraph/scip-go) | `go install github.com/sourcegraph/scip-go/cmd/scip-go@latest` then `scip-go` |

GDScript and Godot scenes have no SCIP indexer; they stay on tree-sitter.

A monorepo with several languages gets one index per indexer: write each one next to its
project (`web/index.scip`, `server/index.scip`) and pass them all with `--scip`.

## What it does not do

- A reference is not always a call. Where tree-sitter saw a call on that line to the same
  target, SCIP confirms it. Otherwise a reference to a function or method inside a
  function becomes a call only when the source text calls it: the name is followed by
  `(`, or by generic arguments and `(` (`parse<T>(`, `collect::<Vec<_>>()`). A function
  passed or stored as a value (`debounce(run, 10)`, `items.map(normalize)`, `{ onClick:
  save }`) or a method read without a call only adds file dependencies, as do references
  to types and constants where tree-sitter saw no call (`new Store()` is one).
- A reference at module level becomes a call only when tree-sitter saw that call; other
  module-level references (exports, type annotations) only add file dependencies.
- Files of one Go package reference each other freely; those references add no
  dependency (the package is the unit, as in N1).
- The `uses` dependencies feed findings, `deps`, `impact` and `path`, but not the folder
  and package edges of the web map, which are drawn from imports.
- `diff` and `check --base` never read SCIP: the base revision has no index, so both
  sides are compared on tree-sitter data alone.
- A `uses` dependency is never type-only: a type reached through a barrel that SCIP
  resolves counts as a runtime dependency, so cycle findings may include it.
- A call is confirmed by its line: when tree-sitter records a multi-line call on another
  line than the one SCIP gives for the name, it counts as dropped and added again (from
  the enclosing function; one at module level is lost).
- Call counts of a covered file (`calls made here: … unresolved N`) keep tree-sitter's
  count of calls into libraries and builtins, less the calls only SCIP found: an
  estimate, since SCIP does not tell a call from another use.
