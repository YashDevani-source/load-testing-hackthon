//! A product in the memory store.

use std::sync::atomic::{AtomicI64, Ordering};

/// Everything a read needs to answer for one product.
///
/// The fixed fields are kept only as the pre-rendered `json_prefix` (plus `id` and
/// `price`, which the category sort needs). `stock` is the only mutable field.
#[derive(Debug)]
pub struct MemProduct {
    pub id: Box<str>,
    pub price: i64,
    /// `{"id":..,"name":..,"category":..,"price":..,"stock":` exactly as stored in
    /// SQLite's `products.json_prefix`.
    pub json_prefix: Box<str>,
    pub stock: AtomicI64,
}

impl MemProduct {
    /// Current stock for a read response.
    ///
    /// `Relaxed` is enough: stock is only written while the writer mutex is held
    /// (after the database commit), and a reader may see a value a moment old,
    /// which the spec allows (<= 2 s). No other memory is published through this
    /// atomic, so no stronger ordering is needed. Orders never consult it.
    pub fn stock(&self) -> i64 {
        self.stock.load(Ordering::Relaxed)
    }

    /// Called only with the writer mutex held, with the stock the database just
    /// committed (`UPDATE ... RETURNING stock`).
    pub fn set_stock(&self, stock: i64) {
        self.stock.store(stock, Ordering::Relaxed);
    }
}
