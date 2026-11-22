//! `cartograph mcp`: a stdio MCP server over `cartograph-query`, for coding agents.
//!
//! Robustness: stdout carries only the protocol (logs go to stderr through
//! `tracing`); the server answers from the first message on and indexes in the background
//! ("indexing N %" until the first index exists); every query runs on a blocking thread
//! under a timeout with panics caught ([`guard`]); before answering, the files a request
//! names are checked against the index and re-indexed first when they changed, and the
//! watcher re-indexes on any other change ([`engine`]).

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

pub mod annotations;
pub mod base;
pub mod engine;
pub mod guard;
pub mod map;
pub mod stats;
pub mod tools;

pub use engine::{Engine, Options, server_running};
pub use tools::Cartograph;

use rmcp::ServiceExt;
use std::time::Duration;

/// Serve MCP on stdin/stdout until the client closes stdin.
pub fn run_stdio(opts: Options) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start the async runtime: {e}"))?;
    let engine = Engine::new(opts)?;
    let res = rt.block_on(async {
        engine.start();
        tracing::info!(root = %engine.root.display(), data = %engine.data_dir.display(), "cartograph mcp ready");
        let service = Cartograph::new(engine.clone())
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| format!("MCP handshake failed: {e}"))?;
        let reason = service.waiting().await;
        tracing::info!(?reason, "client gone; stopping");
        engine.map.stop().await;
        Ok(())
    });
    // A running index stops at its next check; a blocked stdin read or a query thread is
    // not waited for longer than this.
    engine.cancel();
    rt.shutdown_timeout(Duration::from_millis(500));
    res
}
