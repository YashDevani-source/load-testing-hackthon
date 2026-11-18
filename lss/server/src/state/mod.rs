//! Shared application state, one instance per process, handed to every actix
//! worker as `web::Data<AppState>` (an `Arc`).
//!
//! The database is the source of truth. The stores below are read caches that
//! task 2 fills at startup and keeps in step with every committed write, so that
//! the 80% of traffic that only reads never touches the database mutex.

pub mod categories;
pub mod orders;
pub mod products;

use std::sync::atomic::{AtomicBool, Ordering};

use crate::db::{Db, DbError};

pub use categories::CategoryIndex;
pub use orders::OrderStore;
pub use products::ProductStore;

pub struct AppState {
    /// The one request connection. See [`crate::db`].
    pub db: Db,
    /// Set once, after the database is open and every store is loaded. `/health`
    /// reports 503 until then, so the load generator can't start early.
    ready: AtomicBool,
    /// Product by id: fixed fields + pre-built JSON + current stock.
    pub products: ProductStore,
    /// Per-category product lists sorted by (price, id), for `GET /products?category=`.
    pub categories: CategoryIndex,
    /// Per-user order lists, for `GET /users/{userId}/orders`.
    pub orders: OrderStore,
}

impl AppState {
    /// A not-yet-ready state around an opened database.
    pub fn new(db: Db) -> Self {
        Self {
            db,
            ready: AtomicBool::new(false),
            products: ProductStore::default(),
            categories: CategoryIndex::default(),
            orders: OrderStore::default(),
        }
    }

    /// Fills the in-memory stores from the database. Runs once at startup, before
    /// [`AppState::mark_ready`].
    pub fn load_from_db(&self) -> Result<(), DbError> {
        // TODO(task-2): read all products (ORDER BY category, price, id) and all
        // orders, and build the three stores. Until then there is nothing to load,
        // and opening the database is the whole of "ready".
        Ok(())
    }

    pub fn is_ready(&self) -> bool {
        // Acquire pairs with the Release in mark_ready: a worker that sees `true`
        // also sees everything load_from_db wrote.
        self.ready.load(Ordering::Acquire)
    }

    pub fn mark_ready(&self) {
        self.ready.store(true, Ordering::Release);
    }
}
