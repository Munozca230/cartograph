//! `cartograph query`, `check` and `infer`: read-only answers from an existing index.

use crate::DataDirArg;
use anyhow::{Context, Result, bail};
use cartograph_core::{EntryKind, Store};
use cartograph_query::{Answer, Config, QueryError, Repo, deps, find, list, symbols};
use clap::{Args, Subcommand};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Options every query shares.
#[derive(Args)]
pub struct QueryOpts {
    /// The indexed repository.
    #[arg(
        long = "repo",
        short = 'C',
        value_name = "PATH",
        default_value = ".",
        global = true
    )]
    pub repo: PathBuf,
    /// Keep the database in DIR (as DIR/graph.db) instead of <REPO>/.cartograph.
    #[arg(long, value_name = "DIR", global = true)]
    pub data_dir: Option<PathBuf>,
    /// Rules file to use instead of <repo>/cartograph.toml.
    #[arg(long, value_name = "FILE", global = true)]
    pub config: Option<PathBuf>,
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    pub json: bool,
    /// In a workspace, look for plain paths in this member first.
    #[arg(long, value_name = "NAME", global = true)]
    pub member: Option<String>,
}

#[derive(Subcommand)]
pub enum Tool {
    /// Folder tree with files, LOC and kind; entry points; hubs; libraries; findings.
    Overview {
        /// Folder to start from (default: the repo root).
        path: Option<String>,
        /// Folder levels to show (1-4).
        #[arg(long, default_value_t = 1)]
        depth: u32,
    },
    /// One file or folder: role, imports, importers, libraries, findings.
    Explain { node: String },
    /// Dependency tree of a node.
    Deps {
        node: String,
        /// out (what it uses), in (what uses it) or both.
        #[arg(long, default_value = "out")]
        dir: String,
        /// 1 to 3.
        #[arg(long, default_value_t = 1)]
        depth: u8,
        /// file or folder.
        #[arg(long, default_value = "file")]
        level: String,
    },
    /// What breaks if these change, and which tests to run.
    Impact {
        #[arg(required = true)]
        nodes: Vec<String>,
    },
    /// Fuzzy search over folders, files and libraries (or symbols with --kind sym).
    Find {
        #[arg(required = true)]
        query: Vec<String>,
        /// file, dir, ext or sym.
        #[arg(long)]
        kind: Option<String>,
    },
    /// Functions, classes, methods and types of a file or folder, with call counts.
    Symbols { node: String },
    /// Who calls a symbol: `sym:<path>#<name>`, `<path>#<name>`, `Class.method` or a name.
    Callers { symbol: String },
    /// Supertypes and direct subtypes of a class, struct, interface or trait (or of a
    /// method's type): `sym:<path>#<name>`, `<path>#<name>`, `Class.method` or a name.
    Hierarchy { symbol: String },
    /// Where flows start: main, HTTP routes, CLI commands, IPC handlers, Godot main scenes
    /// and autoloads.
    Entries {
        /// main, http, cli, ipc, godot_main_scene or godot_autoload.
        #[arg(long)]
        kind: Option<String>,
        /// Only entries in this file or folder.
        #[arg(long)]
        within: Option<String>,
        /// Also list entries in tests and test, fixture or example trees.
        #[arg(long)]
        include_tests: bool,
    },
    /// Why A depends on B: the shortest dependency path and alternatives.
    Path { from: String, to: String },
    /// Code files that change most and are most nested (git churn × indentation complexity).
    Hotspots {
        /// Only files in this folder (or this file).
        #[arg(long)]
        within: Option<String>,
    },
    /// Cycles, layer violations, god modules, bottlenecks, orphans, hidden coupling…
    Findings {
        /// Only these kinds (repeatable): cycle, folder_cycle, layer_violation, fan_out_limit,
        /// god_module, hub, sdp, orphan, deep_chain, hidden_coupling.
        #[arg(long)]
        kind: Vec<String>,
        /// At least this severity: error, warning or info.
        #[arg(long)]
        severity: Option<String>,
        /// Only findings touching this file or anything under this folder.
        #[arg(long)]
        within: Option<String>,
        /// Only rule violations.
        #[arg(long)]
        violations: bool,
    },
}

/// The database path, or a clear error saying how to create it.
pub fn require_index(repo: &Path, data: &DataDirArg) -> Result<PathBuf> {
    let db = data.db(repo);
    if !db.is_file() {
        let flag = data
            .data_dir
            .as_ref()
            .map(|d| format!(" --data-dir {}", d.display()))
            .unwrap_or_default();
        bail!(
            "no index at {}; run `cartograph index {}{flag}` first",
            db.display(),
            repo.display()
        );
    }
    Ok(db)
}

/// Open the index and the config; a clear message when there is no index. With `strict`
/// (`check`), an invalid config is an error; otherwise (`query`) it is a warning on stderr
/// and the defaults apply, so `find` or `deps` keep working while the file is being fixed.
pub fn open_repo(
    repo: &Path,
    data: &DataDirArg,
    config: Option<&Path>,
    strict: bool,
) -> Result<Repo> {
    Ok(open_repo_hashed(repo, data, config, strict)?.0)
}

/// [`open_repo`] plus the content hashes of the files (for the notes of `explain`).
pub fn open_repo_hashed(
    repo: &Path,
    data: &DataDirArg,
    config: Option<&Path>,
    strict: bool,
) -> Result<(Repo, cartograph_mcp::annotations::FileHashes)> {
    let db = require_index(repo, data)?;
    let store = Store::open(&db).with_context(|| format!("opening {}", db.display()))?;
    let (graph, symbols) = store.load_graph_and_symbols()?;
    let history = store.load_history()?;
    let hashes = cartograph_mcp::annotations::file_hashes(&graph);
    let cfg = match config {
        Some(p) => Config::load_file(p, false),
        None => Config::load(repo),
    };
    let cfg = match cfg {
        Ok(c) => c,
        Err(e) if strict => bail!("invalid config: {e}"),
        Err(e) => {
            eprintln!("warning: invalid config, using the defaults: {e}");
            Config::default()
        }
    };
    Ok((
        Repo::with_symbols(&graph, &symbols, cfg).with_history(&history),
        hashes,
    ))
}

fn emit<T: Serialize>(answer: &Answer<T>, json: bool) -> Result<()> {
    let mut out = std::io::stdout().lock();
    if json {
        serde_json::to_writer(&mut out, &answer.data)?;
        out.write_all(b"\n")?;
    } else {
        out.write_all(answer.text.as_bytes())?;
    }
    out.flush()?;
    Ok(())
}

fn query_error(e: &QueryError, json: bool) -> Result<ExitCode> {
    if json {
        let (code, suggestions) = match e {
            QueryError::UnknownNode { suggestions, .. } => ("unknown_node", suggestions.clone()),
            QueryError::BadArgument(_) => ("bad_argument", Vec::new()),
            QueryError::Ambiguous { candidates, .. }
            | QueryError::AmbiguousPath { candidates, .. } => ("ambiguous", candidates.clone()),
        };
        let body = serde_json::json!({
            "error": { "code": code, "message": e.to_string(), "suggestions": suggestions }
        });
        println!("{body}");
    } else {
        eprintln!("error: {e}");
    }
    Ok(ExitCode::FAILURE)
}

pub fn cmd_query(tool: Tool, opts: &QueryOpts) -> Result<ExitCode> {
    let data = DataDirArg {
        data_dir: opts.data_dir.clone(),
    };
    let (mut repo, hashes) = open_repo_hashed(&opts.repo, &data, opts.config.as_deref(), false)?;
    if let Some(m) = &opts.member {
        if !repo.graph.members.names.contains(m) {
            bail!(
                "--member {m}: not a member of this index (members: {})",
                repo.graph.members.names.join(", ")
            );
        }
        repo.set_default_member(Some(m.clone()));
    }
    let json = opts.json;
    macro_rules! run {
        ($e:expr) => {
            match $e {
                Ok(a) => emit(&a, json).map(|_| ExitCode::SUCCESS),
                Err(e) => query_error(&e, json),
            }
        };
    }
    match tool {
        Tool::Overview { path, depth } => {
            run!(cartograph_query::overview::overview(
                &repo,
                path.as_deref(),
                depth
            ))
        }
        Tool::Explain { node } => {
            use cartograph_mcp::annotations;
            let (notes, warnings) = annotations::load_all(&data.dir(&opts.repo), &opts.repo);
            for e in warnings {
                eprintln!("warning: notes ignored: {e}");
            }
            run!(cartograph_query::explain::explain_with(
                &repo,
                &node,
                |id| { annotations::annotation(&notes, &hashes, id) }
            ))
        }
        Tool::Deps {
            node,
            dir,
            depth,
            level,
        } => {
            let args = (|| -> Result<_, QueryError> {
                Ok((
                    dir.parse::<deps::Direction>()?,
                    level.parse::<deps::Level>()?,
                ))
            })();
            match args {
                Ok((d, l)) => run!(deps::deps(&repo, &node, d, depth, l)),
                Err(e) => query_error(&e, json),
            }
        }
        Tool::Impact { nodes } => run!(cartograph_query::impact::impact(&repo, &nodes)),
        Tool::Find { query, kind } => {
            let kind = match kind.map(|k| k.parse::<find::FindKind>()).transpose() {
                Ok(k) => k,
                Err(e) => return query_error(&e, json),
            };
            run!(find::find(&repo, &query.join(" "), kind))
        }
        Tool::Symbols { node } => run!(symbols::symbols(&repo, &node)),
        Tool::Callers { symbol } => run!(symbols::callers(&repo, &symbol)),
        Tool::Hierarchy { symbol } => {
            run!(cartograph_query::hierarchy::hierarchy(&repo, &symbol))
        }
        Tool::Entries {
            kind,
            within,
            include_tests,
        } => {
            let kind = match kind.map(|k| k.parse::<EntryKind>()) {
                None => None,
                Some(Ok(k)) => Some(k),
                Some(Err(_)) => {
                    return query_error(
                        &QueryError::BadArgument(
                            "kind must be main, http, cli, ipc, godot_main_scene or godot_autoload"
                                .into(),
                        ),
                        json,
                    );
                }
            };
            run!(symbols::entries(
                &repo,
                kind,
                within.as_deref(),
                include_tests
            ))
        }
        Tool::Path { from, to } => run!(cartograph_query::path::path(&repo, &from, &to)),
        Tool::Hotspots { within } => run!(cartograph_query::hotspots::hotspots(
            &repo,
            within.as_deref()
        )),
        Tool::Findings {
            kind,
            severity,
            within,
            violations,
        } => {
            let mut filter = list::Filter {
                within,
                violations,
                ..list::Filter::default()
            };
            for k in kind.iter().flat_map(|k| k.split(',')) {
                match k.parse() {
                    Ok(k) => filter.kinds.push(k),
                    Err(e) => return query_error(&QueryError::BadArgument(e), json),
                }
            }
            if let Some(s) = severity {
                match s.parse() {
                    Ok(s) => filter.severity = Some(s),
                    Err(e) => return query_error(&QueryError::BadArgument(e), json),
                }
            }
            run!(list::findings(&repo, &filter))
        }
    }
}

/// `check`: exit 0 when no rule is broken, 1 when one is, 2 when the check cannot run.
pub fn cmd_check(path: &Path, data: &DataDirArg, config: Option<&Path>, json: bool) -> ExitCode {
    let repo = match open_repo(path, data, config, true) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::from(2);
        }
    };
    let answer = cartograph_query::check::check(&repo);
    if let Err(e) = emit(&answer, json) {
        eprintln!("error: {e:#}");
        return ExitCode::from(2);
    }
    if answer.data.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Both sides of a comparison with a git base: the working tree and the base, with the
/// same config and history.
struct Sides {
    head: Repo,
    base: cartograph_mcp::base::Base,
}

/// Both sides, built in memory: nothing is written to the store, so this never races with
/// an `index` or an MCP server writing the same data dir. The working tree is
/// indexed as the base is (no symbols: the comparison does not use them), both with the
/// extraction cache of the stored index, if any, plus what the working tree's build
/// parsed, so a file is parsed at most once.
/// Both use the size limit given, else the one the stored index was built with, else the
/// default; the git history (for hidden coupling) is the stored one, or none, on both sides.
fn open_sides(
    path: &Path,
    data: &DataDirArg,
    config: Option<&Path>,
    rev: Option<&str>,
    max_file_bytes: Option<u64>,
) -> Result<Sides> {
    let data_dir = data.dir(path);
    let limit = max_file_bytes
        .or_else(|| cartograph_index::stored_max_file_bytes(&data_dir))
        .unwrap_or(cartograph_index::walk::DEFAULT_MAX_FILE_BYTES);
    let opts = cartograph_index::IndexOptions {
        max_file_bytes: limit,
        data_dir: data.data_dir.clone(),
        ..cartograph_index::IndexOptions::default()
    };
    let (head, cache) = cartograph_index::index_working_tree(path, &opts)
        .with_context(|| format!("indexing {}", path.display()))?;
    // The history is optional (hidden coupling only): an unreadable index costs only that.
    let db = data.db(path);
    let history = if db.is_file() {
        let read = || -> cartograph_core::Result<cartograph_core::History> {
            Store::open(&db)?.load_history()
        };
        read().unwrap_or_else(|e| {
            eprintln!(
                "warning: git history not read from {}, compared without it: {e}",
                db.display()
            );
            cartograph_core::History::default()
        })
    } else {
        cartograph_core::History::default()
    };
    let cfg = match config {
        Some(p) => Config::load_file(p, false),
        None => Config::load(path),
    }
    .map_err(|e| anyhow::anyhow!("invalid config: {e}"))?;
    let base = cartograph_mcp::base::open(path, rev, &opts, &cfg, &history, Some(&cache))
        .map_err(|e| anyhow::anyhow!(e))?;
    if let Some(w) = cartograph_mcp::base::write_warning(&base.snapshot) {
        eprintln!("{w}");
    }
    Ok(Sides {
        head: Repo::new(&head.graph, cfg).with_history(&history),
        base,
    })
}

/// `check --base REV`: exit 1 only on violations the working tree introduces, 2 when the
/// check cannot run (no repository, unknown revision, invalid config).
pub fn cmd_check_base(
    path: &Path,
    data: &DataDirArg,
    config: Option<&Path>,
    rev: &str,
    max_file_bytes: Option<u64>,
    json: bool,
) -> ExitCode {
    let sides = match open_sides(path, data, config, Some(rev), max_file_bytes) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::from(2);
        }
    };
    let answer = cartograph_query::check::check_base(
        &sides.head,
        &sides.base.repo,
        &sides.base.revision.label(),
    );
    if let Err(e) = emit(&answer, json) {
        eprintln!("error: {e:#}");
        return ExitCode::from(2);
    }
    if answer.data.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// `diff [REV]`: exit 0 with the diff, 2 when it cannot run.
pub fn cmd_diff(
    path: &Path,
    data: &DataDirArg,
    config: Option<&Path>,
    rev: Option<&str>,
    max_file_bytes: Option<u64>,
    json: bool,
) -> ExitCode {
    let sides = match open_sides(path, data, config, rev, max_file_bytes) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::from(2);
        }
    };
    let answer = cartograph_query::diff::diff(
        &sides.base.repo,
        &sides.head,
        &sides.base.revision.label(),
        "working tree",
    );
    match emit(&answer, json) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

/// `infer`: propose layers; write `cartograph.toml` only with `--write`.
pub fn cmd_infer(
    path: &Path,
    data: &DataDirArg,
    write: bool,
    force: bool,
    json: bool,
) -> Result<ExitCode> {
    // The rest of the existing config is what is being replaced (maybe broken); only its
    // `ignore` counts. When that `ignore` cannot be read (the file is unreadable, not TOML,
    // or its `ignore` is invalid), writing would drop it: refuse, as `check` does when it
    // cannot run. Without `--write` it is a warning, and nothing is ignored.
    let cfg = match Config::load_ignore(path) {
        Ok(cfg) => cfg,
        Err(e) if write => {
            eprintln!(
                "error: cannot read ignore from the existing config, not overwriting it: {e}\n\
                 fix it, or move it away, and run infer --write again"
            );
            return Ok(ExitCode::from(2));
        }
        Err(e) => {
            eprintln!("warning: cannot read ignore from the config, ignoring nothing: {e}");
            Config::default()
        }
    };
    let repo = open_repo_for_infer(path, data, cfg)?;
    let answer = cartograph_query::infer::infer(&repo);
    if write {
        let target = path.join(cartograph_query::config::CONFIG_FILE);
        if target.exists() && !force {
            bail!(
                "{} already exists; pass --force to overwrite it",
                target.display()
            );
        }
        std::fs::write(&target, &answer.data.toml)
            .with_context(|| format!("writing {}", target.display()))?;
        eprintln!("wrote {}", target.display());
    }
    emit(&answer, json)?;
    Ok(ExitCode::SUCCESS)
}

/// The index with `cfg`, the `ignore` of the existing config.
fn open_repo_for_infer(path: &Path, data: &DataDirArg, cfg: Config) -> Result<Repo> {
    let db = require_index(path, data)?;
    let store = Store::open(&db).with_context(|| format!("opening {}", db.display()))?;
    Ok(Repo::new(&store.load_graph()?, cfg))
}
