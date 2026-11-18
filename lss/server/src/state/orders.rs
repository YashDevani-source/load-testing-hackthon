//! An order in the memory store.

use std::sync::Arc;

/// One entry of a user's order list. Appended only after the order's database
/// commit (no ghost orders), in the same critical section (no lost orders).
#[derive(Debug, Clone)]
pub struct MemOrder {
    pub order_id: i64,
    pub product_id: Arc<str>,
}
