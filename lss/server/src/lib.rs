//! Last Server Standing: a flash-sale API server.
//!
//! Everything lives in this library so tests can start the real server;
//! `main.rs` only adds the allocator, logging and the async entry point.
//!
//! Layout:
//! - [`config`]   environment variables, validated once at startup
//! - [`errors`]   the spec's error codes and how they render
//! - [`validate`] pure input validation (no I/O)
//! - [`render`]   response bodies, shared by both read modes
//! - [`http`]     route table, JSON response helpers, query-string parsing
//! - [`routes`]   one module per resource
//! - [`db`]       SQLite: open, migrations, writer/reader/checkpoint connections
//! - [`state`]    shared app state and the memory-mode read caches

// A panic inside a request is a dropped connection, i.e. an error in the scoring.
// These lints turn every "this can't fail" shortcut into a compile error, so each
// failure path has to become a spec error or a logged 500.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::todo,
    clippy::unimplemented
)]

pub mod config;
pub mod db;
pub mod errors;
pub mod http;
pub mod render;
pub mod routes;
pub mod state;
pub mod validate;

use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use actix_web::dev::{Server, ServerHandle};
use actix_web::{App, HttpServer, web};
use anyhow::Context;
use parking_lot::Mutex;

use crate::config::{Config, ReadSource};
use crate::db::checkpoint::{self, Checkpointer};
use crate::state::{AppState, MemoryStores};

/// TCP accept queue. The kernel caps it at net.core.somaxconn, which
/// setup-host.sh raises to the same value, so bursts of new connections queue
/// instead of being refused.
const LISTEN_BACKLOG: u32 = 65_535;
/// Per worker. The load generator opens up to 5,000 connections; actix refuses to
/// accept beyond this, so it must stay far above that.
const MAX_CONNECTIONS_PER_WORKER: usize = 25_000;
/// Longer than any client idle gap we expect, so connections get reused instead
/// of re-handshaking under load.
const KEEP_ALIVE: Duration = Duration::from_secs(75);
/// A client that hasn't sent its full request head in 5 s is dropped, so slow
/// or dead connections can't pile up.
const CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Graceful shutdown budget. Must be below systemd's TimeoutStopSec=10, or systemd
/// SIGKILLs us before the final checkpoint.
const SHUTDOWN_TIMEOUT_SECS: u64 = 5;

/// A bound, ready server that hasn't started running yet.
pub struct RunningServer {
    pub addr: SocketAddr,
    pub handle: ServerHandle,
    /// The future to run (see [`RunningServer::run`]).
    pub server: Server,
    /// Kept so [`RunningServer::run`] can do the final checkpoint after the
    /// HTTP server has drained.
    pub state: web::Data<AppState>,
    pub checkpointer: Checkpointer,
}

impl RunningServer {
    /// Serves until stopped (SIGTERM/SIGINT or `handle.stop`), then stops the
    /// background checkpointer and runs a final TRUNCATE checkpoint so the next
    /// start has no WAL to replay. A crash skips this, which is safe: every
    /// acknowledged write is already in the WAL.
    pub async fn run(self) -> anyhow::Result<()> {
        self.server.await.context("http server")?;
        self.checkpointer.stop();
        let (busy, wal_frames, done) =
            checkpoint::run(&self.state.writer.lock(), checkpoint::Mode::Truncate)?;
        tracing::info!(
            busy,
            wal_frames,
            checkpointed = done,
            "final checkpoint done"
        );
        Ok(())
    }
}

/// Opens the database, migrates it and, in memory mode, loads the read caches.
/// The returned state is not yet marked ready.
pub fn open_state(config: &Config) -> anyhow::Result<AppState> {
    let conn = db::open(&config.database_path)?;
    let mem = match config.read_source {
        ReadSource::Memory => Some(MemoryStores::load(&conn).context("loading memory stores")?),
        ReadSource::Sqlite => None,
    };
    Ok(AppState {
        writer: Mutex::new(conn),
        db_path: config.database_path.clone(),
        read_source: config.read_source,
        ready: AtomicBool::new(false),
        mem,
    })
}

/// Opens the DB, runs migrations, loads memory stores if READ_SOURCE=memory,
/// sets the ready flag, binds, and returns the server without running it.
///
/// Must be called inside an actix system (`#[actix_web::main]` or
/// `actix_web::rt::System`).
pub fn build_server(config: Config) -> anyhow::Result<RunningServer> {
    let state = web::Data::new(open_state(&config)?);
    let checkpointer = Checkpointer::spawn(&config.database_path, checkpoint::INTERVAL)
        .context("starting checkpoint thread")?;
    // Everything /health promises (DB open, migrated, caches loaded) is true now.
    state.mark_ready();

    let app_state = state.clone();
    let http = HttpServer::new(move || {
        // Deliberately no Logger middleware: a log line per request at 10k req/s
        // is 10k writes a second competing with real work.
        App::new()
            .app_data(app_state.clone())
            .configure(http::configure)
    })
    .workers(config.workers)
    .backlog(LISTEN_BACKLOG)
    .max_connections(MAX_CONNECTIONS_PER_WORKER)
    .keep_alive(KEEP_ALIVE)
    .client_request_timeout(CLIENT_REQUEST_TIMEOUT)
    .shutdown_timeout(SHUTDOWN_TIMEOUT_SECS)
    .bind((config.host, config.port))
    .with_context(|| format!("binding {}:{}", config.host, config.port))?;

    let addr = http
        .addrs()
        .first()
        .copied()
        .context("server bound to no address")?;
    let server = http.run();
    let handle = server.handle();
    Ok(RunningServer {
        addr,
        handle,
        server,
        state,
        checkpointer,
    })
}
