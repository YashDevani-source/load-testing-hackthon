//! The SQLite database: open, pragmas, migrations, and the connections.
//!
//! SQLite is the only source of truth. Three kinds of connection exist:
//! - **the writer**: exactly one, behind `AppState::writer` (a parking_lot mutex).
//!   Every write and every order decision goes through it. SQLite allows one
//!   writer at a time anyway; an uncontended mutex is far cheaper than SQLite's
//!   file-lock handoff between connections.
//! - **readers** ([`reader`]): one read-only connection per actix worker thread,
//!   used by the read endpoints in `READ_SOURCE=sqlite` mode. WAL lets them run in
//!   parallel with the writer.
//! - **the checkpointer** ([`checkpoint`]): copies the WAL into the main file in
//!   the background so no request pays for it.

pub mod checkpoint;
pub mod errors;
pub mod migrate;
pub mod reader;

use std::path::Path;

use anyhow::Context;
use rusqlite::Connection;

/// The version-1 (task 1) schema. Migration step 0 -> 1 applies it; later
/// versions are built on top of it in [`migrate`].
pub const SCHEMA_V1: &str = include_str!("schema.sql");

/// Statements are cached per connection by `prepare_cached`. We have only a
/// handful of distinct statements; 32 keeps all of them hot with room to spare.
const STATEMENT_CACHE_CAPACITY: usize = 32;

/// Writer connection settings, applied once at open. Kept as data so tests can
/// assert every one of them took effect.
///
/// - `journal_mode=WAL`: readers never block the writer and commits are sequential
///   appends to one file.
/// - `synchronous=NORMAL`: in WAL mode a commit is not fsynced individually; the
///   WAL is fsynced at checkpoint. A process crash (kill -9) loses nothing because
///   the WAL is already in the OS page cache; only a power loss could lose the
///   last commits.
/// - `wal_autocheckpoint=0`: no request ever pays for a checkpoint inside its
///   commit; [`checkpoint`] does it in the background instead.
/// - `temp_store=MEMORY`: sorts/temp b-trees never touch disk.
/// - `busy_timeout=5000`: if the checkpoint connection ever holds a lock we
///   need, wait for it instead of failing with SQLITE_BUSY.
pub const PRAGMAS: &[(&str, &str)] = &[
    ("journal_mode", "WAL"),
    ("synchronous", "NORMAL"),
    ("wal_autocheckpoint", "0"),
    ("temp_store", "MEMORY"),
    ("busy_timeout", "5000"),
];

/// Opens (creating if needed) the writer connection at `path`, applies the
/// pragmas and migrates the schema to the current version. Safe on an existing
/// database: nothing is overwritten.
pub fn open(path: &Path) -> anyhow::Result<Connection> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating database directory {}", dir.display()))?;
    }
    let mut conn =
        Connection::open(path).with_context(|| format!("opening database {}", path.display()))?;
    for (name, value) in PRAGMAS {
        // execute_batch steps through pragmas that answer with a row
        // (journal_mode, busy_timeout) as well as those that don't.
        conn.execute_batch(&format!("PRAGMA {name}={value}"))
            .with_context(|| format!("PRAGMA {name}={value}"))?;
    }
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    // Refuse to run without WAL (e.g. a filesystem without shared memory)
    // rather than silently serialise readers behind the writer.
    anyhow::ensure!(
        mode.eq_ignore_ascii_case("wal"),
        "journal_mode is {mode:?}, expected \"wal\""
    );
    migrate::migrate(&mut conn)?;
    conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
    Ok(conn)
}
