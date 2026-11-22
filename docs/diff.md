# diff and check --base: what a change does to the architecture

```
cartograph diff [BASE] [-C REPO] [--data-dir DIR] [--config FILE] [--json]
cartograph check [REPO] --base BASE [--data-dir DIR] [--config FILE] [--json]
```

Both compare the **working tree** (HEAD plus uncommitted and untracked, non-ignored files)
with a git revision `BASE` (a branch, tag, commit or `HEAD~3`). Without `BASE`, `diff` uses
the merge-base of HEAD with the default branch (`origin/HEAD`, else `main`, `master`,
`origin/main`, `origin/master`); on that branch itself it is HEAD, so the diff shows the
uncommitted work. The MCP tools `diff(base?)` and `check(base?)` do the same.

## What diff reports

Within 1.5k tokens, every list ending in `+N more`:

- **files** added and removed (by path);
- **rule violations introduced**: new ones, and existing ones that now rest on a dependency
  the base lacks ("worse"), showing only the new evidence;
- **new and resolved findings** (cycles, god modules, orphans…), matched by stable id;
  cycles also by shared files, since a cycle's id names one of its files;
- **dependencies between modules**, new, removed and changed in count. A module is a layer of
  `cartograph.toml` when the file is in one; otherwise the package (a manifest below the
  root) that holds it; otherwise its top folder, where a folder with half the repo's files
  or more is split into its subfolders;
- **unresolved imports** new and fixed, matched by file, specifier and reason (not line).

Example on this repository (`cartograph diff HEAD~20`, at an earlier point of its history):

```
diff HEAD~20 (6353f757) → working tree
files: +20 −0; added crates/cartograph-mcp/Cargo.toml, crates/cartograph-mcp/src/annotations.rs, crates/cartograph-mcp/src/base.rs, crates/cartograph-mcp/src/engine.rs, crates/cartograph-mcp/src/guard.rs, +15 more
unchanged findings: 4
dependencies between packages and top folders: 5 new, 0 removed, 2 changed
  + crates/cartograph-mcp/ → crates/cartograph-query/ (8 imports, e.g. crates/cartograph-mcp/src/annotations.rs:10 → crates/cartograph-query/src/explain.rs)
  + crates/cartograph-mcp/ → crates/cartograph-core/ (3 imports, e.g. crates/cartograph-mcp/src/base.rs:5 → crates/cartograph-core/src/lib.rs)
  …
  ~ crates/cartograph-cli/ → crates/cartograph-index/: 2 → 3
```

## check --base

Exit 1 only for violations the working tree **introduces** relative to the base (new or
worse, as above); violations that are also in the base are listed as *existing* and do not
fail; violations the change removed are counted as *fixed*. Exit 2 when the check cannot run
(not a git repository, unknown revision, invalid config). Without `--base`, `check` is
unchanged: every violation of the stored index fails.

A violation is "the same" on both sides when every file → file dependency it rests on is in
the base too. So one more import across an already violated layer pair fails (only that
import is shown), and so does one more dependency of a file already over `max_fan_out`.

## How the base is built

- The working tree is indexed **in memory**, as the base is: the stored index is never
  written, so `diff` and `check --base` never race with `index` or an MCP server on
  the same data dir, and a stale index never hides a change. The extraction cache of the
  stored index (if there is one) is read, so unchanged files are not parsed again. Both
  sides use `--max-file-bytes` when given (for this run only), else the size limit the
  stored index was built with, else 1 MiB. Calls and entry points are not resolved on
  either side: the comparison does not use them. Without a stored index it still runs
  (every file parsed, no git history) and creates none; with an unreadable one, the same
  after a warning.
- The extraction cache is not refreshed either: every run parses again the files changed
  since the last `cartograph index` (all of them without an index). Where `diff` or
  `check --base` runs often without an MCP server keeping the index current, as in
  CI, run `cartograph index` once first (for example when the CI cache is set up, with the
  data dir kept in that cache): later runs then parse only what changed since.
- The base is read with gix: the tree of the commit (only the part under the indexed root) is
  written to a temporary folder under `<data dir>/.cartograph-base/`, indexed by the usual pipeline with
  the working tree's extraction cache (read only), and the folder is deleted. No checkout,
  no worktree, no ref, nothing written in the repository.
- Its graph is cached as `<data dir>/.cartograph-base/<commit>.json` (key: commit, indexer and extractor
  versions, size limit, indexed sub-folder, `core.autocrlf`/`core.eol`), with the
  working-tree files its build read or looked for (the files resolvers read from disk, such
  as a `tsconfig` in `node_modules` or an untracked `compile_commands.json`, and the
  `.gitattributes` of every folder) and their size and modification time: a cached base is
  reused only while they are unchanged. The 8 most recently used are kept. A second
  `check --base main` only loads it.
- Both sides use the **current** `cartograph.toml` and the same git history, so a difference
  is a difference in code, never in the rules.
- What a checkout would see: resolvers that look on disk for unindexed files (a `tsconfig`
  in `node_modules`) look in the working tree; Git LFS pointers become files of the size
  they stand for (skipped as binary or too large, as the real files are); symlinks and
  submodules are left out (on both sides: the walk skips every folder holding a `.git`
  file); blobs over the size limit are written as files of zeros one byte over the limit
  (skipped as too large, never written at full size); text goes through the worktree
  conversion (`core.autocrlf`, `eol`…), so unchanged files keep the working tree's hash and
  come from the extraction cache. A file whose name cannot be created on this system is
  reported (and counts as added).

## Timings

`check --base HEAD~20`, release build, Windows 11. "Cold" = no cached base (the working
tree's index and extraction cache exist); "warm" = base cached. Both include indexing
the working tree and computing the findings of both sides. (Measured when the working
tree was still re-indexed into the store, ≈0.6 s of each run spent writing it; see below
for the in-memory build.)

| Repo | cold (base built in) | warm (base loaded in) |
|---|---|---|
| this repository (≈630 files, Rust) | 4.0 s (2.9 s; 394 of 421 files from the cache) | 1.2-1.4 s (7-11 ms) |
| a TypeScript desktop app (≈1,270 files, CRLF working tree) | 4.9 s (4.0 s; 1,114 of 1,172 from the cache) | 1.1-1.4 s (18-23 ms) |

Building the working tree in memory instead (debug build, `check --base HEAD~5` on this
repository, ≈665 files, on a busy machine, so ranges; re-indexed into the store → in
memory): with an index and the base cached, 0.7-2.3 s → 0.5-1.6 s; with an index and the
base built, 3.8-8.5 s → 3.3-6.1 s; without any index, about the same (6-12 s), since the
files the working tree's build parses feed the base's.

Building the base is dominated by creating its files on disk: on this machine a plain `cp`
of 1,176 files of the same app takes 5.1 s (and `rm` 1 s), so the temporary tree costs
about 3 ms per file; parsing is mostly skipped by the cache. Without the worktree
conversion the CRLF app got 3 cache hits instead of 1,114.

Loading a cached base also checks the working-tree files its build read (one `stat` each;
measured before that check was added): about 320 on this repository (a `.gitattributes`
per folder, the manifests the resolvers read from disk), ≈10 ms on Windows.

## Limits

- Findings keep a sample of their edges (48); a cycle whose shortest loop is unchanged but
  that grew elsewhere counts as existing. Layer violations past the sample, and fan-out
  violations, are worked out again from the dependencies the rules count, as the detectors
  do (tests and files of `ignore` left out, type-only imports in), so an import the rules
  do not count never makes a violation worse.
- A cycle that is not a violation and is replaced by another sharing a file shows as
  unchanged.
- Renamed files are a removal plus an addition (paths are compared, not content).
- The worktree conversion uses the working tree's `.gitattributes` (as a checkout in place
  would), not the base's; external filter drivers (LFS smudge) never run. If the bytes
  differ anyway, a file is just parsed again: results are the same.
- SCIP indexes (N3) are not read on either side (the base revision has none; see
  [scip.md](scip.md)); dependencies are those of the N1 import layer.
