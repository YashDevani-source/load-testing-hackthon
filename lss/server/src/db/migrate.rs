//! Schema migrations, tracked with `PRAGMA user_version`.
//!
//! | version | contents |
//! |---|---|
//! | 0 | empty database (or a task-1 database, which never set user_version) |
//! | 1 | `products`, `products_category_price_id`, `orders` (schema.sql) |
//! | 2 | + `products.json_prefix`, + `categories(category, total)` |
//!
//! All pending steps run in one IMMEDIATE transaction together with the version
//! bump, so a crash mid-migration leaves the database exactly as it was.
//!
//! A task-1 database has the version-1 tables but `user_version = 0`. Step 0 -> 1
//! only uses `CREATE ... IF NOT EXISTS`, so it is a no-op there and the database
//! then takes the normal 1 -> 2 path.

use anyhow::Context;
use rusqlite::{Connection, Transaction, TransactionBehavior};

use super::SCHEMA_V1;
use crate::render::build_json_prefix;

/// The version this binary writes and expects.
pub const CURRENT_VERSION: i64 = 2;

pub fn migrate(conn: &mut Connection) -> anyhow::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Read the version inside the write transaction, so two processes can't both
    // decide to migrate the same database.
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    anyhow::ensure!(
        version <= CURRENT_VERSION,
        "database schema is version {version}, newer than this binary ({CURRENT_VERSION}); refusing to touch it"
    );
    if version == CURRENT_VERSION {
        return Ok(()); // dropping tx is a no-op rollback
    }

    if version < 1 {
        tx.execute_batch(SCHEMA_V1).context("migration 0 -> 1")?;
    }
    if version < 2 {
        v1_to_v2(&tx).context("migration 1 -> 2")?;
    }
    // PRAGMA can't take bound parameters; CURRENT_VERSION is a constant.
    tx.execute_batch(&format!("PRAGMA user_version = {CURRENT_VERSION}"))?;
    tx.commit()?;
    tracing::info!(from = version, to = CURRENT_VERSION, "database migrated");
    Ok(())
}

/// Adds the pre-rendered product JSON and the per-category totals.
fn v1_to_v2(tx: &Transaction<'_>) -> anyhow::Result<()> {
    tx.execute_batch(
        "ALTER TABLE products ADD COLUMN json_prefix TEXT NOT NULL DEFAULT '';
         CREATE TABLE IF NOT EXISTS categories (
           category TEXT PRIMARY KEY,
           total    INTEGER NOT NULL
         ) WITHOUT ROWID;
         INSERT INTO categories (category, total)
           SELECT category, COUNT(*) FROM products GROUP BY category;",
    )?;

    // Backfill in Rust so the prefix comes from the same function POST /products
    // uses: one serialiser, so old and new rows can't differ by a byte.
    let rows: Vec<(String, String, String, i64)> = {
        let mut stmt =
            tx.prepare("SELECT id, name, category, price FROM products WHERE json_prefix = ''")?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?
    };
    let mut update = tx.prepare("UPDATE products SET json_prefix = ?2 WHERE id = ?1")?;
    for (id, name, category, price) in &rows {
        update.execute((id, build_json_prefix(id, name, category, *price)))?;
    }
    tracing::info!(backfilled = rows.len(), "json_prefix backfilled");
    Ok(())
}
