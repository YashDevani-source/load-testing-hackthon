//! Request handlers, one module per resource.
//!
//! Handlers call SQLite directly (no `web::block`): the database is in-process and
//! a query takes microseconds, so hopping to a blocking thread pool would cost more
//! than the query itself.

pub mod health;
pub mod orders;
pub mod products;
pub mod users;
