//! lss-server: the binary systemd runs.
//!
//! Startup: read config, start logging, then `build_server` opens and migrates
//! the database, loads the memory caches (READ_SOURCE=memory), marks the server
//! ready and binds 0.0.0.0:8080. Shutdown (SIGTERM from systemd, or Ctrl-C):
//! actix drains in-flight requests, then a final WAL checkpoint runs.

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

use tracing::{error, info};

use lss_server::config::Config;

// mimalloc: fast per-thread allocation for the many small, short-lived buffers an
// HTTP server makes, and no glibc malloc arena contention between workers.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[actix_web::main]
async fn main() -> ExitCode {
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

    info!(
        workers = config.workers,
        database = %config.database_path.display(),
        read_source = %config.read_source,
        "starting"
    );
    let read_source = config.read_source;
    let running = match lss_server::build_server(config) {
        Ok(r) => r,
        Err(err) => {
            // Non-zero exit -> systemd restarts us (Restart=always).
            error!(error = format!("{err:#}"), "startup failed");
            return ExitCode::FAILURE;
        }
    };
    info!(addr = %running.addr, %read_source, "ready");

    match running.run().await {
        Ok(()) => {
            info!("stopped cleanly");
            ExitCode::SUCCESS
        }
        Err(err) => {
            error!(error = format!("{err:#}"), "fatal");
            ExitCode::FAILURE
        }
    }
}
