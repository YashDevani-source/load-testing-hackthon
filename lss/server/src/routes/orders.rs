//! `POST /orders`: buy one unit. The only write on the hot path, and the one
//! that decides disqualification.

use actix_web::http::StatusCode;
use actix_web::{HttpResponse, web};
use rusqlite::{OptionalExtension, TransactionBehavior};

use crate::db::errors::is_unique_violation;
use crate::errors::ApiError;
use crate::http::respond;
use crate::render;
use crate::state::AppState;
use crate::validate;

/// `POST /orders` -> 201 `{"orderId","userId","productId"}` | 400 | 404 | 409.
///
/// Exactness: both checks below run in one transaction on the only write
/// connection, so no other order can interleave. `WHERE stock > 0` makes
/// overselling impossible and the UNIQUE(user_id, product_id) constraint makes a
/// second purchase impossible, whatever the timing of simultaneous requests.
/// `CHECK (stock >= 0)` is a second safety net under the first.
///
/// Errors change nothing: every error path returns while `tx` is still open, and
/// dropping an uncommitted transaction rolls it back.
///
/// Never decide an order from a cached number. There is no "memory says 0, skip
/// the database" shortcut, ever: a stale 0 would turn away buyers and leave a
/// flash drop with stock (spec rule 7).
pub async fn create_order(
    state: web::Data<AppState>,
    body: Result<web::Bytes, actix_web::Error>,
) -> Result<HttpResponse, ApiError> {
    // 400 first: validation needs no lock.
    let body = body.map_err(|_| ApiError::Validation)?;
    let order = validate::new_order(&body)?;

    let mut conn = state.writer.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // a) Take one unit, only if one is left.
    //
    // UPDATE before INSERT: once a flash drop sells out (seconds into each
    // level), ~70% of orders are for a sold-out product; checking stock first
    // makes them one indexed lookup with no write. When a user already bought
    // *and* it's sold out, the spec accepts either 409. 404 is still decided
    // before any 409, as required (400 -> 404 -> 409).
    let new_stock: Option<i64> = tx
        .prepare_cached(
            "UPDATE products SET stock = stock - 1 WHERE id = ?1 AND stock > 0 RETURNING stock",
        )?
        .query_row([&order.product_id], |r| r.get(0))
        .optional()?;
    let Some(new_stock) = new_stock else {
        // Nothing was updated: either no such product, or no stock left.
        let exists: bool = tx
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM products WHERE id = ?1)")?
            .query_row([&order.product_id], |r| r.get(0))?;
        return Err(if exists {
            ApiError::SoldOut
        } else {
            ApiError::ProductNotFound
        });
    };

    // b) Record the order. UNIQUE(user_id, product_id) makes a 2nd purchase
    // impossible: of two simultaneous identical orders, the second fails here.
    let inserted = tx
        .prepare_cached(
            "INSERT INTO orders (user_id, product_id) VALUES (?1, ?2) RETURNING order_id",
        )?
        .query_row((&order.user_id, &order.product_id), |r| r.get::<_, i64>(0));
    let order_id = match inserted {
        Ok(id) => id,
        // Returning drops `tx`: the rollback also restores the unit taken in (a).
        Err(e) if is_unique_violation(&e) => return Err(ApiError::AlreadyPurchased),
        Err(e) => return Err(e.into()),
    };

    // c) Durable from here on (WAL append); only now may anything say "bought".
    tx.commit()?;

    // d) Memory mode: update the cache after the commit, still holding the lock,
    // so the cache never shows an order the database doesn't have (rule 6) and
    // every 201 is in the user's list before the response goes out (rule 5).
    if let Some(mem) = &state.mem {
        mem.apply_order(&order.product_id, new_stock, &order.user_id, order_id);
    }
    drop(conn);

    Ok(respond::json(
        StatusCode::CREATED,
        render::order_created(order_id, &order.user_id, &order.product_id),
    ))
}
