//! The web map behind `open_map`: the HTTP server of `cartograph serve`, started inside this
//! process on first use and fed by the [`Engine`] instead of indexing on its own.
//!
//! - One server per process, on 127.0.0.1 and a port the OS picks (never one in use). Later
//!   calls reuse it; it stops with the MCP server.
//! - No second indexer or watcher: each state the engine publishes is read back from the
//!   store into a snapshot and served with the usual `index-finished` event, so an open page
//!   refreshes live. The page's "reindex" button asks the engine for a run.
//! - The server keeps its Host and Origin checks (DNS rebinding).

use crate::engine::Engine;
use cartograph_query::{NodeRef, QueryError, Repo};
use cartograph_server::state::AppState;
use cartograph_server::{Running, ServeConfig, indexer};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// The map server of this process, once started.
#[derive(Default)]
pub struct Map {
    served: Mutex<Option<Served>>,
}

struct Served {
    running: Running,
    /// The feed from the engine and the reindex forwarder.
    tasks: Vec<JoinHandle<()>>,
}

/// `http://127.0.0.1:<port>/`.
pub fn base_url(addr: SocketAddr) -> String {
    format!("http://{addr}/")
}

impl Map {
    /// The map's base URL, starting the server first if this is the first call. The first
    /// snapshot is served before this returns, so the URL answers with data at once. Call it
    /// once the engine has an index (the store is read, nothing is indexed here).
    pub async fn ensure(&self, engine: &Arc<Engine>) -> Result<String, String> {
        let mut slot = self.served.lock().await;
        if let Some(s) = slot.as_ref() {
            return Ok(base_url(s.running.addr));
        }
        let config = ServeConfig {
            data_dir: Some(engine.data_dir.clone()),
            port: Some(0),
            watch: engine.watching(),
            max_file_bytes: engine.opts.max_file_bytes,
            ..ServeConfig::new(engine.root.clone())
        };
        let running = cartograph_server::start_fed(config)
            .await
            .map_err(|e| format!("cannot start the web map: {e}"))?;
        let state = running.state.clone();
        // Subscribed before the first snapshot: a state published while it is built gets
        // its own snapshot right after.
        let mut published = engine.subscribe();
        if let Err(e) = publish(engine, &state).await {
            running.stop().await;
            return Err(format!("cannot read the index for the web map: {e}"));
        }
        let feed = {
            let engine = engine.clone();
            let state = state.clone();
            tokio::spawn(async move {
                while published.changed().await.is_ok() {
                    if let Err(e) = publish(&engine, &state).await {
                        tracing::warn!(error = %e, "web map not refreshed");
                    }
                }
            })
        };
        let reindex = {
            let engine = engine.clone();
            tokio::spawn(async move {
                loop {
                    let _ = state.scheduler.next().await;
                    engine.request_reindex();
                }
            })
        };
        let url = base_url(running.addr);
        tracing::info!(%url, "web map started");
        *slot = Some(Served {
            running,
            tasks: vec![feed, reindex],
        });
        Ok(url)
    }

    /// Stop the map server (open pages lose their connection), if it runs.
    pub async fn stop(&self) {
        let Some(s) = self.served.lock().await.take() else {
            return;
        };
        for t in &s.tasks {
            t.abort();
        }
        s.running.stop().await;
        tracing::info!("web map stopped");
    }
}

/// Serve what the store holds now as the next generation of the map.
async fn publish(engine: &Arc<Engine>, state: &Arc<AppState>) -> Result<(), String> {
    let t0 = Instant::now();
    let generation = state.generation() + 1;
    let eng = engine.clone();
    let snap = tokio::task::spawn_blocking(move || {
        indexer::snapshot_from_store(&eng.data_dir, generation, eng.load_config())
    })
    .await
    .map_err(|e| format!("snapshot task failed: {e}"))??;
    let changed = engine
        .current()
        .map(|l| l.changed.clone())
        .unwrap_or_default();
    indexer::publish(state, snap, t0.elapsed().as_millis() as u64, &changed);
    tracing::debug!(generation, "web map refreshed");
    Ok(())
}

/// Which view of the map a link opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapView {
    /// The architecture plan (Plano): folders and files.
    Plan,
    /// The call flows (Flujos) from a function.
    Flows,
}

impl MapView {
    pub fn as_str(self) -> &'static str {
        match self {
            MapView::Plan => "plan",
            MapView::Flows => "flows",
        }
    }
}

impl std::str::FromStr for MapView {
    type Err = QueryError;

    fn from_str(s: &str) -> Result<MapView, QueryError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "plan" => Ok(MapView::Plan),
            "flows" | "flow" => Ok(MapView::Flows),
            other => Err(QueryError::BadArgument(format!(
                "view must be plan or flows, not {other:?}"
            ))),
        }
    }
}

/// What a link to the map shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapTarget {
    pub view: MapView,
    /// The id the page selects, as the web API names it (`file:`, `dir:`, `ext:`, `sym:`);
    /// none = the whole repo.
    pub focus: Option<String>,
    /// One line for the user: what the page shows.
    pub what: String,
}

impl MapTarget {
    /// `base` (ending in `/`) plus `?view=…&focus=…`, the focus percent-encoded.
    pub fn url(&self, base: &str) -> String {
        let mut u = format!("{base}?view={}", self.view.as_str());
        if let Some(f) = &self.focus {
            u.push_str("&focus=");
            u.push_str(&encode(f));
        }
        u
    }
}

/// Percent-encode a query value. `:` and `/` stay as they are (both are allowed in a query,
/// and ids stay readable); `#`, `&`, `+`, `%`, `=`, `?`, spaces and non-ASCII bytes are
/// encoded.
pub fn encode(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b':' | b'/') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// A focus resolved by the query layer.
enum Hit {
    Node(NodeRef),
    Sym(String),
}

/// Ids that name a file, folder or library, never a symbol.
const NODE_PREFIXES: [&str; 5] = ["file:", "dir:", "repo:", "ext:", "external:"];

/// What `focus` names: a `sym:` id is a symbol, a `file:`/`dir:`/library id a node; anything
/// else is tried as a file, folder or library first, then as a symbol (`path#name`,
/// `Class.method`, a bare name). Unknown: the suggestions of both.
fn resolve(repo: &Repo, input: &str) -> Result<Hit, QueryError> {
    if input.starts_with("sym:") {
        return repo.symbols.resolve(input).map(Hit::Sym);
    }
    if NODE_PREFIXES.iter().any(|p| input.starts_with(p)) {
        return repo.resolve(input).map(Hit::Node);
    }
    let node_err = match repo.resolve(input) {
        Ok(n) => return Ok(Hit::Node(n)),
        Err(e) => e,
    };
    match repo.symbols.resolve(input) {
        Ok(id) => Ok(Hit::Sym(id)),
        Err(e @ QueryError::Ambiguous { .. }) => Err(e),
        Err(QueryError::UnknownNode {
            suggestions: syms, ..
        }) => match node_err {
            QueryError::UnknownNode {
                input,
                suggestions: nodes,
            } => {
                // A `#` means a symbol was meant: its suggestions first.
                let (first, second) = if input.contains('#') {
                    (syms, nodes)
                } else {
                    (nodes, syms)
                };
                let mut suggestions: Vec<String> = Vec::new();
                for s in first.into_iter().chain(second) {
                    if !suggestions.contains(&s) {
                        suggestions.push(s);
                    }
                }
                suggestions.truncate(8);
                Err(QueryError::UnknownNode { input, suggestions })
            }
            other => Err(other),
        },
        Err(_) => Err(node_err),
    }
}

/// The web API's id of a query id (`external:` → `ext:`).
fn api_id(id: &str) -> String {
    match id.strip_prefix("external:") {
        Some(rest) => format!("ext:{rest}"),
        None => id.to_string(),
    }
}

/// What `open_map` shows for `focus` (already repo-relative) and `view`:
/// - nothing, or the repo root: the whole plan (or Flujos at its first entry point);
/// - a folder or a library: the plan, selected (no flows: they start at code);
/// - a file: the plan, selected, or with `flows` the flow of its module-level code;
/// - a function or method: its flow (default), or with `plan` its file;
/// - another symbol (class, type…): its file on the plan (default), or with `flows` its flow.
pub fn target(
    repo: &Repo,
    focus: Option<&str>,
    view: Option<MapView>,
) -> Result<MapTarget, QueryError> {
    let whole = |view: MapView| MapTarget {
        view,
        focus: None,
        what: match view {
            MapView::Plan => "the plan of the whole repo".to_string(),
            MapView::Flows => "the call flows, from the first entry point".to_string(),
        },
    };
    let Some(input) = focus.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(whole(view.unwrap_or(MapView::Plan)));
    };
    let no_flow = |id: &str, what: &str| {
        QueryError::BadArgument(format!(
            "{id} is a {what}: the flows view starts at a function, a method or a file. Use \
             view plan, or focus a function (find with kind sym lists them)"
        ))
    };
    match resolve(repo, input)? {
        Hit::Node(n) => {
            let id = api_id(&repo.graph.id(n));
            match (n, view) {
                (NodeRef::Ext(_), Some(MapView::Flows)) => Err(no_flow(&id, "library")),
                (NodeRef::Dir(_), Some(MapView::Flows)) if id != "repo:" => {
                    Err(no_flow(&id, "folder"))
                }
                (NodeRef::Dir(_), v) if id == "repo:" => Ok(whole(v.unwrap_or(MapView::Plan))),
                (NodeRef::Ext(_), _) => Ok(MapTarget {
                    view: MapView::Plan,
                    what: format!(
                        "library {} on the plan, with the libraries shown",
                        id.trim_start_matches("ext:")
                    ),
                    focus: Some(id),
                }),
                (NodeRef::Dir(_), _) => Ok(MapTarget {
                    view: MapView::Plan,
                    what: format!(
                        "folder {}/ on the plan, selected, its parent folders open",
                        id.trim_start_matches("dir:")
                    ),
                    focus: Some(id),
                }),
                (NodeRef::File(_), Some(MapView::Flows)) => Ok(MapTarget {
                    view: MapView::Flows,
                    what: format!(
                        "the call flow of the module-level code of {}",
                        id.trim_start_matches("file:")
                    ),
                    focus: Some(id),
                }),
                (NodeRef::File(_), _) => Ok(MapTarget {
                    view: MapView::Plan,
                    what: format!(
                        "file {} on the plan, selected, its folders open",
                        id.trim_start_matches("file:")
                    ),
                    focus: Some(id),
                }),
            }
        }
        Hit::Sym(id) => {
            let Some(sym) = repo.symbols.symbol(&id) else {
                return Err(QueryError::UnknownNode {
                    input: input.to_string(),
                    suggestions: Vec::new(),
                });
            };
            let callable = sym.kind.is_callable();
            let name = sym.qualified();
            match view.unwrap_or(if callable {
                MapView::Flows
            } else {
                MapView::Plan
            }) {
                MapView::Flows => Ok(MapTarget {
                    view: MapView::Flows,
                    what: format!("the call flow from {name} ({}:{})", sym.file, sym.line),
                    focus: Some(id),
                }),
                MapView::Plan => Ok(MapTarget {
                    view: MapView::Plan,
                    what: format!(
                        "file {} on the plan, selected (it defines {name}, line {})",
                        sym.file, sym.line
                    ),
                    focus: Some(format!("file:{}", sym.file)),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartograph_core::{Graph, Node, NodeKind, Symbol, SymbolKind, Symbols, node_id};
    use cartograph_query::Config;

    #[test]
    fn focus_is_percent_encoded() {
        assert_eq!(encode("dir:src/app"), "dir:src/app");
        assert_eq!(
            encode("sym:src/a b.rs#Auth.login"),
            "sym:src/a%20b.rs%23Auth.login"
        );
        assert_eq!(encode("file:a&b=c+d%e?.ts"), "file:a%26b%3Dc%2Bd%25e%3F.ts");
        assert_eq!(encode("dir:ñ"), "dir:%C3%B1");
        let t = MapTarget {
            view: MapView::Flows,
            focus: Some("sym:src/a.rs#run".into()),
            what: String::new(),
        };
        assert_eq!(
            t.url("http://127.0.0.1:5000/"),
            "http://127.0.0.1:5000/?view=flows&focus=sym:src/a.rs%23run"
        );
        let whole = MapTarget {
            view: MapView::Plan,
            focus: None,
            what: String::new(),
        };
        assert_eq!(whole.url("http://x/"), "http://x/?view=plan");
    }

    #[test]
    fn views_parse() {
        assert_eq!("plan".parse::<MapView>().unwrap(), MapView::Plan);
        assert_eq!(" Flows ".parse::<MapView>().unwrap(), MapView::Flows);
        assert!("graph".parse::<MapView>().is_err());
    }

    fn repo() -> Repo {
        let file = |p: &str| Node {
            id: node_id(NodeKind::File, p),
            kind: NodeKind::File,
            path: p.to_string(),
            name: p.rsplit('/').next().unwrap_or(p).to_string(),
            lang: Some("rust".to_string()),
            loc: 10,
            bytes: 100,
            hash: None,
            meta: serde_json::Value::Null,
        };
        let graph = Graph {
            nodes: vec![file("src/auth/login.rs"), file("src/main.rs")],
            ..Graph::default()
        };
        let sym = |file: &str, name: &str, container: Option<&str>, kind| Symbol {
            file: file.to_string(),
            name: name.to_string(),
            container: container.map(str::to_string),
            kind,
            line: 3,
            end_line: 9,
            exported: true,
        };
        let symbols = Symbols {
            symbols: vec![
                sym("src/auth/login.rs", "login", None, SymbolKind::Function),
                sym("src/auth/login.rs", "Session", None, SymbolKind::Struct),
                sym(
                    "src/auth/login.rs",
                    "check",
                    Some("Session"),
                    SymbolKind::Method,
                ),
            ],
            indexed: true,
            ..Symbols::default()
        };
        Repo::with_symbols(&graph, &symbols, Config::default())
    }

    #[test]
    fn targets_follow_what_the_focus_is() {
        let r = repo();
        let t = |f: Option<&str>, v: Option<MapView>| target(&r, f, v);
        let whole = t(None, None).unwrap();
        assert_eq!((whole.view, whole.focus), (MapView::Plan, None));
        let root = t(Some("."), None).unwrap();
        assert_eq!((root.view, root.focus), (MapView::Plan, None));

        let dir = t(Some("src/auth/"), None).unwrap();
        assert_eq!(dir.view, MapView::Plan);
        assert_eq!(dir.focus.as_deref(), Some("dir:src/auth"));
        assert!(t(Some("dir:src/auth"), Some(MapView::Flows)).is_err());

        let file = t(Some("src/main.rs"), None).unwrap();
        assert_eq!(file.focus.as_deref(), Some("file:src/main.rs"));
        let module = t(Some("file:src/main.rs"), Some(MapView::Flows)).unwrap();
        assert_eq!(module.view, MapView::Flows);

        // A function opens its flow, a type its file; either can be asked the other way.
        let f = t(Some("login"), None).unwrap();
        assert_eq!(f.view, MapView::Flows);
        assert_eq!(f.focus.as_deref(), Some("sym:src/auth/login.rs#login"));
        let m = t(Some("Session.check"), None).unwrap();
        assert_eq!(m.view, MapView::Flows);
        let s = t(Some("sym:src/auth/login.rs#Session"), None).unwrap();
        assert_eq!(s.view, MapView::Plan);
        assert_eq!(s.focus.as_deref(), Some("file:src/auth/login.rs"));
        let p = t(Some("src/auth/login.rs#login"), Some(MapView::Plan)).unwrap();
        assert_eq!(p.focus.as_deref(), Some("file:src/auth/login.rs"));
        assert!(p.what.contains("login"), "{}", p.what);
    }

    #[test]
    fn unknown_focus_suggests_nodes_and_symbols() {
        let r = repo();
        match target(&r, Some("logn"), None) {
            Err(QueryError::UnknownNode { suggestions, .. }) => {
                assert!(
                    suggestions.iter().any(|s| s.contains("login")),
                    "{suggestions:?}"
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            target(&r, Some("file:src/nope.rs"), None),
            Err(QueryError::UnknownNode { .. })
        ));
    }
}
