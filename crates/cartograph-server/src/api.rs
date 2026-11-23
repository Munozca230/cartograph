//! HTTP routes (`docs/api-f2.md`). Every response is JSON with `generation`; errors use
//! the `{ "error": { "code", "message" } }` envelope; handler panics become a 500.

use crate::scheduler::Reason;
use crate::snapshot::{Depth, GraphQuery, Snapshot};
use crate::state::{AppState, Phase};
use axum::Router;
use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use serde_json::{Value, json};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::catch_panic::CatchPanicLayer;

type AppStateRef = State<Arc<AppState>>;

/// Keepalive comment interval on `/api/events`.
pub const KEEPALIVE: Duration = Duration::from_secs(15);

/// An API error with the contract's envelope.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub generation: u64,
    pub progress: Option<Value>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiError {
        ApiError {
            status,
            code,
            message: message.into(),
            generation: 0,
            progress: None,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    pub fn not_found(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    pub fn internal(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }

    fn at(mut self, generation: u64) -> ApiError {
        self.generation = generation;
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({
            "generation": self.generation,
            "error": { "code": self.code, "message": self.message },
        });
        if let Some(p) = self.progress {
            body["progress"] = p;
        }
        (self.status, axum::Json(body)).into_response()
    }
}

type ApiResult = Result<axum::Json<Value>, ApiError>;

/// All routes, with panic isolation.
pub fn router(state: Arc<AppState>) -> Router {
    let guarded = routes()
        .layer(axum::middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state.clone());
    with_isolation(guarded, Some(state))
}

/// Loopback names a request may use in `Host` (with the server's port).
const LOOPBACK_NAMES: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];

/// Whether `host` (a `Host` header) names this server on a loopback name and its port.
/// Anything else (another name pointing at 127.0.0.1, as in DNS rebinding) is refused.
pub fn allowed_host(host: &str, port: u16) -> bool {
    let host = host.trim().to_ascii_lowercase();
    LOOPBACK_NAMES
        .iter()
        .any(|n| host == format!("{n}:{port}") || (port == 80 && host == *n))
}

/// Whether `origin` (an `Origin` header) is this server's own origin.
pub fn allowed_origin(origin: &str, port: u16) -> bool {
    let origin = origin.trim().to_ascii_lowercase();
    match origin.strip_prefix("http://") {
        Some(rest) => allowed_host(rest, port),
        None => false,
    }
}

/// DNS-rebinding and cross-site protection, in front of every route (static files too):
/// - the `Host` header must be a loopback name with the server's port;
/// - a request other than GET/HEAD that carries an `Origin` must come from this server's
///   own origin. A request without `Origin` (curl, scripts) is allowed.
///
/// A refused request gets 403 `forbidden`.
async fn guard(
    State(state): AppStateRef,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    use axum::http::{Method, header};
    let port = state.port();
    let forbidden = |msg: String| {
        ApiError::new(StatusCode::FORBIDDEN, "forbidden", msg)
            .at(state.generation())
            .into_response()
    };
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok());
    if !host.is_some_and(|h| allowed_host(h, port)) {
        tracing::warn!(host = ?host, "refused request with a foreign Host");
        return forbidden(format!(
            "Host must be 127.0.0.1:{port}, localhost:{port} or [::1]:{port}"
        ));
    }
    if !matches!(*req.method(), Method::GET | Method::HEAD)
        && let Some(origin) = req.headers().get(header::ORIGIN)
        && !origin.to_str().is_ok_and(|o| allowed_origin(o, port))
    {
        tracing::warn!(?origin, method = %req.method(), "refused cross-origin request");
        return forbidden("cross-origin requests may only read".to_string());
    }
    next.run(req).await
}

fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/meta", get(meta))
        .route("/api/tree", get(tree))
        .route("/api/graph", get(graph))
        .route("/api/node", get(node))
        .route("/api/packages", get(packages))
        .route("/api/issues", get(issues))
        .route("/api/findings", get(findings))
        .route("/api/search", get(search))
        .route("/api/entries", get(entries))
        .route("/api/flow", get(flow))
        .route("/api/symbol", get(symbol))
        .route("/api/file-flows", get(file_flows))
        .route("/api/events", get(events))
        .route("/api/reindex", post(reindex))
        .route("/api", any(api_not_found))
        .route("/api/", any(api_not_found))
        .route("/api/{*rest}", any(api_not_found))
        .method_not_allowed_fallback(method_not_allowed)
        .fallback(crate::assets::serve)
}

/// Wrap `router` so a panicking handler answers 500 `internal` instead of dropping the
/// connection, and the server keeps running.
/// `state`, when given, supplies the `generation` of the error body.
pub fn with_isolation(router: Router, state: Option<Arc<AppState>>) -> Router {
    router.layer(CatchPanicLayer::custom(
        move |p: Box<dyn std::any::Any + Send + 'static>| {
            let msg = crate::panic_message(p.as_ref());
            tracing::error!(panic = %msg, "request handler panicked");
            let generation = state.as_ref().map_or(0, |s| s.generation());
            ApiError::internal(format!("handler panicked: {msg}"))
                .at(generation)
                .into_response()
        },
    ))
}

async fn api_not_found(State(state): AppStateRef, uri: axum::http::Uri) -> ApiError {
    ApiError::not_found(format!("no such endpoint: {}", uri.path())).at(state.generation())
}

async fn method_not_allowed(State(state): AppStateRef) -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "bad_request",
        "method not allowed",
    )
    .at(state.generation())
}

fn parse_query(raw: Option<String>) -> Result<Vec<(String, String)>, ApiError> {
    serde_urlencoded::from_str::<Vec<(String, String)>>(raw.as_deref().unwrap_or(""))
        .map_err(|e| ApiError::bad_request(format!("bad query string: {e}")))
}

fn param<'a>(q: &'a [(String, String)], key: &str) -> Option<&'a str> {
    q.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn num_param(q: &[(String, String)], key: &str) -> Result<Option<u64>, ApiError> {
    param(q, key)
        .map(|v| {
            v.parse::<u64>().map_err(|_| {
                ApiError::bad_request(format!("{key} must be a non-negative integer, got {v:?}"))
            })
        })
        .transpose()
}

/// The current snapshot, or the error a data endpoint returns without one.
fn snapshot(state: &AppState) -> Result<Arc<Snapshot>, ApiError> {
    if let Some(s) = state.snapshot() {
        return Ok(s);
    }
    if state.phase() == Phase::Error {
        let msg = state.error().unwrap_or_default();
        return Err(ApiError::internal(format!("indexing failed: {msg}")));
    }
    Err(ApiError {
        progress: Some(state.progress_json()),
        ..ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "indexing",
            "the first index is still running",
        )
    })
}

/// Run CPU-bound query work off the async threads. A panic in it is a 500, not a crash.
async fn blocking<T, F>(generation: u64, f: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r.map_err(|e| e.at(generation)),
        Err(e) => {
            let msg = match e.try_into_panic() {
                Ok(p) => crate::panic_message(p.as_ref()),
                Err(e) => e.to_string(),
            };
            tracing::error!(panic = %msg, "query panicked");
            Err(ApiError::internal(format!("query panicked: {msg}")).at(generation))
        }
    }
}

fn with_generation(mut body: Value, generation: u64) -> Value {
    if let Value::Object(m) = &mut body {
        m.insert("generation".into(), json!(generation));
    }
    body
}

async fn meta(State(state): AppStateRef) -> axum::Json<Value> {
    let snap = state.snapshot();
    let phase = state.phase();
    let mut body = json!({
        "generation": snap.as_ref().map_or(0, |s| s.generation),
        "status": phase.as_str(),
        "progress": state.progress_json(),
        "reindexing": state.running() && snap.is_some(),
        "repo": { "name": state.repo_name, "root": state.root_display },
        "indexed_at": snap.as_ref().and_then(|s| s.indexed_at.clone()),
        "index_ms": snap.as_ref().and_then(|s| s.index_ms),
        "counts": snap.as_ref().map(|s| json!(s.counts)),
        "languages": snap.as_ref().map_or_else(|| json!([]), |s| json!(s.languages)),
        "watching": state.watching(),
    });
    if let Some(ws) = snap.as_ref().and_then(|s| s.workspace.clone()) {
        body["workspace"] = ws;
    }
    if let Some(e) = state.error() {
        body["error"] = json!(e);
    }
    axum::Json(body)
}

async fn tree(State(state): AppStateRef, RawQuery(raw): RawQuery) -> ApiResult {
    let snap = snapshot(&state)?;
    let g = snap.generation;
    let q = parse_query(raw).map_err(|e| e.at(g))?;
    let depth = num_param(&q, "depth")
        .map_err(|e| e.at(g))?
        .map(|d| d.min(u32::MAX as u64) as u32);
    let root = param(&q, "root").map(str::to_string);
    blocking(g, move || {
        let r = match root.as_deref() {
            None | Some("") => crate::snapshot::ROOT,
            Some(id) => match snap.dir_of_id(id) {
                Some(d) => d,
                None if snap.file_of_id(id).is_some() => {
                    return Err(ApiError::bad_request(format!(
                        "root must be a directory, got file {id:?}"
                    )));
                }
                None => return Err(ApiError::not_found(format!("no directory {id:?}"))),
            },
        };
        Ok(axum::Json(
            json!({ "generation": g, "root": snap.tree(r, depth) }),
        ))
    })
    .await
}

/// `expanded` / `collapsed` values: each one is taken whole if it names a directory (so a
/// dir with a comma in its name can be passed as its own parameter), else split on commas.
fn dir_ids(snap: &Snapshot, q: &[(String, String)], key: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (k, v) in q {
        if k != key {
            continue;
        }
        if snap.dir_of_id(v).is_some() {
            out.push(v.clone());
        } else {
            out.extend(
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            );
        }
    }
    out
}

/// `depth`: a level of at least 1 (1 opens nothing, 2 opens the top-level dirs, …) or
/// `all`. Without it, 1. Only ASCII digits count as a number (no sign, no spaces, as the
/// mock API); a level past `u32::MAX` is `u32::MAX`.
fn depth_param(q: &[(String, String)]) -> Result<Depth, ApiError> {
    match param(q, "depth") {
        None => Ok(Depth::Levels(1)),
        Some(v) if v.eq_ignore_ascii_case("all") => Ok(Depth::All),
        Some(v) if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => {
            let n = v.parse::<u64>().unwrap_or(u64::MAX);
            if n >= 1 {
                Ok(Depth::Levels(n.min(u32::MAX as u64) as u32))
            } else {
                Err(bad_depth(v))
            }
        }
        Some(v) => Err(bad_depth(v)),
    }
}

fn bad_depth(v: &str) -> ApiError {
    ApiError::bad_request(format!(
        "depth must be an integer of at least 1 or \"all\", got {v:?}"
    ))
}

async fn graph(State(state): AppStateRef, RawQuery(raw): RawQuery) -> ApiResult {
    let snap = snapshot(&state)?;
    let g = snap.generation;
    let q = parse_query(raw).map_err(|e| e.at(g))?;
    let depth = depth_param(&q).map_err(|e| e.at(g))?;
    blocking(g, move || {
        let expanded = dir_ids(&snap, &q, "expanded");
        let collapsed = dir_ids(&snap, &q, "collapsed");
        let view = GraphQuery {
            depth,
            expanded: &expanded,
            collapsed: &collapsed,
        };
        Ok(axum::Json(with_generation(snap.graph_view(&view), g)))
    })
    .await
}

async fn node(State(state): AppStateRef, RawQuery(raw): RawQuery) -> ApiResult {
    let snap = snapshot(&state)?;
    let g = snap.generation;
    let q = parse_query(raw).map_err(|e| e.at(g))?;
    let Some(id) = param(&q, "id").map(str::to_string) else {
        return Err(ApiError::bad_request("missing id").at(g));
    };
    blocking(g, move || {
        snap.node_detail(&id)
            .map(|v| axum::Json(with_generation(v, g)))
            .ok_or_else(|| ApiError::not_found(format!("no node {id:?}")))
    })
    .await
}

async fn packages(State(state): AppStateRef) -> ApiResult {
    let snap = snapshot(&state)?;
    Ok(axum::Json(
        json!({ "generation": snap.generation, "packages": snap.packages }),
    ))
}

/// Default and maximum `limit` of `/api/issues`.
pub const ISSUES_LIMIT: (u64, u64) = (500, 100_000);
/// Default and maximum `limit` of `/api/search`.
pub const SEARCH_LIMIT: (u64, u64) = (50, 500);

async fn issues(State(state): AppStateRef, RawQuery(raw): RawQuery) -> ApiResult {
    let snap = snapshot(&state)?;
    let g = snap.generation;
    let q = parse_query(raw).map_err(|e| e.at(g))?;
    let limit = num_param(&q, "limit")
        .map_err(|e| e.at(g))?
        .unwrap_or(ISSUES_LIMIT.0)
        .min(ISSUES_LIMIT.1) as usize;
    blocking(g, move || {
        let gr = &snap.graph;
        let mut by_reason = serde_json::Map::new();
        for u in &gr.unresolved {
            let e = by_reason.entry(u.reason.as_str()).or_insert(json!(0));
            *e = json!(e.as_u64().unwrap_or(0) + 1);
        }
        let mut unresolved: Vec<_> = gr.unresolved.iter().collect();
        unresolved.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
        let unresolved: Vec<Value> = unresolved
            .into_iter()
            .take(limit)
            .map(|u| {
                json!({ "file": u.file, "line": u.line, "specifier": u.specifier,
                        "reason": u.reason.as_str(), "detail": u.detail })
            })
            .collect();
        let skipped: Vec<Value> = gr
            .skipped
            .iter()
            .take(limit)
            .map(|s| json!({ "path": s.path, "reason": s.reason.as_str(), "detail": s.detail }))
            .collect();
        Ok(axum::Json(json!({
            "generation": g,
            "unresolved": unresolved,
            "by_reason": by_reason,
            "diagnostics": gr.diagnostics,
            "skipped": skipped,
            "total": { "unresolved": gr.unresolved.len(), "skipped": gr.skipped.len() },
            "truncated": gr.unresolved.len() > limit || gr.skipped.len() > limit,
        })))
    })
    .await
}

/// Default and maximum `limit` of `/api/findings`.
pub const FINDINGS_LIMIT: (u64, u64) = (500, 10_000);

/// Findings of the current snapshot (cycles, layer violations, god modules, orphans…),
/// sorted: rule violations first, then by severity. Ids are API ids (`file:`, `dir:`,
/// `repo:`).
async fn findings(State(state): AppStateRef, RawQuery(raw): RawQuery) -> ApiResult {
    let snap = snapshot(&state)?;
    let g = snap.generation;
    let q = parse_query(raw).map_err(|e| e.at(g))?;
    let limit = num_param(&q, "limit")
        .map_err(|e| e.at(g))?
        .unwrap_or(FINDINGS_LIMIT.0)
        .min(FINDINGS_LIMIT.1) as usize;
    blocking(g, move || {
        let all = &snap.findings;
        let summary = cartograph_query::findings::summarize(all.iter());
        let items: Vec<Value> = all
            .iter()
            .take(limit)
            .map(|f| {
                let mut v = json!(f);
                // Store ids and API ids only differ for externals and packages, which
                // findings never name; translate anyway so the contract holds.
                if let Some(nodes) = v["nodes"].as_array_mut() {
                    for n in nodes {
                        if let Some(s) = n.as_str() {
                            *n = json!(crate::snapshot::api_id(s));
                        }
                    }
                }
                v
            })
            .collect();
        Ok(axum::Json(json!({
            "generation": g,
            "findings": items,
            "summary": summary,
            "config": { "origin": snap.config_origin, "error": snap.config_error },
            "total": all.len(),
            "truncated": all.len() > limit,
        })))
    })
    .await
}

/// Kinds `/api/search` can return; `sym` (functions and methods) only when asked for.
const SEARCH_KINDS: [&str; 4] = ["dir", "file", "ext", "sym"];

async fn search(State(state): AppStateRef, RawQuery(raw): RawQuery) -> ApiResult {
    let snap = snapshot(&state)?;
    let g = snap.generation;
    let q = parse_query(raw).map_err(|e| e.at(g))?;
    let limit = num_param(&q, "limit")
        .map_err(|e| e.at(g))?
        .unwrap_or(SEARCH_LIMIT.0)
        .min(SEARCH_LIMIT.1) as usize;
    let text = param(&q, "q").unwrap_or("").to_string();
    let kinds: Option<Vec<String>> = param(&q, "kinds").map(|v| {
        v.split(',')
            .map(|k| k.trim().to_ascii_lowercase())
            .filter(|k| !k.is_empty())
            .collect()
    });
    if let Some(bad) = kinds
        .iter()
        .flatten()
        .find(|k| !SEARCH_KINDS.contains(&k.as_str()))
    {
        return Err(ApiError::bad_request(format!(
            "kinds takes dir, file, ext and sym, got {bad:?}"
        ))
        .at(g));
    }
    blocking(g, move || {
        let Some(kinds) = kinds else {
            return Ok(axum::Json(
                json!({ "generation": g, "results": snap.search(&text, limit) }),
            ));
        };
        let mut results: Vec<Value> = Vec::new();
        if kinds.iter().any(|k| k == "sym")
            && let Some(fl) = &snap.flows
        {
            for h in cartograph_query::flow::search_functions(&fl.repo, &text, limit) {
                results.push(json!({
                    "id": h.id, "kind": "sym", "path": h.file, "name": h.name,
                    "line": h.line, "symbol_kind": h.kind, "callers": h.callers,
                }));
            }
        }
        if kinds.iter().any(|k| k != "sym") {
            let room = limit.saturating_sub(results.len());
            let all = snap.search(&text, SEARCH_LIMIT.1 as usize);
            results.extend(
                all.into_iter()
                    .filter(|r| {
                        r["kind"]
                            .as_str()
                            .is_some_and(|k| kinds.iter().any(|x| x == k))
                    })
                    .take(room),
            );
        }
        Ok(axum::Json(json!({ "generation": g, "results": results })))
    })
    .await
}

/// A query error of `cartograph-query` as an API error.
fn query_error(e: cartograph_query::QueryError) -> ApiError {
    use cartograph_query::QueryError;
    match e {
        QueryError::UnknownNode { .. } => ApiError::not_found(e.to_string()),
        QueryError::BadArgument(_)
        | QueryError::Ambiguous { .. }
        | QueryError::AmbiguousPath { .. } => ApiError::bad_request(e.to_string()),
    }
}

/// The flow layer of a snapshot, or the body that says the index has no symbols (200, so a
/// page that asks before re-indexing logs no error).
fn flow_layer(snap: &Snapshot) -> Result<&crate::snapshot::FlowLayer, Value> {
    match &snap.flows {
        Some(f) if f.repo.symbols.indexed => Ok(f),
        _ => Err(json!({ "generation": snap.generation, "indexed": false })),
    }
}

/// Flow entries outside test, fixture and example trees (`docs/api-f2.md`, 20).
async fn entries(State(state): AppStateRef) -> ApiResult {
    let snap = snapshot(&state)?;
    let g = snap.generation;
    blocking(g, move || {
        let fl = match flow_layer(&snap) {
            Ok(f) => f,
            Err(mut body) => {
                body["entries"] = json!([]);
                body["total"] = json!(0);
                body["hidden"] = json!(0);
                body["truncated"] = json!(false);
                return Ok(axum::Json(body));
            }
        };
        let list = cartograph_query::flow::entries(&fl.repo, &fl.calls);
        Ok(axum::Json(with_generation(json!(list), g)))
    })
    .await
}

/// Values of a repeatable list parameter (`?expand=a&expand=b` or `?expand=a,b`). A value
/// that holds a single id (a symbol name may have a comma) is kept whole.
fn list_param(q: &[(String, String)], key: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (k, v) in q {
        if k != key {
            continue;
        }
        let several = v.contains(",sym:") || v.contains(",file:");
        if several {
            out.extend(
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            );
        } else if !v.trim().is_empty() {
            out.push(v.trim().to_string());
        }
    }
    out
}

/// The bounded call flow from a function (`docs/api-f2.md`, 21).
async fn flow(State(state): AppStateRef, RawQuery(raw): RawQuery) -> ApiResult {
    let snap = snapshot(&state)?;
    let g = snap.generation;
    let q = parse_query(raw).map_err(|e| e.at(g))?;
    let Some(from) = param(&q, "from").map(str::to_string) else {
        return Err(ApiError::bad_request("missing from").at(g));
    };
    let depth = num_param(&q, "depth").map_err(|e| e.at(g))?;
    let max = num_param(&q, "max").map_err(|e| e.at(g))?;
    let expand = list_param(&q, "expand");
    blocking(g, move || {
        let fl = match flow_layer(&snap) {
            Ok(f) => f,
            Err(body) => return Ok(axum::Json(body)),
        };
        let opts = cartograph_query::flow::FlowOptions {
            depth: depth.map_or(cartograph_query::flow::DEFAULT_DEPTH, |d| {
                d.min(u64::from(u32::MAX)) as u32
            }),
            max_nodes: max.map_or(cartograph_query::flow::DEFAULT_NODES, |m| {
                m.min(cartograph_query::flow::MAX_NODES as u64) as usize
            }),
            expand,
        };
        let f =
            cartograph_query::flow::flow(&fl.repo, &fl.calls, &from, &opts).map_err(query_error)?;
        let mut body = with_generation(json!(f), g);
        body["indexed"] = json!(true);
        Ok(axum::Json(body))
    })
    .await
}

/// One function: callers (production first, tests counted), callees, git (21).
async fn symbol(State(state): AppStateRef, RawQuery(raw): RawQuery) -> ApiResult {
    let snap = snapshot(&state)?;
    let g = snap.generation;
    let q = parse_query(raw).map_err(|e| e.at(g))?;
    let Some(id) = param(&q, "id").map(str::to_string) else {
        return Err(ApiError::bad_request("missing id").at(g));
    };
    blocking(g, move || {
        let fl = match flow_layer(&snap) {
            Ok(f) => f,
            Err(body) => return Ok(axum::Json(body)),
        };
        let card =
            cartograph_query::flow::symbol_card(&fl.repo, &fl.calls, &id).map_err(query_error)?;
        let mut body = with_generation(json!(card), g);
        body["indexed"] = json!(true);
        Ok(axum::Json(body))
    })
    .await
}

/// A file's functions and the flows that reach it, for "Ver flujos" on the map (21).
async fn file_flows(State(state): AppStateRef, RawQuery(raw): RawQuery) -> ApiResult {
    let snap = snapshot(&state)?;
    let g = snap.generation;
    let q = parse_query(raw).map_err(|e| e.at(g))?;
    let Some(id) = param(&q, "id").map(str::to_string) else {
        return Err(ApiError::bad_request("missing id").at(g));
    };
    blocking(g, move || {
        let fl = match flow_layer(&snap) {
            Ok(f) => f,
            Err(body) => return Ok(axum::Json(body)),
        };
        let ff =
            cartograph_query::flow::file_flows(&fl.repo, &fl.calls, &id).map_err(query_error)?;
        Ok(axum::Json(with_generation(json!(ff), g)))
    })
    .await
}

async fn events(
    State(state): AppStateRef,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    // Subscribe before reading the state, so nothing between `hello` and the stream is lost.
    let rx = state.subscribe();
    let hello = json!({ "generation": state.generation(), "status": state.phase().as_str() });
    let first = tokio_stream::once(Ok(sse_event("hello", &hello)));
    let rest = BroadcastStream::new(rx).filter_map(|r| match r {
        Ok(ev) => Some(Ok(sse_event(ev.name, &ev.data))),
        // A slow client missed events; it still gets the next `index-finished`.
        Err(e) => {
            tracing::debug!(error = %e, "sse client lagged");
            None
        }
    });
    Sse::new(first.chain(rest)).keep_alive(KeepAlive::new().interval(KEEPALIVE))
}

fn sse_event(name: &str, data: &Value) -> Event {
    Event::default().event(name).data(data.to_string())
}

async fn reindex(State(state): AppStateRef) -> Response {
    state.scheduler.request(Reason::Manual, std::iter::empty());
    (
        StatusCode::ACCEPTED,
        axum::Json(json!({ "accepted": true, "generation": state.generation() })),
    )
        .into_response()
}
