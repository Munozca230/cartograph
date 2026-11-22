# Bench: extraction cache and `cartograph serve`

Date: 2026-11-20. Same machine as `bench-f1.md` (Windows 11 Pro, 8 cores, Rust 1.99.1, release
build). Every run was read-only on the analyzed repo, with `--data-dir` in a scratch folder.
None of the repos had a `.cartograph` directory before or after the runs.

The repositories are the private ones of `bench-f1.md`, named by the same labels. The
stress repo is bench A×24 (24 git worktrees of an Electron + React TypeScript app, ~30k
files, ~125k imports).

## Extraction cache (`cartograph index`, wall time)

The runs were sequential, with nothing else running at the same time.

| Binary | Run 1 | Run 2 |
|---|---:|---:|
| before (no cache) | 176.8 s (cold file cache) | 39.1 s |
| after (cache, empty data dir) | 35.8 s (empty extraction cache, warm file cache) | **15.4 s** (every file hit) |

How to read the table:
- Run 1 of "before" is the only run with a cold Windows file cache. Run 1 of "after" ran right
  after it, so it measures an empty extraction cache, not a cold disk.
- The fair comparison is the two warm runs, 39.1 s against 15.4 s.

Phase timings of the warm runs, from the `timings` metadata:

| | walk | read + hash + parse | build | resolve | write | total |
|---|---:|---:|---:|---:|---:|---:|
| before | 3.0 s | 28.2 s | 0.2 s | 0.9 s | 4.5 s | 37.0 s |
| after | 3.9 s | 3.9 s | 0.3 s | 1.1 s | 6.1 s | 15.5 s |

Resolution still runs over every file on every index. What remains is mostly the walk and the
full rewrite of the graph tables. Writing only the rows that changed is the next lever.

bench B (a Next.js TypeScript web app, ~170 files) drops from 156 ms to 58 ms (index time,
every cacheable file hit).

## `cartograph serve` response times (curl, loopback, 3 runs each)

| Endpoint | bench A (~1.3k files) | bench A×24 (~30k files) |
|---|---:|---:|
| `/api/tree` (full) | 30-53 ms, 311 KB | 500-580 ms, 8.4 MB |
| `/api/tree?depth=2` | 11-25 ms | 31-51 ms |
| `/api/graph` | 4-10 ms | 26-61 ms |
| `/api/graph?expanded=<top dir>` | 3-17 ms | 18-41 ms |
| `/api/search?q=button` | 9-14 ms | 45-76 ms |
| `/api/meta` | 2-28 ms | — |

Other measurements:
- On the worktrees repo the first index of a `serve` process, with a warm cache, took 11.1 s,
  and the API was ready after about 16.6 s.
- The generation did not move while the server sat idle, with the watcher on.
- The HTTP server starts listening at once. On Windows, a `curl` sent before the port is open
  waits about 2 s for the refused connection, so a startup measured from outside looks slower
  than it is.

On bench B the weight sums of `/api/graph` were checked by hand against the `import` edges in
the database: collapsed, and with `src` expanded, the sums equal the crossing file edges.
