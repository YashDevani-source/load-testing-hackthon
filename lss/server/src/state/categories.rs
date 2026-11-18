//! Keeping a category's product list in the spec's order.

use std::sync::Arc;

use super::MemProduct;

/// Inserts `product` into `list`, which is sorted by `(price, id)`.
///
/// Rust compares `str` byte by byte, which is SQLite's BINARY collation and the
/// spec's "plain character order" tie-break, so this order matches
/// `ORDER BY price, id` exactly. `partition_point` finds the slot in O(log n);
/// the shift is O(n) but categories are small and this runs only on create.
pub fn insert_sorted(list: &mut Vec<Arc<MemProduct>>, product: Arc<MemProduct>) {
    let pos = list.partition_point(|p| (p.price, &*p.id) < (product.price, &*product.id));
    list.insert(pos, product);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicI64;

    fn p(id: &str, price: i64) -> Arc<MemProduct> {
        Arc::new(MemProduct {
            id: id.into(),
            price,
            json_prefix: "".into(),
            stock: AtomicI64::new(0),
        })
    }

    #[test]
    fn sorted_by_price_then_id_bytes() {
        let mut v = Vec::new();
        for (id, price) in [
            ("pb", 349),
            ("pz", 100),
            ("pa", 349),
            ("Pa", 349),
            ("p0", 349),
            ("pc", 999),
        ] {
            insert_sorted(&mut v, p(id, price));
        }
        let ids: Vec<&str> = v.iter().map(|p| &*p.id).collect();
        // Uppercase and digits sort before lowercase in byte order.
        assert_eq!(ids, ["pz", "Pa", "p0", "pa", "pb", "pc"]);
    }
}
