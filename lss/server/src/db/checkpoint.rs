//! Background WAL checkpointing.
//!
//! With `wal_autocheckpoint=0` the WAL only shrinks when someone checkpoints it.
//! A dedicated thread does a PASSIVE checkpoint every few seconds: it copies
//! committed pages from the WAL into the main database file without ever waiting
//! for, or blocking, the writer. Once a checkpoint has caught up, the next commit
//! starts reusing the WAL from the beginning, so the file stays small.
//!
//! The thread uses its own SQLite connection rather than locking the request
//! connection: a checkpoint can take milliseconds (it fsyncs), and holding the
//! request mutex that long would stall every order behind it, which is exactly the
//! latency spike that moving checkpoints off the request path is meant to avoid.
//! This connection never reads or writes rows; it only checkpoints.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// How often the background thread checkpoints.
pub const INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Copy as much as possible without blocking anyone. Used in the background.
    Passive,
    /// Wait for everything, copy it all, truncate the WAL file. Used at shutdown.
    Truncate,
}

/// Runs one checkpoint and returns SQLite's `(busy, wal_frames, checkpointed_frames)`.
pub fn run(conn: &Connection, mode: Mode) -> rusqlite::Result<(i64, i64, i64)> {
    let sql = match mode {
        Mode::Passive => "PRAGMA wal_checkpoint(PASSIVE)",
        Mode::Truncate => "PRAGMA wal_checkpoint(TRUNCATE)",
    };
    conn.query_row(sql, [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
}

/// Handle to the background thread. Call [`Checkpointer::stop`] before the final
/// TRUNCATE checkpoint; dropping the handle stops it too.
pub struct Checkpointer {
    stop_tx: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Checkpointer {
    /// Opens a checkpoint-only connection to the (already created) database and
    /// starts the thread.
    pub fn spawn(db_path: &Path, interval: Duration) -> anyhow::Result<Self> {
        // No CREATE flag: the request connection created the file and schema; if
        // the file is missing something is badly wrong and we want to know.
        let conn = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(Duration::from_secs(5))?;

        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("lss-checkpoint".into())
            .spawn(move || {
                // Sleeping on the channel (instead of thread::sleep) lets stop()
                // wake us immediately at shutdown: a message or a dropped sender
                // ends the loop, a timeout means "time to checkpoint".
                while let Err(RecvTimeoutError::Timeout) = stop_rx.recv_timeout(interval) {
                    if let Err(err) = run(&conn, Mode::Passive) {
                        // Not fatal: the WAL just grows until the next try.
                        tracing::warn!(error = %err, "background WAL checkpoint failed");
                    }
                }
            })?;

        Ok(Self {
            stop_tx: Some(stop_tx),
            thread: Some(thread),
        })
    }

    /// Stops the thread and waits for it (at most one in-flight checkpoint).
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        // Dropping the sender disconnects the channel, which ends the loop.
        drop(self.stop_tx.take());
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("checkpoint thread panicked");
        }
    }
}

impl Drop for Checkpointer {
    fn drop(&mut self) {
        self.shutdown();
    }
}
