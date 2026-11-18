//! Category index (task 2). Definition and design notes only.
//!
//! Planned design:
//! - `category -> Vec<Arc<ProductEntry>>`, kept sorted by `(price, id)` with id
//!   compared as plain bytes (what the spec calls "plain character order").
//! - Built at startup from `SELECT .. ORDER BY category, price, id`, which the
//!   `products_category_price_id` index serves without a sort.
//! - A new product is inserted at its `binary_search` position. Categories are
//!   small (~200 products), so the shift is cheap and happens only on create.
//! - A list request is then `total = len`, `items = slice[offset..offset+limit]`
//!   (bounds-checked with `get`), each item rendered from the product's
//!   pre-built JSON plus its current stock.

/// Per-category sorted product lists. Empty placeholder until task 2.
#[derive(Debug, Default)]
pub struct CategoryIndex {
    _private: (),
}
