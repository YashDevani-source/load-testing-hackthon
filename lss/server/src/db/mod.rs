//! The SQLite database: open, pragmas, schema, and the one shared connection.
//!
//! Why SQLite in-process: no network hop, no second process competing for our one
//! physical core, and a durable WAL on local disk. Why one connection behind a
//! mutex: every write is serialised anyway (SQLite has a single writer), and doing
//! it with an uncontended `parking_lot::Mutex` is far cheaper than SQLite's own
//! file-lock dance between connections. Reads are served from memory (task 2), so
//! the mutex is mostly taken by `POST /orders` and `POST /products`.

pub mod checkpoint;

use std::fmt;
use std::path::{Path, PathBuf};

use parking_lot::{Mutex, MutexGuard};
use rusqlite::Connection;

/// The schema, compiled into the binary so startup never depends on files next to it.
pub const SCHEMA: &str = include_str!("schema.sql");

/// Statements are cached per connection by `prepare_cached`. We have only a
/// handful of distinct statements; 32 keeps all of them hot with room to spare.
const STATEMENT_CACHE_CAPACITY: usize = 32;

/// Connection settings, applied once at open. Kept as data so tests can assert
/// every one of them took effect.
///
/// - `journal_mode=WAL`: readers never block the writer and commits are sequential
///   appends to one file.
/// - `synchronous=NORMAL`: in WAL mode a commit is not fsynced individually; the
///   WAL is fsynced at checkpoint. A process crash (kill -9) loses nothing because
///   the WAL is already in the OS page cache; only a power loss could lose the
///   last commits. That is the trade the contest allows (data "writes to disk").
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

/// The database handle shared by all workers.
pub struct Db {
    conn: Mutex<Connection>,
    path: PathBuf,
}

#[derive(Debug)]
pub enum DbError {
    /// Creating the database directory failed.
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    /// SQLite refused WAL mode (e.g. the path is on a filesystem without shared
    /// memory support). We refuse to run without it rather than run slowly.
    NotWal(String),
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "database directory: {e}"),
            Self::Sqlite(e) => write!(f, "sqlite: {e}"),
            Self::NotWal(mode) => write!(f, "journal_mode is {mode:?}, expected \"wal\""),
        }
    }
}

impl std::error::Error for DbError {}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

impl From<std::io::Error> for DbError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl Db {
    /// Opens (creating if needed) the database at `path`, applies the pragmas and
    /// the schema. Safe to call on an existing database: nothing is overwritten.
    pub fn open(path: &Path) -> Result<Self, DbError> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        apply_pragmas(&conn)?;
        let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(DbError::NotWal(mode));
        }
        conn.execute_batch(SCHEMA)?;
        conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
        Ok(Self {
            conn: Mutex::new(conn),
            path: path.to_owned(),
        })
    }

    /// Locks the connection. Hold the guard only for the statements of one
    /// request (microseconds); never across an `.await`.
    pub fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Copies the whole WAL into the main file and truncates the WAL to zero
    /// bytes. Run on graceful shutdown so the next start has nothing to replay.
    /// Returns `(busy, wal_frames, checkpointed_frames)` as SQLite reports them.
    pub fn checkpoint_truncate(&self) -> rusqlite::Result<(i64, i64, i64)> {
        checkpoint::run(&self.lock(), checkpoint::Mode::Truncate)
    }
}

/// Runs each `PRAGMA name=value`. Some pragmas answer with a row (journal_mode,
/// busy_timeout) and some don't; stepping through the result handles both.
fn apply_pragmas(conn: &Connection) -> rusqlite::Result<()> {
    for (name, value) in PRAGMAS {
        let mut stmt = conn.prepare(&format!("PRAGMA {name}={value}"))?;
        let mut rows = stmt.query([])?;
        while rows.next()?.is_some() {}
    }
    Ok(())
}
