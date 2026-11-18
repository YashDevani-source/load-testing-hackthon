//! Last Server Standing: a flash-sale API server.
//!
//! Everything lives in this library so `tests/` can build the real app in-process;
//! `main.rs` only wires config, logging and the HTTP server together.
//!
//! Layout:
//! - [`config`]   environment variables, validated once at startup
//! - [`errors`]   the spec's error codes and how they render
//! - [`validate`] pure input validation (no I/O)
//! - [`http`]     route table, JSON response helpers, query-string parsing
//! - [`routes`]   one module per resource
//! - [`db`]       the single SQLite connection, schema, WAL checkpointing
//! - [`state`]    shared app state: DB handle, ready flag, in-memory read caches

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
pub mod routes;
pub mod state;
pub mod validate;
