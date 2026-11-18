//! `READ_SOURCE=memory`: read caches in front of SQLite.
//!
//! Rules that keep these caches honest:
//! - They are written only by the write handlers, **after** the SQLite commit and
//!   **while still holding** the writer mutex. So they never show anything the
//!   database doesn't have, and writes reach them in commit order.
//! - They are never used to decide an order. `POST /orders` always asks SQLite.
//! - Readers may see a value a moment old (the spec allows 2 s); in practice the
//!   lag is the few microseconds between commit and cache update.
//!
//! Locking: DashMap shards + a per-category RwLock. Read functions clone what
//! they need (an `Arc`) or render a small body while holding a read guard; none
//! of them does anything slow under a guard.

use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::time::Instant;

use dashmap::DashMap;
use parking_lot::RwLock;
use rusqlite::Connection;

use super::categories::insert_sorted;
use super::{MemOrder, MemProduct};
use crate::render::{self, ListBody, OrdersBody};

#[derive(Debug, Default)]
pub struct MemoryStores {
    /// id -> product.
    pub products: DashMap<Box<str>, Arc<MemProduct>>,
    /// category -> products sorted by (price, id).
    pub categories: DashMap<Box<str>, RwLock<Vec<Arc<MemProduct>>>>,
    /// user id -> orders in order_id order.
    pub orders: DashMap<Box<str>, Vec<MemOrder>>,
}

impl MemoryStores {
    /// Builds the stores from the database at startup, before the server accepts
    /// requests.
    pub fn load(conn: &Connection) -> anyhow::Result<Self> {
        let started = Instant::now();
        let stores = Self::default();

        // Already in (category, price, id) order thanks to the index, so each
        // category list is built by pushing, no sorting.
        let mut stmt = conn.prepare(
            "SELECT id, category, price, stock, json_prefix FROM products ORDER BY category, price, id",
        )?;
        let mut rows = stmt.query([])?;
        let mut product_count = 0usize;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let category: String = row.get(1)?;
            let product = Arc::new(MemProduct {
                id: id.into(),
                price: row.get(2)?,
                stock: AtomicI64::new(row.get(3)?),
                json_prefix: row.get::<_, String>(4)?.into(),
            });
            stores
                .products
                .insert(product.id.clone(), Arc::clone(&product));
            stores
                .categories
                .entry(category.into())
                .or_default()
                .write()
                .push(product);
            product_count += 1;
        }

        let mut stmt =
            conn.prepare("SELECT order_id, user_id, product_id FROM orders ORDER BY order_id")?;
        let mut rows = stmt.query([])?;
        let mut order_count = 0usize;
        while let Some(row) = rows.next()? {
            let user_id: String = row.get(1)?;
            let product_id: String = row.get(2)?;
            stores
                .orders
                .entry(user_id.into())
                .or_default()
                .push(MemOrder {
                    order_id: row.get(0)?,
                    product_id: product_id.into(),
                });
            order_count += 1;
        }

        tracing::info!(
            products = product_count,
            categories = stores.categories.len(),
            orders = order_count,
            users = stores.orders.len(),
            elapsed_ms = started.elapsed().as_millis(),
            "memory stores loaded"
        );
        Ok(stores)
    }

    /// Adds a product just committed by `POST /products`. Writer lock held.
    pub fn insert_product(
        &self,
        id: &str,
        category: &str,
        price: i64,
        stock: i64,
        json_prefix: String,
    ) {
        let product = Arc::new(MemProduct {
            id: id.into(),
            price,
            json_prefix: json_prefix.into(),
            stock: AtomicI64::new(stock),
        });
        self.products
            .insert(product.id.clone(), Arc::clone(&product));
        // Existing category: take only a shard read guard, then the list's own
        // write lock, so readers of other categories in the same shard aren't
        // blocked. New category: create it through `entry`.
        if let Some(list) = self.categories.get(category) {
            insert_sorted(&mut list.write(), product);
            return;
        }
        insert_sorted(
            &mut self.categories.entry(category.into()).or_default().write(),
            product,
        );
    }

    /// Records an order just committed by `POST /orders`. Writer lock held.
    /// `new_stock` is what the database's `UPDATE ... RETURNING stock` returned.
    pub fn apply_order(&self, product_id: &str, new_stock: i64, user_id: &str, order_id: i64) {
        let product = self.products.get(product_id).map(|p| Arc::clone(&p));
        match product {
            Some(p) => p.set_stock(new_stock),
            // Impossible while the cache is consistent (the product was found in
            // SQLite under the same lock). Logged, not panicked: SQLite is right
            // either way and a restart would rebuild the cache.
            None => tracing::error!(
                product_id,
                "memory store missing a product that has an order"
            ),
        }
        let order = MemOrder {
            order_id,
            product_id: product_id.into(),
        };
        if let Some(mut list) = self.orders.get_mut(user_id) {
            list.push(order);
            return;
        }
        self.orders.entry(user_id.into()).or_default().push(order);
    }

    /// Body for `GET /products/{id}`, or `None` for 404.
    pub fn get_product_json(&self, id: &str) -> Option<String> {
        // Clone the Arc and let the shard guard go before rendering.
        let product = self.products.get(id).map(|p| Arc::clone(&p))?;
        Some(render::product(&product.json_prefix, product.stock()))
    }

    /// Body for `GET /products?category=&page=&limit=`.
    pub fn list_json(&self, category: &str, page: u64, limit: u64) -> String {
        let mut body = ListBody::new(limit);
        let Some(list) = self.categories.get(category) else {
            return body.finish(page, limit, 0);
        };
        let list = list.read();
        let total = u64::try_from(list.len()).unwrap_or(u64::MAX);
        // skip/take instead of slicing: an offset past the end simply yields
        // nothing, which is the spec's "page past the end -> items: []".
        let offset =
            usize::try_from(page.saturating_sub(1).saturating_mul(limit)).unwrap_or(usize::MAX);
        let take = usize::try_from(limit).unwrap_or(usize::MAX);
        for product in list.iter().skip(offset).take(take) {
            body.push(&product.json_prefix, product.stock());
        }
        body.finish(page, limit, total)
    }

    /// Body for `GET /users/{userId}/orders`. Unknown user -> `"orders":[]`.
    pub fn user_orders_json(&self, user_id: &str) -> String {
        let mut body = OrdersBody::new(user_id);
        // Rendering a user's few orders under the shard read guard takes
        // microseconds; copying the Vec out first would cost more.
        if let Some(orders) = self.orders.get(user_id) {
            for order in orders.iter() {
                body.push(order.order_id, &order.product_id);
            }
        }
        body.finish()
    }
}
