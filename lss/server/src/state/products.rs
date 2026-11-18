//! Product store (task 2). Definition and design notes only.
//!
//! Planned design:
//! - `id -> Arc<ProductEntry>` in a map that is read-mostly (written only by
//!   `POST /products`, ~10,000 times during seeding plus one flash drop per level).
//! - `ProductEntry` holds the fields that never change, pre-rendered as JSON
//!   (`{"id":..,"name":..,"category":..,"price":..,"stock":`) so a read only
//!   appends the current stock and `}`: no serialisation per request.
//! - Stock lives in an `AtomicI64` per product, written only after the database
//!   commit of an order (under the DB mutex), so it is never ahead of the truth
//!   and never negative. Reads may lag by microseconds, far inside the 2 s rule.
//! - `POST /orders` never trusts this number to accept or reject; the database
//!   decides (spec rule: the stock check inside an order must be exact).

/// All products, keyed by id. Empty placeholder until task 2.
#[derive(Debug, Default)]
pub struct ProductStore {
    _private: (),
}
