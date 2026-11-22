# Bench: import graph (TS/JS and Python)

Date: 2026-11-20. Machine: Windows 11 Pro, 8 cores, Rust 1.99.1, release build
(`target/release/cartograph.exe`, static CRT). The runs were made before the project was
renamed (from codevgraph), so the binary was then called `codevgraph.exe`; nothing else
differs.

The benchmark repositories are private, so they are named by label:

| Label | What it is |
|---|---|
| bench A | Electron + React TypeScript desktop app, ~1.3k files |
| bench A×24 (stress) | 24 git worktrees of bench A side by side, ~30k files |
| bench B | Next.js TypeScript web app, ~170 files |
| bench C | TypeScript Cloudflare Worker, ~190 files |
| bench D | the root of a mixed monorepo (TS/JS, some Python and Java), ~3.4k files; bench A and bench E are nested repositories in it |
| bench E | TypeScript Node service, ~490 files |

Every run was **read-only** on the analyzed repo. The database went to a scratch folder
outside the repo through `cartograph index --data-dir <scratch>/db-<name> <repo>`. Before
the runs, none of the repos had a `.cartograph` directory, and afterwards there was still
none (see "Write check" at the end).

## How it was measured

- **Wall time:** PowerShell `Stopwatch` around `Start-Process`. Each repo ran twice in a row.
  Run 1 is "cold": first contact after a while, so the Windows file cache is cold (and,
  presumably, Defender scans the files on first read; that is inferred, not traced). Run 2 is
  "warm".
- **Peak memory:** `Process.PeakWorkingSet64`, polled every 20 ms while the process ran. It
  is a sampled lower bound, not an exact figure.
- **Resolution:** queried from the `import_sites` / `unresolved` tables; the same numbers
  appear in `cartograph report` under `## Imports`.
  - *Internal* means resolved to a file in the repo.
  - *External* means `npm:` / `node:` / `pypi:` / `python:` / scheme packages.
  - *Classified* means internal + external.

## Results

File and import counts are rounded; shares are exact.

| Repo | Files indexed | Imports | Internal | External | Unresolved | Classified | Cold | Warm | Peak WS |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| bench B | ~170 | ~440 | 60.5% | 39.5% | 0 | 100% | 3.8 s | 0.65 s | 17 MB |
| bench C | ~190 | ~460 | 65.4% | 34.6% | 0 | 100% | 3.2 s | 0.36 s | 13 MB |
| bench D (root) | ~3.4k | ~2.1k | 60.5% | 38.4% | 1.05% | 98.9% | 42.7 s | 3.5 s | 26 MB |
| bench A | ~1.3k | ~5.2k | 61.2% | 38.7% | 0.10% | 99.9% | 12.9 s | 2.1 s | 21 MB |
| bench E | ~490 | ~2.2k | 74.4% | 25.5% | 0.09% | 99.9% | 4.0 s | 0.9 s | 19 MB |
| **bench A×24 (stress)** | **~30k** | **~125k** | 61.3% | 38.6% | 0.10% | 99.9% | 181.5 s | 42.6 s | 155 MB |

Notes on the table:

- **bench C before the fix:** the first run had 2 unresolved imports (0.43%), both
  `cloudflare:sockets`. The `cloudflare:` bug below covers the fix.
- **Python:** bench D has a few dozen Python imports, all classified (stdlib, PyPI or
  internal). That is too little Python to say much about the Python resolver on real code;
  the golden fixture `tests/fixtures/imports-py` is the main evidence.
- **Java:** Java imports were not resolved yet at the time. The bench D root run indexed its
  `.java` files at the file level (files, LOC, no imports). Most Java of that tree lives in
  the nested repos that the root's `.git/info/exclude` hides (see below).
- **Confidence of file imports:** bench B resolved two thirds of its internal imports
  through `@/…` tsconfig aliases (`Resolved`), the rest `Exact`; bench D resolved a few
  dozen through workspace packages. Every other repo had only `Exact` edges. No repo
  produced a `Heuristic` edge.

### Phase timing (warm, from the `timings` metadata)

| Repo | walk | read + hash + parse | build | resolve | write db | total |
|---|---:|---:|---:|---:|---:|---:|
| bench B | 109 ms | 266 ms | 1 ms | 12 ms | 22 ms | 415 ms |
| bench A | 179 ms | 1,457 ms | 5 ms | 77 ms | 187 ms | 1,913 ms |
| bench A×24 | 2,319 ms | 33,178 ms | 221 ms | 977 ms | 4,064 ms | 40,829 ms |

Resolution is cheap: 1 s for 125k imports, running in parallel with immutable resolvers. The
cost is parsing:

- A single-threaded profile over bench A's `.ts`/`.tsx` files (~8.6 MB) spent 0.5 s
  reading, 3.9 s in the tree-sitter parse and 1.8 s running the import query. The parser
  alone runs at about 2.2 MB/s per core.
- Anchoring the `require` pattern to the first argument did not change the query time. The
  cost is the cursor walking the whole tree, not match fan-out, so the change was reverted.
- Possible later optimisations: skip the query on files with no `import`/`require` text, or
  pre-filter by byte search.

### Golden fixtures (for reference)

| Fixture | Imports | Internal | External | Unresolved (all deliberate) | Classified |
|---|---:|---:|---:|---:|---:|
| `tests/fixtures/imports-ts` | 42 | 25 (16 exact, 8 resolved, 1 heuristic) | 9 | 8 (4 not_found, 2 dynamic, 1 ambiguous, 1 parse_error) | 81.0% |
| `tests/fixtures/imports-py` | 37 | 17 (7 exact, 9 resolved, 1 heuristic) | 16 | 4 (2 not_found, 1 dynamic, 1 parse_error) | 89.2% |

## Nested repositories: what is and is not indexed

The bench D root's `.git/info/exclude` hides its nested repositories (bench A, bench E and
others) and every worktree folder. The walker honours git excludes just as it honours
`.gitignore`, so indexing the bench D root skips them. This is intended (the user's own
ignore rules apply), but it surprises people: the root run finds a few hundred `.ts` files
while the tree holds about 21k. The nested repos were therefore benchmarked as their own
roots, and bench A×24 is the stress test.

The root run also skipped about 3.5k binary files and about 90 files over 1 MB. All of them
are listed in the `skipped` table, mostly screenshots, videos and spreadsheets in backup and
capture folders.

## Unresolved imports: top reasons

### bench D root (~20)

| Reason | Share | Examples |
|---|---:|---|
| not_found | ~2/3 | partial copies of scripts in a capture folder whose siblings were not copied; imports of a PNG that was skipped as binary; a test corpus importing screens that are not in the corpus |
| dynamic | ~1/3 | `import(pathToFileURL(path.resolve(here, "…")).href)`, `import(impl)`, `import(script)`, `require(path.join(dir, "node_modules", "…"))` |

These are real gaps in the code, not resolver errors. Two config diagnostics were also
recorded (in the `diagnostics` table and the report's "Diagnostics" section; at bench time
they still went to `skipped` as `parse_error`, fixed since): two sample projects'
`tsconfig.json` extend a base file one directory off from where it really is. This is a real
bug in those samples.

### bench A and the stress test

| Reason | Share | Examples |
|---|---:|---|
| not_found | ~4/5 | imports of a generated, gitignored source file (the detail says "exists on disk but is not indexed (ignored)"; in some worktrees it does not exist at all); an image import whose target was skipped as binary |
| dynamic | ~1/5 | `import(pathToFileURL(req.resolve('playwright-core')).href)` in a helper script, once per worktree |

The worktree numbers are exactly the bench A numbers times the 24 worktrees. The resolver
behaves the same on 30k files as on 1.3k.

### bench E (2)

Both are `dynamic`: `import(script)` in contract tests.

### bench B (0), bench C (0)

None after the `cloudflare:` fix.

## Crashes and things that looked wrong

- **No crashes, panics or errors** on any repo. stderr held only the two tsconfig warnings in
  bench D.
- **Bug, fixed:** runtime-scheme specifiers (`cloudflare:sockets` in bench C's Cloudflare
  worker) were `not_found` with "not a valid package name".
  - `node:`, `bun:` and `cloudflare:` now become External `<scheme>:<name>`.
  - Deno-style `npm:` / `jsr:` now become `npm:<pkg>` / `jsr:<pkg>`.
  - Covered by a unit test and the `imports-ts` golden fixture (`apps/web/src/runtime.ts`).
- **Bug, fixed (found while writing the golden fixtures):** `from pkg import ns`, where `ns`
  is a namespace package, pointed at `pkg/__init__.py` instead of the `ns` dir.
- **Bug, fixed (found in review, not by the benches, which held little Python):** any
  top-level folder counted as a Python namespace package. So `from alembic import op` in a
  repo with an `alembic/` migrations folder pointed at the folder, and `import docker.errors`
  with a `docker/` folder became `not_found`. Following PEP 420, a bare folder now loses to an
  installed package, unless it is nested in a regular package or the import lands on a file in
  it.
- **Cold first runs are I/O-bound.** The cold bench D root run took 42.7 s against 3.5 s
  warm. Most of its files are binaries and documents in backup folders, read cold (Defender
  scanning is a likely but untraced contributor). The walker reads up to 1 MB of each file
  before sniffing for NUL bytes, so a binary costs up to 1 MB of I/O. Sniffing the first 8 KB
  first would make cold runs on folders like this much cheaper; that walker improvement was
  not part of this bench.
- **Imports of assets** (`./logo.png`) are `not_found`, with the detail "was skipped (binary)".
  That is by design: resolution only looks at indexed files. It inflates `not_found` a little
  in UI-heavy code.

## Write check (read-only guarantee)

- **Before:** no `.cartograph` existed in any of the repos. Marker files were created in the
  scratch folder, and bench D's `git status --porcelain` was recorded (with a checksum).
- **After:**
  - **`.cartograph`:** none at any indexed root (benches A to E and bench A×24) or in any of
    the 24 worktree dirs. That directory is the only thing cartograph could create inside a
    repo, and with `--data-dir` it is never created.
  - **Newer files:** `find <repo> -newer <marker>` (pruning `node_modules` and `.git`) listed
    no file in bench B or bench C. The same full-tree scan over bench D was stopped after
    about 30 minutes (the tree is huge), so the targeted checks are the evidence for bench D.
  - **git status:** `git --no-optional-locks status --porcelain` in bench D gives exactly the
    same entries and checksum as before.
- **Automated:** the CLI test `data_dir_keeps_the_analyzed_repo_untouched` checks the same
  guarantee for index, report and doctor, comparing every file's size and mtime before and
  after.
- **Caveat:** the first state snapshot used a plain `git status` in bench D, and git may
  refresh `.git/index` stat data when it runs. That came from the bench procedure, not from
  cartograph; every later check used `--no-optional-locks`.
