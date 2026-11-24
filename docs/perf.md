# Performance budget

A change must not make indexing slower, hungrier or bigger without anyone noticing. The
`perf` job of `.github/workflows/ci.yml` measures that on every pull request and every push
to `main`, with `scripts/perf-gate.mjs`.

## What it measures

Runners differ from one run to the next, so there are no absolute numbers to meet. The job
builds two binaries on the same runner, the change (head) and its base, and compares them:

- base: the merge base of a pull request with its target branch; `HEAD~1` on a push;
- both binaries index the same corpora, each into a fresh data dir outside the corpus,
  after one untimed run that warms the disk cache; base and head runs alternate, so a
  drift of the runner hits both alike.

| Metric | How | Limit |
|---|---|---:|
| cold index | wall time of `cartograph index` into an empty data dir, median of 3 | +25 % (and +0.25 s) |
| peak RSS | of those runs, median (`/usr/bin/time -v`) | +20 % |
| DB size | `graph.db` (plus `-wal`, if any) after a cold index | +10 % |
| warm index | re-index of an unchanged corpus, median of 3 | reported |
| queries | sum of the median time of each query (`find`, `callers`, `findings`, `bridges`) | reported |

A metric fails when the head exceeds the base by more than its limit; a time must also
exceed it by more than 0.25 s, so a corpus that indexes in a second does not fail on
jitter. The table is printed in the log and in the job summary either way.

The job **blocks pull requests** and **only reports on a push to main** (the commit is in
already). Warm index and query times are reported, not gated: they are short and dominated
by process start and loading the database, so 3 runs do not tell a change from noise.

### Why these limits

Running the script with the same binary as base and head gives the noise. On a Windows
desktop (8 cores), three runs over this repo moved the cold index by at most 3.9 %, peak
RSS by at most 0.9 %, warm index by at most 2.1 %, and the DB size by 0 (it is
deterministic). Shared CI runners are noisier than that (other jobs on the host, disk
throughput that varies), so the time limit leaves a wide margin: +25 % is six times the
spread measured here, and a real regression of that size is worth a look anyway. Peak RSS
moves less than time but still depends on how threads interleave: +20 %. The DB size
does not move by itself, so +10 % is a decision: an index that grows by a tenth should say
why.

## Corpora

- **This repository** (the head's tree, with its git history), about 1,000 files of Rust,
  TypeScript and fixtures in many languages: small, and it covers every language pack.
- **[Apache DolphinScheduler](https://github.com/apache/dolphinscheduler) 3.4.3**, pinned
  to commit `a190201acffa03d199d4ca216288734a6513de3d` and fetched at depth 1 in the job:
  about 4,900 files, 2,550 of them Java (Spring Boot) and 580 TypeScript (a Vue UI that
  calls the server's REST routes). It is the stack cartograph's bridges, Spring and
  contract passes are built for, large enough that time and memory dominate process
  start, and small enough to keep the job short. Apache-2.0; it is only read.

To move the pinned corpus, change `CORPUS_SHA` (and the comment) in the workflow; the base
and the head always index the same tree, so the numbers of one run stay comparable.

## Cost

A release build takes a few minutes, so the job keeps it small:

- `Swatinem/rust-cache` keeps the compiled dependencies;
- base and head share one target dir, so the base build only recompiles the workspace
  crates;
- on a push to main the head binary is saved in the Actions cache, keyed by commit and
  compiler version; a pull request whose base is that commit restores it and builds only
  its own head.

With both caches warm, the job builds the head once, then indexes each corpus 13 times
(one warm-up, 6 cold, 6 warm) and runs each query 6 times.

## Running it locally

Build the two binaries (for example the base from a `git worktree` of `main`), then:

```sh
node scripts/perf-gate.mjs --base <base binary> --head <head binary> \
  --corpus self=. --corpus other=<path to another repo> \
  --query "self:find Store" --query "self:callers Store.open" --query "self:findings"
```

Options: `--runs N` (default 3), `--work <dir>` to keep the data dirs, `--json <file>`,
`--max-time`, `--max-rss`, `--max-size`, `--max-warm`, `--max-query` (percentages; 0 reports
without gating) and `--min-time-delta <seconds>`. Exit 0 within the limits, 1 past one, 2
when a binary fails to run. On Linux peak RSS comes from `/usr/bin/time -v` (install the
`time` package if it is missing), on macOS from `/usr/bin/time -l`, on Windows from the
peak working set of the process. A corpus can be a workspace (a folder with a
`cartograph-workspace.toml`).

This repo on that desktop, `main` before the trim below against the branch with it:

| corpus | metric | base | head | change |
|---|---|---:|---:|---:|
| self | cold index (median) | 1.84 s | 1.76 s | -4.5 % |
| self | peak RSS (median) | 89.4 MiB | 89.1 MiB | -0.3 % |
| self | DB size | 10.5 MiB | 9.0 MiB | -14.2 % |
| self | warm index (median) | 0.55 s | 0.51 s | -7.1 % |
| self | queries (sum of medians) | 0.27 s | 0.26 s | -1.8 % |

## What the index stores

Schema v16 dropped what no answer reads:

- six secondary indexes (`nodes.kind`, `edges.dst`, `import_sites.file` and `.target`,
  `unresolved.file` and `.reason`): every reader loads those tables whole, so the indexes
  only cost space and write time;
- the rowid copy of `edges`: the table is now `WITHOUT ROWID`, its primary key the only
  B-tree;
- the comment rows of `key_sites`: a comment that names a string key now keeps only
  `(key, file)` in `key_mentions`, which is all a key trace reads of it (the files it lists
  as naming the key, and the files where no container is said to miss it). A migrated
  database moves its comment rows there.

The extraction cache also carries a 16-character digest of what extraction depends on
instead of the full list of grammar versions on every row (the list itself is
`imports::extractor_descriptor`).

On this repo the vacuumed database went from 10.3 MiB to 8.8 MiB. The largest part left is
the extraction cache (about half on this repo), which is what makes a warm index several
times faster than a cold one; compressing it would need a new dependency and was left out.
