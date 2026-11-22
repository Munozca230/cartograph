//! `cartograph serve`: a local HTTP server (127.0.0.1 only) with the JSON API of
//! `docs/api-f2.md`, server-sent events, a file watcher that keeps the index fresh, and
//! the embedded web frontend.
//!
//! The HTTP server is up immediately; the first index runs in the background and data
//! endpoints answer 503 `indexing` until it is done. After every index an immutable
//! [`snapshot::Snapshot`] is built from the store and swapped in atomically, so readers
//! never wait on indexing.

// Library code must not panic or write to stdout/stderr (logs go through `tracing`).
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr
    )
)]

pub mod api;
pub mod assets;
pub mod indexer;
pub mod scheduler;
pub mod snapshot;
pub mod state;
pub mod watch;

use crate::indexer::IndexFn;
use crate::scheduler::Reason;
use crate::state::AppState;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// First port tried when none is given.
pub const DEFAULT_PORT: u16 = 7878;
/// How many ports after the first one are tried when it is taken (and no port was given).
pub const PORT_TRIES: u16 = 10;

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("{0} is not a directory")]
    NotADirectory(PathBuf),
    #[error("port {port} on 127.0.0.1 is not available: {source}")]
    PortInUse { port: u16, source: std::io::Error },
    #[error("no free port on 127.0.0.1 between {from} and {to}")]
    NoFreePort { from: u16, to: u16 },
    #[error("i/o error on {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot start the async runtime: {0}")]
    Runtime(std::io::Error),
}

/// What to serve and how.
#[derive(Debug, Clone)]
pub struct ServeConfig {
    pub root: PathBuf,
    /// `None` = `<root>/.cartograph`.
    pub data_dir: Option<PathBuf>,
    /// Explicit port: used as is, or the server fails. `Some(0)` lets the OS pick.
    pub port: Option<u16>,
    /// First port tried when `port` is `None` (then up to [`PORT_TRIES`] more).
    pub base_port: u16,
    /// Watch the repo and re-index on changes.
    pub watch: bool,
    pub max_file_bytes: u64,
    /// Read git history after each index (churn, owners, hidden coupling).
    pub git: bool,
}

impl ServeConfig {
    pub fn new(root: impl Into<PathBuf>) -> ServeConfig {
        ServeConfig {
            root: root.into(),
            data_dir: None,
            port: None,
            base_port: DEFAULT_PORT,
            watch: true,
            max_file_bytes: cartograph_index::walk::DEFAULT_MAX_FILE_BYTES,
            git: true,
        }
    }
}

/// Text of a panic payload.
pub fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// Absolute path without Windows' `\\?\` prefix, resolving `.` and symlinks when the path
/// exists.
pub fn normalize(p: &Path) -> Result<PathBuf, ServeError> {
    dunce::canonicalize(p).map_err(|source| ServeError::Io {
        path: p.to_path_buf(),
        source,
    })
}

/// Bind a listener on 127.0.0.1. With `port`, exactly that port (or an error); otherwise
/// `base`, then the next [`PORT_TRIES`] ports, logging on stderr which one was used.
pub fn bind(port: Option<u16>, base: u16) -> Result<std::net::TcpListener, ServeError> {
    let try_bind = |p: u16| std::net::TcpListener::bind(("127.0.0.1", p));
    if let Some(p) = port {
        return try_bind(p).map_err(|source| ServeError::PortInUse { port: p, source });
    }
    let last = base.saturating_add(PORT_TRIES);
    for p in base..=last {
        match try_bind(p) {
            Ok(l) => {
                if p != base {
                    tracing::warn!(port = p, wanted = base, "port {base} is taken; using {p}");
                }
                return Ok(l);
            }
            Err(e) => tracing::debug!(port = p, error = %e, "port unavailable"),
        }
    }
    Err(ServeError::NoFreePort {
        from: base,
        to: last,
    })
}

/// A running server. Dropping it stops the server, the worker and the watcher.
pub struct Running {
    pub addr: SocketAddr,
    pub state: Arc<AppState>,
    server: Option<JoinHandle<()>>,
    /// The indexing worker (none when the server is fed from outside, see [`start_fed`]).
    worker: Option<JoinHandle<()>>,
    watcher: Option<watch::Watcher>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl Running {
    /// Wait until the server task ends (it only ends after [`Running::stop`] or an error).
    pub async fn wait(&mut self) {
        if let Some(h) = self.server.as_mut() {
            let _ = h.await;
            self.server = None;
        }
    }

    /// Stop accepting connections and end everything. Open event streams never finish on
    /// their own, so the graceful phase is capped at two seconds.
    ///
    /// A running index is cancelled: it stops at its next check (between files or
    /// phases) instead of finishing. The blocking thread it runs on is not waited for here;
    /// [`run_blocking`] bounds that with `shutdown_timeout`.
    pub async fn stop(mut self) {
        self.state.progress.cancel();
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.server.take() {
            let abort = h.abort_handle();
            if tokio::time::timeout(GRACEFUL_STOP, h).await.is_err() {
                abort.abort();
            }
        }
        // Aborting the worker drops its progress ticker with it (see `indexer::Ticker`).
        if let Some(w) = self.worker.take() {
            w.abort();
            let _ = w.await;
        }
        self.watcher = None;
    }
}

/// How long [`Running::stop`] lets open connections finish.
pub const GRACEFUL_STOP: Duration = Duration::from_secs(1);

impl Drop for Running {
    fn drop(&mut self) {
        self.state.progress.cancel();
        if let Some(h) = &self.server {
            h.abort();
        }
        if let Some(w) = &self.worker {
            w.abort();
        }
    }
}

/// Start serving `config` with the real indexer.
pub async fn start(config: ServeConfig) -> Result<Running, ServeError> {
    start_with(config, indexer::default_index_fn()).await
}

/// Start serving with a custom index function (tests use it to slow or count runs).
pub async fn start_with(config: ServeConfig, index_fn: IndexFn) -> Result<Running, ServeError> {
    let (listener, addr, state) = listen(&config)?;

    // The watcher starts before the first index, so edits made during it queue a run.
    let watcher = if config.watch {
        match watch::start(state.clone()) {
            Ok(w) => {
                state.set_watching(true);
                Some(w)
            }
            Err(e) => {
                tracing::error!(error = %e, "file watcher failed to start; serving without it");
                None
            }
        }
    } else {
        None
    };

    state.scheduler.request(Reason::Startup, std::iter::empty());
    let worker = tokio::spawn(indexer::run_worker(
        state.clone(),
        index_fn,
        config.max_file_bytes,
        config.git,
    ));
    Ok(serve(listener, addr, state, Some(worker), watcher))
}

/// Start the HTTP server alone, fed by an indexer that runs elsewhere in the process (the
/// MCP server's): no worker and no watcher of its own, so the repo is never indexed twice.
///
/// The caller publishes each new index with [`indexer::publish`] (data endpoints answer
/// 503 `indexing` until the first one) and takes the `POST /api/reindex` requests from
/// `state.scheduler`. `config.watch` only says what `/api/meta` reports as `watching`
/// (whether the feeder watches the repo); `max_file_bytes` and `git` are the feeder's.
pub async fn start_fed(config: ServeConfig) -> Result<Running, ServeError> {
    let (listener, addr, state) = listen(&config)?;
    state.set_watching(config.watch);
    Ok(serve(listener, addr, state, None, None))
}

/// Check the root, create the data dir, bind the port and build the state.
fn listen(
    config: &ServeConfig,
) -> Result<(tokio::net::TcpListener, SocketAddr, Arc<AppState>), ServeError> {
    if !config.root.is_dir() {
        return Err(ServeError::NotADirectory(config.root.clone()));
    }
    let root = normalize(&config.root)?;
    let data_dir = cartograph_index::data_dir(&root, config.data_dir.as_deref());
    std::fs::create_dir_all(&data_dir).map_err(|source| ServeError::Io {
        path: data_dir.clone(),
        source,
    })?;
    let data_dir = normalize(&data_dir)?;

    let std_listener = bind(config.port, config.base_port)?;
    std_listener
        .set_nonblocking(true)
        .map_err(|source| ServeError::Io {
            path: PathBuf::from("127.0.0.1"),
            source,
        })?;
    let listener =
        tokio::net::TcpListener::from_std(std_listener).map_err(|source| ServeError::Io {
            path: PathBuf::from("127.0.0.1"),
            source,
        })?;
    let addr = listener.local_addr().map_err(|source| ServeError::Io {
        path: PathBuf::from("127.0.0.1"),
        source,
    })?;

    let state = Arc::new(AppState::new(
        cartograph_index::repo_name(&root),
        root,
        data_dir,
    ));
    state.set_port(addr.port());
    Ok((listener, addr, state))
}

/// Serve the API and the frontend on `listener` until [`Running::stop`].
fn serve(
    listener: tokio::net::TcpListener,
    addr: SocketAddr,
    state: Arc<AppState>,
    worker: Option<JoinHandle<()>>,
    watcher: Option<watch::Watcher>,
) -> Running {
    let app = api::router(state.clone());
    let (tx, rx) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let res = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
        if let Err(e) = res {
            tracing::error!(error = %e, "http server stopped");
        }
    });
    tracing::info!(%addr, root = %state.root_display, "serving");
    Running {
        addr,
        state,
        server: Some(server),
        worker,
        watcher,
        shutdown: Some(tx),
    }
}

/// Run until Ctrl-C. `on_ready` gets the bound address once the server listens.
pub fn run_blocking(
    config: ServeConfig,
    on_ready: impl FnOnce(SocketAddr),
) -> Result<(), ServeError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(ServeError::Runtime)?;
    let res = rt.block_on(async move {
        let mut running = start(config).await?;
        on_ready(running.addr);
        tokio::select! {
            r = tokio::signal::ctrl_c() => {
                if let Err(e) = r {
                    tracing::error!(error = %e, "cannot listen for Ctrl-C");
                }
                tracing::info!("shutting down");
            }
            () = running.wait() => {}
        }
        running.stop().await;
        Ok(())
    });
    // A cancelled index may still be finishing a file or a phase on a blocking thread.
    // Dropping the runtime would wait for it; this waits at most two seconds.
    rt.shutdown_timeout(Duration::from_secs(2));
    res
}
