//! lss-server: the binary systemd runs.
//!
//! Startup order matters for `/health`:
//!   1. read config, start logging
//!   2. open the database (creates file + schema), start the checkpoint thread
//!   3. bind 0.0.0.0:8080 and start serving: `/health` answers 503 from here on
//!   4. load in-memory state on a separate thread, then flip the ready flag
//!      -> `/health` answers 200 and the load generator may start
//!
//! Shutdown (SIGTERM from systemd, or Ctrl-C): actix stops accepting, finishes
//! in-flight requests, then we stop the checkpoint thread and run a final
//! TRUNCATE checkpoint so the next start has no WAL to replay.
//! A crash (kill -9) skips all of that, which is fine: every acknowledged order is
//! already in the WAL, and SQLite replays it on the next open.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::todo,
    clippy::unimplemented
)]

use std::io::IsTerminal;
use std::process::ExitCode;
use std::time::Duration;

use actix_web::{App, HttpServer, web};
use tracing::{error, info};

use lss_server::config::Config;
use lss_server::db::{Db, checkpoint};
use lss_server::http;
use lss_server::state::AppState;

// mimalloc: fast per-thread allocation for the many small, short-lived buffers an
// HTTP server makes, and no glibc malloc arena contention between workers.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
/// Graceful shutdown budget. Must be below systemd's TimeoutStopSec=10, or systemd
/// SIGKILLs us before the final checkpoint.
const SHUTDOWN_TIMEOUT_SECS: u64 = 5;

fn main() -> ExitCode {
    let config = match Config::from_env() {
        Ok(c) => c,
        Err(err) => {
            // Logging isn't up yet (LOG_LEVEL itself may be the bad value).
            eprintln!("lss-server: {err}");
            return ExitCode::from(2);
        }
    };

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(&config.log_level))
        // Colours only on a terminal; under systemd they'd be junk in journalctl.
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    match actix_web::rt::System::new().block_on(run(config)) {
        Ok(()) => {
            info!("stopped cleanly");
            ExitCode::SUCCESS
        }
        Err(err) => {
            // Non-zero exit -> systemd restarts us (Restart=always).
            error!(error = %err, "fatal");
            ExitCode::FAILURE
        }
    }
}

async fn run(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    info!(
        host = %config.host,
        port = config.port,
        workers = config.workers,
        database = %config.database_path.display(),
        "starting"
    );

    let db = Db::open(&config.database_path)?;
    let checkpointer = checkpoint::Checkpointer::spawn(db.path(), checkpoint::INTERVAL)?;
    let state = web::Data::new(AppState::new(db));

    let app_state = state.clone();
    let server = HttpServer::new(move || {
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
    .shutdown_timeout(SHUTDOWN_TIMEOUT_SECS)
    .bind((config.host, config.port))?
    .run();

    // Load state off the async runtime so /health can answer 503 meanwhile.
    let loader_state = state.clone();
    std::thread::Builder::new()
        .name("lss-loader".into())
        .spawn(move || match loader_state.load_from_db() {
            Ok(()) => {
                loader_state.mark_ready();
                info!("ready");
            }
            Err(err) => {
                // Without state we can't answer correctly. Exit and let systemd
                // restart us; the WAL keeps everything committed so far.
                error!(error = %err, "loading state failed");
                std::process::exit(1);
            }
        })?;

    server.await?;

    info!("http server stopped; final checkpoint");
    checkpointer.stop();
    let (busy, wal_frames, done) = state.db.checkpoint_truncate()?;
    info!(
        busy,
        wal_frames,
        checkpointed = done,
        "final checkpoint done"
    );
    Ok(())
}
