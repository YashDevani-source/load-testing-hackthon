//! Shared application state, one instance per process, handed to every actix
//! worker as `web::Data<AppState>` (an `Arc`).
//!
//! SQLite (through `writer`) is the source of truth in both read modes. In
//! `READ_SOURCE=memory`, `mem` holds read caches that are loaded from SQLite at
//! startup and updated only after a write has committed, under the writer lock.
//! If the process dies, the caches die with it and are rebuilt from SQLite on the
//! next start: memory is never the only copy (spec section 09).

pub mod categories;
pub mod memory;
pub mod orders;
pub mod products;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use rusqlite::Connection;

use crate::config::ReadSource;

pub use memory::MemoryStores;
pub use orders::MemOrder;
pub use products::MemProduct;

pub struct AppState {
    /// The only write connection. Hold the guard for the statements of one
    /// request (microseconds), never across an `.await`.
    pub writer: Mutex<Connection>,
    /// Database file, for opening per-worker read connections.
    pub db_path: PathBuf,
    pub read_source: ReadSource,
    /// Set once, after migrations and (in memory mode) loading. `/health`
    /// reports 503 until then.
    pub ready: AtomicBool,
    /// `Some` only in memory mode.
    pub mem: Option<MemoryStores>,
}

impl AppState {
    pub fn is_ready(&self) -> bool {
        // Acquire pairs with the Release in mark_ready: a worker that sees `true`
        // also sees everything written before it.
        self.ready.load(Ordering::Acquire)
    }

    pub fn mark_ready(&self) {
        self.ready.store(true, Ordering::Release);
    }
}
