//! `cartograph` command-line interface.
//!
//! Command output goes to stdout; logs go to stderr only (a future `cartograph mcp`
//! stdio server shares the process, and stray stdout output would break the protocol).

use anyhow::{Context, Result, bail};
use cartograph_core::Store;
use cartograph_index::grammars::{self, Grammar};
use cartograph_index::{DB_FILE, IndexOptions};
use clap::{Parser, Subcommand};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod eval;
mod query;

#[derive(Parser)]
#[command(
    name = "cartograph",
    version,
    about = "Architecture map of a code repository"
)]
struct Cli {
    /// More logging on stderr (-v info, -vv debug). `RUST_LOG` overrides it.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Index a repository into <PATH>/.cartograph/graph.db (or <DATA_DIR>/graph.db).
    Index {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Skip files larger than this many bytes, for this run only. Default: the limit the
        /// index was built with, else 1 MiB.
        #[arg(long)]
        max_file_bytes: Option<u64>,
        #[command(flatten)]
        data: DataDirArg,
        /// Skip the git history (churn, owners, co-change, hidden coupling).
        #[arg(long)]
        no_git: bool,
        /// Import this SCIP index for exact definitions and calls (repeatable). Remembered
        /// for later re-indexes; `<PATH>/index.scip` is always picked up. See docs/scip.md.
        #[arg(long, value_name = "FILE.scip", conflicts_with = "no_scip")]
        scip: Vec<PathBuf>,
        /// Import no SCIP index (not even `<PATH>/index.scip`) and forget remembered ones.
        #[arg(long)]
        no_scip: bool,
    },
    /// Print a Markdown report of an indexed repository.
    Report {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[command(flatten)]
        data: DataDirArg,
    },
    /// Serve the map and its JSON API on 127.0.0.1, re-indexing as files change.
    Serve {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[command(flatten)]
        data: DataDirArg,
        /// Port to listen on. Without it, 7878 or the first free one of the next 10.
        #[arg(long)]
        port: Option<u16>,
        /// Open the map in the default browser.
        #[arg(long)]
        open: bool,
        /// Do not watch the repo for changes (index once at startup).
        #[arg(long)]
        no_watch: bool,
        /// Skip the git history (churn, owners, hidden coupling).
        #[arg(long)]
        no_git: bool,
        /// Skip files larger than this many bytes, for this run only. Default: the limit the
        /// index was built with, else 1 MiB.
        #[arg(long)]
        max_file_bytes: Option<u64>,
    },
    /// Ask an existing index (text by default, JSON with --json).
    Query {
        #[command(subcommand)]
        tool: query::Tool,
        #[command(flatten)]
        opts: query::QueryOpts,
    },
    /// Check the rules of cartograph.toml: exit 1 on a violation (layers, cycles in
    /// `no_cycles` scopes, `max_fan_out`), 0 otherwise, 2 if the check cannot run.
    Check {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[command(flatten)]
        data: DataDirArg,
        /// Rules file to use instead of <PATH>/cartograph.toml.
        #[arg(long, value_name = "FILE")]
        config: Option<PathBuf>,
        /// Fail only on violations the working tree introduces relative to this git
        /// revision (commit, branch, tag); the others are reported as existing. Both sides
        /// are indexed in memory; the stored index is not written.
        #[arg(long, value_name = "REV", value_parser = revision)]
        base: Option<String>,
        /// With --base: skip files larger than this many bytes, for this run only. Default:
        /// the limit the index was built with, else 1 MiB.
        #[arg(long)]
        max_file_bytes: Option<u64>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// What the working tree (HEAD plus uncommitted changes) changes in the architecture
    /// relative to a git revision: findings, dependencies between top folders or layers,
    /// unresolved imports, files. Both sides are indexed in memory (the stored index is not
    /// written); the base is built without touching the repo and cached by commit.
    Diff {
        /// Commit, branch or tag. Default: the merge-base of HEAD with the default branch.
        #[arg(value_parser = revision)]
        base: Option<String>,
        /// The repository.
        #[arg(long = "repo", short = 'C', value_name = "PATH", default_value = ".")]
        path: PathBuf,
        #[command(flatten)]
        data: DataDirArg,
        /// Rules file to use instead of <PATH>/cartograph.toml.
        #[arg(long, value_name = "FILE")]
        config: Option<PathBuf>,
        /// Skip files larger than this many bytes, for this run only. Default: the limit the
        /// index was built with, else 1 MiB.
        #[arg(long)]
        max_file_bytes: Option<u64>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Propose layers from the dependency graph and print a suggested cartograph.toml.
    Infer {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[command(flatten)]
        data: DataDirArg,
        /// Write <PATH>/cartograph.toml (refuses to overwrite one without --force).
        #[arg(long)]
        write: bool,
        /// With --write, overwrite an existing cartograph.toml. Its `ignore` is kept; when
        /// that cannot be read (not TOML, or an invalid `ignore`), nothing is written (exit 2).
        #[arg(long, requires = "write")]
        force: bool,
        /// Print JSON instead of the TOML text.
        #[arg(long)]
        json: bool,
    },
    /// Run the MCP server on stdio (for any MCP client or coding agent). stdout carries only
    /// the protocol; logs go to stderr. Indexes in the background and re-indexes on changes.
    Mcp {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[command(flatten)]
        data: DataDirArg,
        /// Rules file to use instead of <PATH>/cartograph.toml.
        #[arg(long, value_name = "FILE")]
        config: Option<PathBuf>,
        /// Do not watch the repo (freshness then relies on per-request checks of the files
        /// each request names).
        #[arg(long)]
        no_watch: bool,
        /// Also send each answer as JSON `structuredContent` (some clients show only the
        /// structured JSON to the model instead of the compact text, so this is off by default).
        #[arg(long)]
        structured: bool,
        /// Longest a tool may run, in seconds.
        #[arg(long, default_value_t = 30)]
        tool_timeout: u64,
        /// Longest a request waits for a pending re-index, in milliseconds.
        #[arg(long, default_value_t = 8000)]
        fresh_wait_ms: u64,
        /// Skip files larger than this many bytes, for this run only. Default: the limit the
        /// index was built with, else 1 MiB.
        #[arg(long)]
        max_file_bytes: Option<u64>,
        /// Do not record tool calls in <data dir>/stats.jsonl (also: CARTOGRAPH_NO_STATS=1).
        #[arg(long)]
        no_stats: bool,
    },
    /// Usage of the MCP tools in a repo: calls per tool, latency, errors, answer
    /// size, truncation (from <data dir>/stats.jsonl; no arguments or paths are recorded).
    Stats {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[command(flatten)]
        data: DataDirArg,
        /// Only the last N days.
        #[arg(long, value_name = "N")]
        days: Option<u64>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Run an eval suite: agent questions as MCP tool calls against real repos, each checked
    /// for facts the answer must (not) contain and its token budget. Exit 1 if a case fails.
    Eval {
        /// The cases file (TOML; see docs/mcp.md).
        #[arg(default_value = "evals/cases.toml")]
        cases: PathBuf,
        /// Only cases whose id contains this.
        #[arg(long)]
        filter: Option<String>,
        /// Print the answer of each failing case.
        #[arg(long, short = 'a')]
        answers: bool,
        /// Print the answer of every case.
        #[arg(long, short = 'A')]
        all_answers: bool,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Check that cartograph can run here: database, grammars, write access.
    Doctor {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[command(flatten)]
        data: DataDirArg,
    },
}

/// A git revision argument: anything but empty (an empty `--base` is a mistake, not the
/// default base).
fn revision(s: &str) -> Result<String, String> {
    match s.trim() {
        "" => Err("the revision is empty".into()),
        r => Ok(r.to_string()),
    }
}

#[derive(clap::Args)]
struct DataDirArg {
    /// Keep the database in DIR (as DIR/graph.db) instead of <PATH>/.cartograph. Nothing is
    /// written inside the analyzed repo when this is given.
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
}

impl DataDirArg {
    fn dir(&self, repo: &Path) -> PathBuf {
        cartograph_index::data_dir(repo, self.data_dir.as_deref())
    }

    fn db(&self, repo: &Path) -> PathBuf {
        self.dir(repo).join(DB_FILE)
    }
}

fn init_logging(verbose: u8) {
    use tracing_subscriber::EnvFilter;
    let default = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    // No colour codes when stderr is a pipe (an MCP client's log).
    use std::io::IsTerminal;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_target(false)
        .init();
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    let result = match cli.command {
        Command::Index {
            path,
            max_file_bytes,
            data,
            no_git,
            scip,
            no_scip,
        } => cmd_index(
            &path,
            size_limit(&path, &data, max_file_bytes),
            &data,
            !no_git,
            (scip, no_scip),
        ),
        Command::Report { path, data } => cmd_report(&path, &data),
        Command::Doctor { path, data } => cmd_doctor(&path, &data),
        Command::Mcp {
            path,
            data,
            config,
            no_watch,
            structured,
            tool_timeout,
            fresh_wait_ms,
            max_file_bytes,
            no_stats,
        } => {
            let opts = cartograph_mcp::Options {
                stats: !no_stats && std::env::var_os("CARTOGRAPH_NO_STATS").is_none(),
                max_file_bytes: size_limit(&path, &data, max_file_bytes),
                data_dir: data.data_dir,
                config,
                watch: !no_watch,
                structured,
                tool_timeout: std::time::Duration::from_secs(tool_timeout.max(1)),
                fresh_wait: std::time::Duration::from_millis(fresh_wait_ms),
                ..cartograph_mcp::Options::new(path)
            };
            cartograph_mcp::run_stdio(opts)
                .map(|()| ExitCode::SUCCESS)
                .map_err(|e| anyhow::anyhow!(e))
        }
        Command::Eval {
            cases,
            filter,
            answers,
            all_answers,
            json,
        } => eval::cmd_eval(&eval::EvalOpts {
            cases,
            filter,
            json,
            answers: match (all_answers, answers) {
                (true, _) => eval::Answers::All,
                (false, true) => eval::Answers::Failing,
                (false, false) => eval::Answers::None,
            },
        }),
        Command::Stats {
            path,
            data,
            days,
            json,
        } => cmd_stats(&path, &data, days, json),
        Command::Query { tool, opts } => query::cmd_query(tool, &opts),
        Command::Check {
            path,
            data,
            config,
            base,
            max_file_bytes,
            json,
        } => Ok(match base {
            None => query::cmd_check(&path, &data, config.as_deref(), json),
            Some(rev) => {
                query::cmd_check_base(&path, &data, config.as_deref(), &rev, max_file_bytes, json)
            }
        }),
        Command::Diff {
            base,
            path,
            data,
            config,
            max_file_bytes,
            json,
        } => Ok(query::cmd_diff(
            &path,
            &data,
            config.as_deref(),
            base.as_deref(),
            max_file_bytes,
            json,
        )),
        Command::Infer {
            path,
            data,
            write,
            force,
            json,
        } => query::cmd_infer(&path, &data, write, force, json),
        Command::Serve {
            path,
            data,
            port,
            open,
            no_watch,
            no_git,
            max_file_bytes,
        } => {
            let limit = size_limit(&path, &data, max_file_bytes);
            cmd_serve(path, data, port, open, !no_watch, !no_git, limit)
        }
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The size limit to index with: `--max-file-bytes` when given, else the limit the index in
/// the data dir was built with (so a re-index never undoes a custom limit), else 1 MiB.
fn size_limit(path: &Path, data: &DataDirArg, flag: Option<u64>) -> u64 {
    flag.or_else(|| cartograph_index::stored_max_file_bytes(&data.dir(path)))
        .unwrap_or(cartograph_index::walk::DEFAULT_MAX_FILE_BYTES)
}

fn cmd_index(
    path: &Path,
    max_file_bytes: u64,
    data: &DataDirArg,
    git: bool,
    (scip, no_scip): (Vec<PathBuf>, bool),
) -> Result<ExitCode> {
    for f in &scip {
        if !f.is_file() {
            bail!("--scip {}: no such file", f.display());
        }
    }
    let opts = IndexOptions {
        max_file_bytes,
        data_dir: data.data_dir.clone(),
        scip,
        no_scip,
        ..IndexOptions::default()
    };
    let out = cartograph_index::index_to_store(path, &opts)
        .with_context(|| format!("indexing {}", path.display()))?;
    let g = &out.graph;
    println!(
        "indexed {} files, {} dirs, {} packages ({} skipped), {} imports ({} unresolved) in {} ms -> {}",
        g.nodes_of(cartograph_core::NodeKind::File).count(),
        g.nodes_of(cartograph_core::NodeKind::Dir).count(),
        g.nodes_of(cartograph_core::NodeKind::Package).count(),
        g.skipped.len(),
        g.imports.len() + g.unresolved.len(),
        g.unresolved.len(),
        out.timings.total_ms,
        data.db(path).display()
    );
    if let Some(st) = &out.symbols.scip.stats {
        for line in scip_lines(st) {
            println!("{line}");
        }
    }
    if git {
        // The history never fails the index: a problem is a warning.
        let db = data.db(path);
        let outcome = Store::open(&db)
            .map_err(anyhow::Error::from)
            .and_then(|mut s| Ok(cartograph_index::git::update(path, &mut s)?));
        match outcome {
            Ok(o) => println!("{}", git_line(&o)),
            Err(e) => eprintln!("warning: git history not indexed: {e:#}"),
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// What the SCIP import did: one line per index read, then the totals.
fn scip_lines(st: &cartograph_core::ScipStats) -> Vec<String> {
    let mut out = Vec::new();
    for ix in &st.indexes {
        if let Some(e) = &ix.error {
            out.push(format!("warning: scip {} not imported: {e}", ix.path));
            continue;
        }
        let mut l = format!(
            "scip {}: {} documents, {} match files{}{}",
            ix.path,
            ix.documents,
            ix.matched,
            if ix.prefix.is_empty() {
                String::new()
            } else {
                format!(" under {}/", ix.prefix)
            },
            if ix.cached { " (cached)" } else { "" }
        );
        if ix.unmatched > 0 {
            l.push_str(&format!(", {} match none", ix.unmatched));
            if let Some(p) = ix.unmatched_sample.first() {
                l.push_str(&format!(" (e.g. {p})"));
            }
        }
        if ix.stale > 0 {
            l.push_str(&format!(
                ", {} changed since it was written (left to tree-sitter; regenerate it)",
                ix.stale
            ));
        }
        out.push(l);
    }
    if st.files_covered > 0 {
        out.push(format!(
            "scip: {} files precise · symbols {} matched, {} added · calls {} confirmed ({} upgraded), {} added, {} dropped · {} file and {} library dependencies added",
            st.files_covered,
            st.symbols_matched,
            st.symbols_added,
            st.calls_confirmed,
            st.calls_upgraded,
            st.calls_added,
            st.calls_dropped,
            st.deps_added,
            st.externals_added
        ));
    }
    out
}

/// One line about the git pass.
fn git_line(o: &cartograph_index::git::GitOutcome) -> String {
    use cartograph_core::GitState;
    let s = &o.status;
    match s.state {
        GitState::NoGit => "git: not a git repository (no history)".into(),
        GitState::NoCommits => "git: no commits yet".into(),
        GitState::Error => format!(
            "git: history not read: {}",
            s.error.as_deref().unwrap_or("unknown error")
        ),
        GitState::Ok => {
            let partial = if s.shallow {
                " · shallow clone: partial history"
            } else {
                ""
            };
            // Only a pass that read commits has its own mode, count and time.
            let what = if o.read {
                format!("{} pass, {} commits read", s.mode, s.scanned)
            } else {
                "no new commits".to_string()
            };
            format!(
                "git: {what}; {} commits ({} merges), {} authors{partial}{}",
                s.commits,
                s.merges,
                s.authors,
                if o.read {
                    format!(" in {} ms", s.ms)
                } else {
                    String::new()
                }
            )
        }
    }
}

fn cmd_serve(
    path: PathBuf,
    data: DataDirArg,
    port: Option<u16>,
    open: bool,
    watch: bool,
    git: bool,
    max_file_bytes: u64,
) -> Result<ExitCode> {
    let config = cartograph_server::ServeConfig {
        data_dir: data.data_dir,
        port,
        watch,
        git,
        max_file_bytes,
        ..cartograph_server::ServeConfig::new(path.clone())
    };
    cartograph_server::run_blocking(config, |addr| {
        let url = format!("http://{addr}/");
        // The URL goes to stderr with the logs; stdout stays free for machine output.
        eprintln!(
            "cartograph serving {} at {url} (Ctrl-C to stop)",
            path.display()
        );
        if open && let Err(e) = open::that_detached(&url) {
            eprintln!("warning: could not open a browser: {e}");
        }
    })
    .with_context(|| format!("serving {}", path.display()))?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_stats(path: &Path, data: &DataDirArg, days: Option<u64>, json: bool) -> Result<ExitCode> {
    use cartograph_mcp::stats;
    let dir = data.dir(path);
    let (records, bad) = stats::load(&dir);
    let since = days.map(|d| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |t| t.as_secs());
        now.saturating_sub(d.saturating_mul(86_400))
    });
    let summary = stats::summarize(&records, bad, since);
    let mut out = std::io::stdout().lock();
    if json {
        serde_json::to_writer_pretty(&mut out, &summary)?;
        out.write_all(b"\n")?;
    } else {
        out.write_all(stats::render(&summary).as_bytes())?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_report(path: &Path, data: &DataDirArg) -> Result<ExitCode> {
    let db = data.db(path);
    if !db.is_file() {
        let flag = data
            .data_dir
            .as_ref()
            .map(|d| format!(" --data-dir {}", d.display()))
            .unwrap_or_default();
        bail!(
            "no index at {}; run `cartograph index {}{flag}` first",
            db.display(),
            path.display()
        );
    }
    let store = Store::open(&db).with_context(|| format!("opening {}", db.display()))?;
    let md = cartograph_index::report::render(&store)?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(md.as_bytes())?;
    stdout.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// One doctor check: a name and its outcome.
struct Check {
    name: &'static str,
    outcome: Result<String>,
}

fn cmd_doctor(path: &Path, data: &DataDirArg) -> Result<ExitCode> {
    let data_dir = data.dir(path);
    let is_dir = path.is_dir();
    let not_a_dir =
        || -> Result<String> { bail!("skipped: {} is not an existing directory", path.display()) };
    let checks = vec![
        Check {
            name: "repo path",
            outcome: if is_dir {
                Ok(path.display().to_string())
            } else {
                Err(anyhow::anyhow!(
                    "{} is not an existing directory",
                    path.display()
                ))
            },
        },
        Check {
            name: "binary",
            outcome: Ok(format!(
                "cartograph {} ({} {})",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH
            )),
        },
        Check {
            name: "data dir writable",
            outcome: if is_dir {
                check_writable(&data_dir)
            } else {
                not_a_dir()
            },
        },
        Check {
            name: "sqlite wal",
            outcome: if is_dir {
                check_wal(&data_dir)
            } else {
                not_a_dir()
            },
        },
        Check {
            name: "grammars",
            outcome: check_grammars(),
        },
        Check {
            name: "web ui",
            // Informational: a build without web/dist works, `serve` explains the rest.
            outcome: Ok(if cartograph_server::assets::embedded() {
                "embedded in this binary".to_string()
            } else {
                "not embedded (built without web/dist; `serve` shows a page on how to build it)"
                    .to_string()
            }),
        },
        Check {
            name: "existing index",
            outcome: check_existing_index(&data.db(path)),
        },
    ];
    let mut failed = 0;
    for c in &checks {
        match &c.outcome {
            Ok(msg) => println!("PASS  {:<18} {msg}", c.name),
            Err(e) => {
                failed += 1;
                println!("FAIL  {:<18} {e:#}", c.name);
            }
        }
    }
    // SCIP indexes are optional: a problem with one is a warning, never a failure.
    let mut warned = 0;
    if is_dir {
        for (ok, msg) in check_scip(path, &data_dir) {
            if !ok {
                warned += 1;
            }
            let status = if ok { "PASS" } else { "WARN" };
            println!("{status}  {:<18} {msg}", "scip index");
        }
    }
    let warnings = match warned {
        0 => String::new(),
        1 => " (1 warning)".to_string(),
        n => format!(" ({n} warnings)"),
    };
    if failed > 0 {
        println!("{failed} check(s) failed{warnings}");
        Ok(ExitCode::FAILURE)
    } else {
        println!("all checks passed{warnings}");
        Ok(ExitCode::SUCCESS)
    }
}

/// One `(ok, message)` per SCIP index an index of `path` would import: readable, how many
/// documents match files, and whether files it covers changed after it was written.
fn check_scip(path: &Path, data_dir: &Path) -> Vec<(bool, String)> {
    let found = cartograph_index::scip::health(path, data_dir);
    if found.is_empty() {
        return vec![(
            true,
            "none (optional: an index.scip at the repo root makes calls exact; see docs/scip.md)"
                .to_string(),
        )];
    }
    found
        .into_iter()
        .map(|h| match h.outcome {
            Err(e) => (false, format!("{} is unreadable: {e}", h.label)),
            Ok(ok) if ok.stale_total > 0 => (
                false,
                format!(
                    "{} is stale: older than {} of the {} files it covers (newest: {}); regenerate it",
                    h.label,
                    ok.stale_total,
                    ok.matched,
                    ok.stale.join(", ")
                ),
            ),
            Ok(ok) if ok.matched == 0 => (
                false,
                format!(
                    "{}: none of its {} documents names a file of this repo",
                    h.label, ok.documents
                ),
            ),
            Ok(ok) => (
                true,
                format!(
                    "{} ({}): {} documents, {} match files, up to date",
                    h.label,
                    if ok.tool.is_empty() {
                        "unknown tool"
                    } else {
                        ok.tool.as_str()
                    },
                    ok.documents,
                    ok.matched
                ),
            ),
        })
        .collect()
}

fn check_writable(dir: &Path) -> Result<String> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let probe = dir.join(".doctor-write-probe");
    std::fs::write(&probe, b"ok").with_context(|| format!("writing {}", probe.display()))?;
    std::fs::remove_file(&probe).with_context(|| format!("removing {}", probe.display()))?;
    Ok(dir.display().to_string())
}

fn check_wal(dir: &Path) -> Result<String> {
    std::fs::create_dir_all(dir)?;
    let db = dir.join("doctor-probe.db");
    let outcome = (|| -> Result<String> {
        let store = Store::open(&db)?;
        let mode = store.journal_mode()?;
        if !mode.eq_ignore_ascii_case("wal") {
            bail!("journal_mode is {mode}, expected wal");
        }
        // A second connection must be able to read while the first is open.
        let reader = Store::open(&db)?;
        reader.load_graph()?;
        Ok(format!(
            "journal_mode=wal, schema v{}, busy_timeout={} ms",
            store.schema_version()?,
            cartograph_core::BUSY_TIMEOUT.as_millis()
        ))
    })();
    for suffix in ["", "-wal", "-shm"] {
        let mut p = db.clone().into_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(p);
    }
    outcome
}

fn check_grammars() -> Result<String> {
    let mut failures = Vec::new();
    for g in Grammar::ALL {
        if let Err(e) = grammars::parse(g, b"") {
            failures.push(format!("{}: {e}", g.name()));
        }
    }
    if failures.is_empty() {
        Ok(format!(
            "{} grammars load (tree-sitter ABI {}..={})",
            Grammar::ALL.len(),
            grammars::MIN_COMPATIBLE_ABI,
            grammars::ABI
        ))
    } else {
        bail!(failures.join("; "))
    }
}

fn check_existing_index(db: &Path) -> Result<String> {
    if !db.is_file() {
        return Ok("none yet (run `cartograph index`)".into());
    }
    let store = Store::open(db).with_context(|| format!("opening {}", db.display()))?;
    let graph = store.load_graph()?;
    Ok(format!(
        "{} (schema v{}, {} nodes, {} skipped)",
        db.display(),
        store.schema_version()?,
        graph.nodes.len(),
        graph.skipped.len()
    ))
}
