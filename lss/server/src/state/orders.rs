//! Per-user order lists (task 2). Definition and design notes only.
//!
//! Planned design:
//! - `user_id -> Vec<(order_id, product_id)>`, appended after an order's database
//!   commit succeeds, never before (spec rule 6: no ghost orders), and always in
//!   the same critical section as the commit (rule 5: no lost orders).
//! - Loaded at startup from the `orders` table so a restart keeps every list.
//! - Sharded by a hash of `user_id` so readers and the order writer rarely
//!   contend on the same lock.

/// Orders grouped by user. Empty placeholder until task 2.
#[derive(Debug, Default)]
pub struct OrderStore {
    _private: (),
}
