//! The MCP tools. Each one is a thin wrapper over a `cartograph-query` function: parse
//! the arguments (enums arrive as strings and go through the query crate's `FromStr`, so a
//! bad value is a tool error with the accepted values, not a protocol error), then
//! [`Cartograph::run`] does the rest (freshness gate, guard, text + JSON result).
//!
//! Adding a tool is a few lines: an argument struct and a `#[tool]` method whose body is
//! `self.run("name", args_that_name_files, move |l| query(&l.repo, …)).await`.

use crate::annotations;
use crate::base;
use crate::engine::{Engine, Loaded, Ready};
use crate::guard::Guard;
use crate::map;
use cartograph_core::EntryKind;
use cartograph_query::{Answer, QueryError, deps, find, list, symbols};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

/// What every session sees before any tool is loaded (a client may defer tool schemas
/// behind a tool search; names and these instructions always load). Key words first.
pub const INSTRUCTIONS: &str = "\
cartograph: the architecture map of this repository (folders, files, imports, symbols, \
callers, entry points, architecture findings), kept fresh as files change. Use it INSTEAD \
of grep/glob/reading many files when the question is structural: where is X, what does \
this folder do, who imports or calls this, what breaks if I change this, how does A reach \
B, where does execution start, which tests cover this. One call returns a compact answer \
(at most ~1.5k tokens) that would otherwise take many reads; long lists end in \"+N more\".

Workflow: overview (the map) -> find / explain (locate, understand) -> impact before \
editing a file, callers before changing a function -> check before finishing \
(with base=main only what your change introduces fails; diff shows what it changes in \
the architecture). annotate leaves a note on a file or folder that explain shows in later sessions. \
To show the user the architecture or a flow (a diagram, the map), call open_map.

Ids: file:src/a.rs, dir:src, sym:src/a.rs#name, plain repo paths (src/a.rs, src/), \
absolute paths inside the repo, path#name, libraries as npm:react. An unknown id comes \
back with suggestions. Read the code itself only when you need the lines.";

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OverviewArgs {
    /// Folder to map (`src`, `dir:src`); empty or omitted = the whole repo.
    #[serde(default)]
    pub path: Option<String>,
    /// Folder levels to show, 1-4 (default 1).
    #[serde(default)]
    pub depth: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NodeArgs {
    /// A file or folder: `file:src/a.rs`, `dir:src`, `src/a.rs`, `src/`, or an absolute
    /// path inside the repo; libraries as `npm:react`.
    pub node: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DepsArgs {
    /// A file or folder id or path.
    pub node: String,
    /// `out` (what it uses, default), `in` (what uses it) or `both`.
    #[serde(default)]
    pub dir: Option<String>,
    /// 1-3 (default 1).
    #[serde(default)]
    pub depth: Option<u8>,
    /// `file` (default) or `folder`.
    #[serde(default)]
    pub level: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ImpactArgs {
    /// Files or folders about to change, e.g. `["src/model.rs"]`.
    pub nodes: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindArgs {
    /// Name or path fragment, e.g. `store`, `api/routes`, `parseConfig`.
    pub query: String,
    /// `file`, `dir`, `ext` (libraries) or `sym` (functions, classes, methods, types).
    /// Omitted = folders, files and libraries.
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PathArgs {
    /// The file or folder that depends.
    pub from: String,
    /// The file or folder it depends on.
    pub to: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CallersArgs {
    /// `sym:src/a.rs#name`, `src/a.rs#Type.method`, `Class.method` or a bare name.
    pub symbol: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EntriesArgs {
    /// `main`, `http`, `cli`, `ipc`, `godot_main_scene` or `godot_autoload`; omitted = all.
    #[serde(default)]
    pub kind: Option<String>,
    /// Only entries in this file or folder.
    #[serde(default)]
    pub within: Option<String>,
    /// Also list entries in tests and test, fixture or example trees (left out by default,
    /// like in `overview`, unless `within` is inside one).
    #[serde(default)]
    pub include_tests: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindingsArgs {
    /// Only these kinds: cycle, folder_cycle, layer_violation, fan_out_limit, god_module,
    /// hub, sdp, orphan, deep_chain, hidden_coupling.
    #[serde(default)]
    pub kind: Option<Vec<String>>,
    /// At least this severity: `error`, `warning` or `info`.
    #[serde(default)]
    pub severity: Option<String>,
    /// Only findings touching this file or anything under this folder.
    #[serde(default)]
    pub within: Option<String>,
    /// Only rule violations (cartograph.toml).
    #[serde(default)]
    pub violations: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CheckArgs {
    /// A git revision (branch, tag, commit, `HEAD~3`): then only violations the working tree
    /// introduces relative to it fail; the rest are listed as existing. Omitted, null or
    /// blank = check every rule on the working tree.
    #[serde(default)]
    pub base: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DiffArgs {
    /// A git revision (branch, tag, commit, `HEAD~3`). Omitted, null or blank = the
    /// merge-base of HEAD with the default branch (on that branch itself: HEAD, i.e. the
    /// uncommitted changes).
    #[serde(default)]
    pub base: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct HotspotsArgs {
    /// Only files in this folder (or this file).
    #[serde(default)]
    pub within: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OpenMapArgs {
    /// What to show: a file or folder (`file:src/a.rs`, `dir:src`, `src/`), a function or
    /// method (`sym:src/a.rs#login`, `src/a.rs#Auth.login`, `login`), or any id the other
    /// tools accept. Omitted = the whole repo.
    #[serde(default)]
    pub focus: Option<String>,
    /// `plan` (folders and files) or `flows` (call flows). Default: `flows` when `focus` is
    /// a function or method, else `plan`.
    #[serde(default)]
    pub view: Option<String>,
    /// Open the map in the user's browser (default true). The URL is returned either way.
    #[serde(default)]
    pub open_browser: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AnnotateArgs {
    /// The file or folder the note is about.
    pub node: String,
    /// The note (at most 600 characters). Empty deletes the note.
    pub text: String,
}

/// The MCP server.
#[derive(Clone)]
pub struct Cartograph {
    pub engine: Arc<Engine>,
    /// The web map that `open_map` starts on demand (see [`crate::map`]).
    pub map: Arc<crate::map::Map>,
    guard: Guard,
}

/// A tool failure the caller can act on. The error data always goes in
/// `structuredContent`: the usage stats read its `code`, and `call_tool` drops it before
/// sending unless `--structured` is on.
fn tool_error(message: String, data: Value, _structured: bool) -> CallToolResult {
    let mut r = CallToolResult::error(vec![ContentBlock::text(message)]);
    r.structured_content = Some(json!({ "error": data }));
    r
}

fn query_error(e: &QueryError, structured: bool) -> CallToolResult {
    let (code, msg, suggestions) = match e {
        QueryError::UnknownNode { input, suggestions } => (
            "unknown_node",
            if suggestions.is_empty() {
                format!(
                    "unknown node {input:?}: not a file, folder or library of the index. Call \
                     find with part of its name, or overview to see the folders."
                )
            } else {
                format!(
                    "unknown node {input:?}; did you mean: {}? (find searches by name)",
                    suggestions.join(", ")
                )
            },
            suggestions.clone(),
        ),
        QueryError::BadArgument(m) => ("bad_argument", m.clone(), Vec::new()),
        QueryError::Ambiguous { candidates, .. } => {
            ("ambiguous", e.to_string(), candidates.clone())
        }
    };
    tool_error(
        msg.clone(),
        json!({ "code": code, "message": msg, "suggestions": suggestions }),
        structured,
    )
}

#[tool_router]
impl Cartograph {
    pub fn new(engine: Arc<Engine>) -> Cartograph {
        let guard = Guard::new(engine.opts.max_concurrent, engine.opts.tool_timeout);
        Cartograph {
            engine,
            map: Arc::default(),
            guard,
        }
    }

    /// A tool argument as the query crate wants it: an absolute path inside the repo
    /// becomes repo-relative (prefixes `file:`, `dir:`, `sym:` and a `#name` suffix kept).
    pub fn norm(&self, arg: &str) -> String {
        norm_arg(&self.engine.root, arg)
    }

    /// Answer a query: freshness gate, then the query under the guard, then the text (with
    /// the freshness note first) and, when enabled, its JSON as `structuredContent`.
    pub async fn run<T, F>(&self, tool: &'static str, args: Vec<String>, f: F) -> CallToolResult
    where
        T: Serialize + Send + 'static,
        F: FnOnce(&Loaded) -> Result<Answer<T>, QueryError> + Send + 'static,
    {
        let structured = self.engine.opts.structured;
        let res = self
            .gated(tool, args, move |l| {
                f(l).map(|a| {
                    let data = if structured {
                        serde_json::to_value(&a.data).ok()
                    } else {
                        None
                    };
                    (a.text, data)
                })
            })
            .await;
        let ((text, data), note) = match res {
            Ok(x) => x,
            Err(r) => return r,
        };
        // The query crate points at CLI commands; here they are tools.
        let text = text.replace("`cartograph query ", "`");
        let text = match note {
            Some(n) => format!("{n}\n{text}"),
            None => text,
        };
        let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
        r.structured_content = data;
        r
    }

    /// The freshness gate, then `f` under the guard. The value and the freshness note, or
    /// the answer to send instead: "indexing", the query's error, a timeout or a panic.
    async fn gated<R, F>(
        &self,
        tool: &'static str,
        args: Vec<String>,
        f: F,
    ) -> Result<(R, Option<String>), CallToolResult>
    where
        R: Send + 'static,
        F: FnOnce(&Loaded) -> Result<R, QueryError> + Send + 'static,
    {
        let t0 = Instant::now();
        let structured = self.engine.opts.structured;
        let (loaded, note) = match self.engine.ready(&args).await {
            Ready::Go { loaded, note } => (loaded, note),
            Ready::NotReady(msg) => {
                let (done, total) = self.engine.progress.get();
                // Kept for the stats; `call_tool` drops it unless `--structured`.
                let mut r = CallToolResult::success(vec![ContentBlock::text(msg.clone())]);
                r.structured_content = Some(json!({
                    "status": "indexing", "message": msg, "done": done, "total": total
                }));
                return Err(r);
            }
        };
        let generation = loaded.generation;
        let res = self.guard.run(move || f(&loaded)).await;
        let ms = t0.elapsed().as_millis() as u64;
        tracing::info!(
            tool,
            ms,
            generation,
            ok = matches!(res, Ok(Ok(_))),
            "tool call"
        );
        match res {
            Ok(Ok(v)) => Ok((v, note)),
            Ok(Err(e)) => Err(query_error(&e, structured)),
            Err(e) => {
                tracing::error!(tool, error = %e, "tool failed");
                let msg = format!(
                    "cartograph {tool} failed: {e}. The server is still running; try a \
                     narrower argument or use grep/read for this one."
                );
                let code = match e {
                    crate::guard::GuardError::TimedOut(_) => "timeout",
                    crate::guard::GuardError::Panicked(_) => "internal",
                };
                Err(tool_error(
                    msg.clone(),
                    json!({ "code": code, "message": msg }),
                    structured,
                ))
            }
        }
    }

    #[tool(
        description = "Map of the repo or of one folder: subfolders with file counts, LOC and \
            kind, entry points, hub files, main libraries, findings per folder. Call it first in \
            a session or before exploring an unfamiliar area, instead of listing directories or \
            reading READMEs. `path`: folder (`src`, `dir:src`), omitted = whole repo; `depth` 1-4.",
        annotations(read_only_hint = true)
    )]
    async fn overview(&self, Parameters(a): Parameters<OverviewArgs>) -> CallToolResult {
        let path = a
            .path
            .map(|p| self.norm(&p))
            .filter(|p| !p.trim().is_empty());
        let depth = a.depth.unwrap_or(1);
        self.run("overview", Vec::new(), move |l| {
            cartograph_query::overview::overview(&l.repo, path.as_deref(), depth)
        })
        .await
    }

    #[tool(
        description = "Everything about one file, folder or library in one call: role (kind, \
            layer, entry point), what it imports and who imports it (grouped by folder), tests \
            that import it, libraries, findings, main symbols, and the note left by annotate. \
            Use it instead of opening a file to learn what it is and who depends on it. `node`: \
            file:src/a.rs, dir:src, src/a.rs, an absolute path, or npm:react.",
        annotations(read_only_hint = true)
    )]
    async fn explain(&self, Parameters(a): Parameters<NodeArgs>) -> CallToolResult {
        let node = self.norm(&a.node);
        let data_dir = self.engine.data_dir.clone();
        let root = self.engine.root.clone();
        self.run("explain", vec![node.clone()], move |l| {
            let (notes, _) = annotations::load_all(&data_dir, &root);
            cartograph_query::explain::explain_with(&l.repo, &node, |id| {
                annotations::annotation(&notes, &l.hashes, id)
            })
        })
        .await
    }

    #[tool(
        description = "Dependency tree of a file or folder: what it uses (dir=out, default), \
            what uses it (dir=in) or both, depth 1-3, per file or per folder (level). Use it \
            instead of grepping import statements. `node`: file or folder id or path.",
        annotations(read_only_hint = true)
    )]
    async fn deps(&self, Parameters(a): Parameters<DepsArgs>) -> CallToolResult {
        let structured = self.engine.opts.structured;
        let parsed = (|| -> Result<_, QueryError> {
            Ok((
                a.dir
                    .as_deref()
                    .unwrap_or("out")
                    .parse::<deps::Direction>()?,
                a.level
                    .as_deref()
                    .unwrap_or("file")
                    .parse::<deps::Level>()?,
            ))
        })();
        let (dir, level) = match parsed {
            Ok(x) => x,
            Err(e) => return query_error(&e, structured),
        };
        let node = self.norm(&a.node);
        let depth = a.depth.unwrap_or(1);
        self.run("deps", vec![node.clone()], move |l| {
            deps::deps(&l.repo, &node, dir, depth, level)
        })
        .await
    }

    #[tool(
        description = "Before editing files: everything that depends on them transitively (what \
            can break), grouped by folder, and the test files that reach them (the tests to \
            run). `nodes`: the files or folders about to change, e.g. [\"src/model.rs\"].",
        annotations(read_only_hint = true)
    )]
    async fn impact(&self, Parameters(a): Parameters<ImpactArgs>) -> CallToolResult {
        let nodes: Vec<String> = a.nodes.iter().map(|n| self.norm(n)).collect();
        self.run("impact", nodes.clone(), move |l| {
            cartograph_query::impact::impact(&l.repo, &nodes)
        })
        .await
    }

    #[tool(
        description = "Fuzzy search by name or path fragment over folders, files and libraries; \
            with kind=sym over functions, classes, methods and types (with path:line). Use it \
            instead of glob/grep when you know roughly a name but not where it lives. Returns \
            ids to pass to the other tools.",
        annotations(read_only_hint = true)
    )]
    async fn find(&self, Parameters(a): Parameters<FindArgs>) -> CallToolResult {
        let structured = self.engine.opts.structured;
        let kind = match a
            .kind
            .as_deref()
            .map(str::parse::<find::FindKind>)
            .transpose()
        {
            Ok(k) => k,
            Err(e) => return query_error(&e, structured),
        };
        let q = a.query;
        self.run("find", Vec::new(), move |l| find::find(&l.repo, &q, kind))
            .await
    }

    #[tool(
        description = "Why does A depend on B: the shortest import chain from `from` to `to` \
            (files or folders), each step with its import line, plus alternatives. Use it to \
            explain an unexpected dependency or a layer violation.",
        annotations(read_only_hint = true)
    )]
    async fn path(&self, Parameters(a): Parameters<PathArgs>) -> CallToolResult {
        let (from, to) = (self.norm(&a.from), self.norm(&a.to));
        self.run("path", vec![from.clone(), to.clone()], move |l| {
            cartograph_query::path::path(&l.repo, &from, &to)
        })
        .await
    }

    #[tool(
        description = "Outline of a file (or of a folder's files): functions, classes, methods \
            and types with line numbers, exported flag and how often each is called; when a \
            file has too many for a line each, the members of its biggest objects and classes \
            come by name (`api (150): save, load, …`), then the rest. Use it instead of reading a whole file \
            to see what it defines. `node`: file or folder.",
        annotations(read_only_hint = true)
    )]
    async fn symbols(&self, Parameters(a): Parameters<NodeArgs>) -> CallToolResult {
        let node = self.norm(&a.node);
        self.run("symbols", vec![node.clone()], move |l| {
            symbols::symbols(&l.repo, &node)
        })
        .await
    }

    #[tool(
        description = "Before changing a function, method or type: who calls it, grouped by \
            file with line numbers and confidence (exact, resolved, heuristic); callers in \
            tests come last, summed up per file. `symbol`: \
            sym:src/a.rs#name, src/a.rs#Type.method, Class.method or a bare name (an ambiguous \
            name lists the candidates; part of a name is completed when one symbol has it, \
            and the answer says so).",
        annotations(read_only_hint = true)
    )]
    async fn callers(&self, Parameters(a): Parameters<CallersArgs>) -> CallToolResult {
        let sym = self.norm(&a.symbol);
        self.run("callers", vec![sym.clone()], move |l| {
            symbols::callers(&l.repo, &sym)
        })
        .await
    }

    #[tool(
        description = "Class hierarchy instead of grepping `extends`/`implements`/`impl … for`: \
            the supertypes of a class, struct, interface or trait (the chain, in method-lookup \
            order, with file:line; those outside the repo by name) and its direct subtypes. \
            Given a method, the hierarchy of its type, marking the supertypes that define it \
            and the subtypes that override it: use it before changing a base or interface \
            method, next to callers (calls through a subtype that inherits the method count \
            as its callers). `symbol`: sym:src/a.ts#Shape, Class.method or a bare name.",
        annotations(read_only_hint = true)
    )]
    async fn hierarchy(&self, Parameters(a): Parameters<CallersArgs>) -> CallToolResult {
        let sym = self.norm(&a.symbol);
        self.run("hierarchy", vec![sym.clone()], move |l| {
            cartograph_query::hierarchy::hierarchy(&l.repo, &sym)
        })
        .await
    }

    #[tool(
        description = "Where execution starts: main functions, HTTP routes, CLI commands, IPC \
            handlers (Electron `ipcMain`), Godot main scene and autoloads; optionally one kind or within a folder. Entries in test, \
            fixture or example trees are only counted (`include_tests` lists them). Use it to \
            see how a feature is reached from outside before tracing it.",
        annotations(read_only_hint = true)
    )]
    async fn entries(&self, Parameters(a): Parameters<EntriesArgs>) -> CallToolResult {
        let structured = self.engine.opts.structured;
        let kind = match a.kind.as_deref().map(str::parse::<EntryKind>) {
            None => None,
            Some(Ok(k)) => Some(k),
            Some(Err(_)) => {
                return query_error(
                    &QueryError::BadArgument(
                        "kind must be main, http, cli, ipc, godot_main_scene or godot_autoload"
                            .into(),
                    ),
                    structured,
                );
            }
        };
        let within = a.within.map(|w| self.norm(&w));
        self.run("entries", Vec::new(), move |l| {
            symbols::entries(&l.repo, kind, within.as_deref(), a.include_tests)
        })
        .await
    }

    #[tool(
        description = "Architecture problems with evidence and location: dependency cycles, \
            layer violations (cartograph.toml rules), god modules, hubs, unstable dependencies, \
            orphans, deep chains, hidden coupling (files that change together in git with no \
            import between them). Filter by kind, severity, within (file or folder) or \
            violations only.",
        annotations(read_only_hint = true)
    )]
    async fn findings(&self, Parameters(a): Parameters<FindingsArgs>) -> CallToolResult {
        let structured = self.engine.opts.structured;
        let mut filter = list::Filter {
            within: a.within.map(|w| self.norm(&w)),
            violations: a.violations.unwrap_or(false),
            ..list::Filter::default()
        };
        for k in a.kind.iter().flatten().flat_map(|k| k.split(',')) {
            match k.trim().parse() {
                Ok(k) => filter.kinds.push(k),
                Err(e) => return query_error(&QueryError::BadArgument(e), structured),
            }
        }
        if let Some(s) = a.severity {
            match s.parse() {
                Ok(s) => filter.severity = Some(s),
                Err(e) => return query_error(&QueryError::BadArgument(e), structured),
            }
        }
        self.run("findings", Vec::new(), move |l| {
            list::findings(&l.repo, &filter).map(|a| with_config_warning(a, l))
        })
        .await
    }

    #[tool(
        description = "Before finishing a change: does the working tree still respect the \
            architecture rules of cartograph.toml (layers, no cycles, max fan-out)? Returns ok \
            or each violation with its evidence. With `base` (a branch, tag or commit, e.g. \
            main), only the violations your change introduces count; ones already in the base \
            are listed as existing.",
        annotations(read_only_hint = true)
    )]
    async fn check(&self, Parameters(a): Parameters<CheckArgs>) -> CallToolResult {
        let Some(rev) = given(a.base) else {
            return self
                .run("check", Vec::new(), move |l| {
                    Ok(with_config_warning(
                        cartograph_query::check::check(&l.repo),
                        l,
                    ))
                })
                .await;
        };
        let engine = self.engine.clone();
        self.run("check", Vec::new(), move |l| {
            let b = open_base(&engine, l, Some(&rev))?;
            let a = cartograph_query::check::check_base(&l.repo, &b.repo, &b.revision.label());
            Ok(with_base_warning(with_config_warning(a, l), &b))
        })
        .await
    }

    #[tool(
        description = "What your change does to the architecture, compared with a git base: \
            new and resolved findings (cycles, layer violations…), new or removed dependencies \
            between packages/top folders (or layers), new and fixed unresolved imports, added \
            and removed files. Use it instead of reading `git diff` to review a branch or the \
            uncommitted work structurally. `base`: branch, tag or commit; omitted = the \
            merge-base with the default branch.",
        annotations(read_only_hint = true)
    )]
    async fn diff(&self, Parameters(a): Parameters<DiffArgs>) -> CallToolResult {
        let engine = self.engine.clone();
        let rev = given(a.base);
        self.run("diff", Vec::new(), move |l| {
            let b = open_base(&engine, l, rev.as_deref())?;
            let a =
                cartograph_query::diff::diff(&b.repo, &l.repo, &b.revision.label(), "working tree");
            Ok(with_base_warning(with_config_warning(a, l), &b))
        })
        .await
    }

    #[tool(
        description = "Code files that change most and are most nested (git churn × \
            indentation complexity), with owners and last change: where bugs and refactors \
            concentrate. Use it instead of `git log` statistics to pick what to review or \
            refactor first. `within`: a folder or file.",
        annotations(read_only_hint = true)
    )]
    async fn hotspots(&self, Parameters(a): Parameters<HotspotsArgs>) -> CallToolResult {
        let within = a.within.map(|w| self.norm(&w));
        self.run("hotspots", Vec::new(), move |l| {
            cartograph_query::hotspots::hotspots(&l.repo, within.as_deref())
        })
        .await
    }

    #[tool(
        description = "Show the user cartograph's visual map in the browser: the plan of \
            folders and files, or the call flows of a function, focused on what you name. Use \
            it when the user asks to see, visualize or show the architecture, a diagram, a flow \
            or the map. Returns the URL. `focus`: file, folder or function (file:, dir:, sym:, \
            a path, path#name), omitted = whole repo; `view`: plan or flows (default flows for \
            a function, else plan); `open_browser` (default true).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn open_map(&self, Parameters(a): Parameters<OpenMapArgs>) -> CallToolResult {
        let structured = self.engine.opts.structured;
        let view = match given(a.view).map(|v| v.parse::<map::MapView>()).transpose() {
            Ok(v) => v,
            Err(e) => return query_error(&e, structured),
        };
        let focus = given(a.focus).map(|f| self.norm(&f));
        let args: Vec<String> = focus.iter().cloned().collect();
        let (target, note) = match self
            .gated("open_map", args, move |l| {
                map::target(&l.repo, focus.as_deref(), view)
            })
            .await
        {
            Ok(x) => x,
            Err(r) => return r,
        };
        let base = match self.map.ensure(&self.engine).await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(error = %e, "web map failed");
                return tool_error(
                    e.clone(),
                    json!({ "code": "map_failed", "message": e }),
                    structured,
                );
            }
        };
        let url = target.url(&base);
        // A failed launch is not an error: the URL is in the answer.
        let launched = if a.open_browser.unwrap_or(true) {
            let u = url.clone();
            Some(
                match tokio::task::spawn_blocking(move || open::that_detached(&u)).await {
                    Ok(r) => r.map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                },
            )
        } else {
            None
        };
        let first = match &launched {
            None => format!("Map URL: {url}"),
            Some(Ok(())) => format!("Opened the map in the browser: {url}"),
            Some(Err(e)) => {
                tracing::warn!(error = %e, "no browser opened");
                format!("Could not open a browser ({e}); give the user this URL instead: {url}")
            }
        };
        let opened = matches!(launched, Some(Ok(())));
        let mut text = format!(
            "{first}\nShows {}. The page refreshes when the code changes and stays up while \
             this session runs.\n",
            target.what
        );
        if let Some(n) = note {
            text = format!("{n}\n{text}");
        }
        let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
        r.structured_content = Some(json!({
            "url": url,
            "view": target.view.as_str(),
            "focus": target.focus,
            "shows": target.what,
            "opened": opened,
        }));
        r
    }

    #[tool(
        description = "Leave a short note (at most 600 characters) on a file or folder: what it \
            is for, an invariant, a gotcha. explain shows it in every later session and marks it \
            stale once the code changes. Empty text deletes the note. `node`: file or folder.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn annotate(&self, Parameters(a): Parameters<AnnotateArgs>) -> CallToolResult {
        let structured = self.engine.opts.structured;
        let text = a.text.trim().to_string();
        if text.chars().count() > annotations::MAX_CHARS {
            return query_error(
                &QueryError::BadArgument(format!(
                    "the note has {} characters; keep it at most {} (say what the code is for, \
                     not what each line does)",
                    text.chars().count(),
                    annotations::MAX_CHARS
                )),
                structured,
            );
        }
        let node = self.norm(&a.node);
        let engine = self.engine.clone();
        self.run("annotate", vec![node.clone()], move |l| {
            let n = l.repo.resolve(&node)?;
            let id = l.repo.graph.id(n);
            let Some(hash) = annotations::content_hash(&l.hashes, &id) else {
                return Err(QueryError::BadArgument(format!(
                    "{id} has no content to annotate; notes go on files and folders"
                )));
            };
            let _w = engine.notes_lock.lock().unwrap_or_else(|e| e.into_inner());
            // The repo's versioned notes file once it exists (or when asked for), else the
            // data dir. A note lives in one place: writing it there takes it out of the data
            // dir, and a delete removes it from both.
            let root = &engine.root;
            let in_repo = engine.opts.notes_in_repo || annotations::repo_has_notes(root);
            let mut local = annotations::load(&engine.data_dir).map_err(QueryError::BadArgument)?;
            let mut repo = match in_repo {
                true => Some(annotations::load_repo(root).map_err(QueryError::BadArgument)?),
                false => None,
            };
            let deleted = text.is_empty();
            let had_local = local.notes.remove(&id).is_some();
            let had_repo = repo.as_mut().is_some_and(|r| r.notes.remove(&id).is_some());
            if !deleted {
                let note = annotations::Note {
                    text: text.clone(),
                    hash,
                    updated: annotations::now(),
                };
                match repo.as_mut() {
                    Some(r) => r.notes.insert(id.clone(), note),
                    None => local.notes.insert(id.clone(), note),
                };
            }
            if let Some(r) = &repo
                && (had_repo || !deleted)
            {
                annotations::save_repo(root, r).map_err(QueryError::BadArgument)?;
            }
            if had_local || (!deleted && repo.is_none()) {
                annotations::save(&engine.data_dir, &local).map_err(QueryError::BadArgument)?;
            }
            let place = if in_repo {
                annotations::REPO_FILE
            } else {
                "the data dir"
            };
            let msg = if deleted {
                format!("note on {id} deleted\n")
            } else {
                format!("note on {id} saved in {place}; explain shows it until the code changes\n")
            };
            Ok(Answer {
                data: json!({ "id": id, "deleted": deleted, "text": text }),
                text: msg,
            })
        })
        .await
    }
}

/// Append the config problem to `check` and `findings` answers: rules may be missing.
fn with_config_warning<T>(mut a: Answer<T>, l: &Loaded) -> Answer<T> {
    if let Some(e) = &l.config_error {
        a.text.push_str(&format!(
            "warning: cartograph.toml is invalid, defaults used: {}\n",
            cartograph_query::budget::clip(e, 300)
        ));
    }
    a
}

/// An optional string argument as given: agents often send `""` for one they leave out, so
/// a blank one counts as missing (the CLI, where nobody types `--base ""` by accident,
/// rejects it).
fn given(arg: Option<String>) -> Option<String> {
    arg.filter(|s| !s.trim().is_empty())
}

/// The git base of `diff` / `check(base)`, with the config and history of the working
/// tree's state (the history it was built with, not a newer one in the store: the sides
/// must not differ in history); a git problem (unknown revision, no repository) is a tool
/// error.
fn open_base(engine: &Engine, l: &Loaded, rev: Option<&str>) -> Result<base::Base, QueryError> {
    let opts = cartograph_index::IndexOptions {
        max_file_bytes: engine.opts.max_file_bytes,
        data_dir: Some(engine.data_dir.clone()),
        ..cartograph_index::IndexOptions::default()
    };
    base::open(&engine.root, rev, &opts, &l.repo.config, &l.history, None)
        .map_err(QueryError::BadArgument)
}

/// Append a warning about base files that could not be written.
fn with_base_warning<T>(mut a: Answer<T>, b: &base::Base) -> Answer<T> {
    if let Some(w) = base::write_warning(&b.snapshot) {
        a.text.push_str(&w);
        a.text.push('\n');
    }
    a
}

/// See [`Cartograph::norm`].
pub fn norm_arg(root: &Path, arg: &str) -> String {
    let t = arg.trim();
    for prefix in ["file:", "dir:", "sym:", ""] {
        let Some(rest) = t.strip_prefix(prefix) else {
            continue;
        };
        let (p, frag) = match rest.find('#') {
            Some(i) => rest.split_at(i),
            None => (rest, ""),
        };
        let path = Path::new(p);
        if path.is_absolute() {
            let rel = cartograph_index::walk::relative(root, path).or_else(|| {
                dunce::canonicalize(path)
                    .ok()
                    .and_then(|c| cartograph_index::walk::relative(root, &c))
            });
            if let Some(rel) = rel {
                let rel = if rel.is_empty() { ".".to_string() } else { rel };
                return format!("{prefix}{rel}{frag}");
            }
        }
        break;
    }
    t.to_string()
}

#[tool_handler]
impl ServerHandler for Cartograph {
    /// The router's dispatch, plus the usage stats of every call (argument and unknown-tool
    /// errors included) and, without `--structured`, no `structuredContent`.
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        let t0 = Instant::now();
        let router = Self::tool_router();
        let tool = if router.has_route(&request.name) {
            request.name.to_string()
        } else {
            crate::stats::UNKNOWN_TOOL.to_string()
        };
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let mut res = router.call(tcc).await;
        if let Some(stats) = &self.engine.stats {
            stats.tool_call(&tool, t0.elapsed(), &res);
        }
        if !self.engine.opts.structured
            && let Ok(rmcp::model::CallToolResponse::Complete(r)) = &mut res
        {
            r.structured_content = None;
        }
        res
    }

    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("cartograph", env!("CARGO_PKG_VERSION"))
                    .with_title("cartograph: architecture map of this repo"),
            )
            .with_instructions(INSTRUCTIONS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_fit_the_upfront_limit() {
        assert!(INSTRUCTIONS.chars().count() < 4096);
    }

    #[test]
    fn absolute_paths_become_relative() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "").unwrap();
        let abs = root.join("src").join("a.rs");
        let abs = abs.to_str().unwrap();
        assert_eq!(norm_arg(&root, abs), "src/a.rs");
        assert_eq!(norm_arg(&root, &format!("file:{abs}")), "file:src/a.rs");
        assert_eq!(norm_arg(&root, &format!("{abs}#run")), "src/a.rs#run");
        assert_eq!(norm_arg(&root, root.to_str().unwrap()), ".");
        assert_eq!(norm_arg(&root, "src/a.rs"), "src/a.rs");
        assert_eq!(norm_arg(&root, " dir:src "), "dir:src");
    }
}
