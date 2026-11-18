//! Per-worker read-only connections for `READ_SOURCE=sqlite`.
//!
//! Each actix worker thread lazily opens its own read-only connection and keeps
//! it for the life of the thread, so reads never touch the writer mutex. WAL lets
//! these readers run in parallel with the writer, and each statement sees the
//! latest committed data (a read in autocommit mode starts a fresh snapshot).
//!
//! SQLite's page cache and mmap below are the database's own internals, not an
//! application cache: they hold file pages, are invalidated by SQLite itself when
//! the writer commits, and can never return data older than the last commit.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::errors::ApiError;

thread_local! {
    /// This thread's reader, with the path it was opened for. The path is kept so
    /// a thread can never read the wrong database (tests run several servers in
    /// one process; in production there is only one path).
    static READER: RefCell<Option<(PathBuf, Connection)>> = const { RefCell::new(None) };
}

/// Runs `f` with this worker thread's read-only connection, opening it on first use.
pub fn with_reader<T>(
    db_path: &Path,
    f: impl FnOnce(&Connection) -> Result<T, ApiError>,
) -> Result<T, ApiError> {
    READER.with(|cell| {
        // Re-entrancy is a bug (a reader callback calling with_reader); report it
        // as a 500 rather than panicking.
        let mut slot = cell
            .try_borrow_mut()
            .map_err(|_| ApiError::Internal("reader connection re-entered".into()))?;
        let reusable = matches!(slot.as_ref(), Some((path, _)) if path == db_path);
        if !reusable {
            *slot = Some((db_path.to_owned(), open(db_path)?));
        }
        match slot.as_ref() {
            Some((_, conn)) => f(conn),
            None => Err(ApiError::Internal("reader connection missing".into())),
        }
    })
}

fn open(db_path: &Path) -> Result<Connection, ApiError> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // query_only: belt and braces on top of the read-only open.
    // mmap 256 MiB: page reads become memory reads instead of read() syscalls.
    // cache_size -65536: 64 MiB page cache per reader.
    conn.execute_batch(
        "PRAGMA query_only = 1; PRAGMA mmap_size = 268435456; PRAGMA cache_size = -65536;",
    )?;
    // Readers in WAL mode almost never wait, but a TRUNCATE checkpoint at
    // shutdown can briefly hold a lock; wait instead of failing a request.
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.set_prepared_statement_cache_capacity(16);
    Ok(conn)
}
