//! `POST /products`, `GET /products/{id}`, `GET /products?category=&page=&limit=`.

use actix_web::http::StatusCode;
use actix_web::{HttpRequest, HttpResponse, web};
use rusqlite::{OptionalExtension, TransactionBehavior};

use crate::db::errors::is_primary_key_violation;
use crate::db::reader::with_reader;
use crate::errors::ApiError;
use crate::http::{self, query, respond};
use crate::render::{self, ListBody, build_json_prefix};
use crate::state::AppState;
use crate::validate;

/// `POST /products` -> 201 + product as stored | 400 | 409 `PRODUCT_EXISTS`.
///
/// The body arrives as `Result<Bytes, _>` rather than `Bytes` so that an
/// oversized or unreadable body becomes our `VALIDATION_ERROR` instead of
/// actix's plain-text 413/400.
pub async fn create_product(
    state: web::Data<AppState>,
    body: Result<web::Bytes, actix_web::Error>,
) -> Result<HttpResponse, ApiError> {
    let body = body.map_err(|_| ApiError::Validation)?;
    let p = validate::new_product(&body)?;
    // Rendered before taking the lock: serialisation is the slowest part and
    // doesn't need it.
    let prefix = build_json_prefix(&p.id, &p.name, &p.category, p.price);

    let mut conn = state.writer.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let inserted = tx
        .prepare_cached(
            "INSERT INTO products (id, name, category, price, stock, json_prefix)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute((&p.id, &p.name, &p.category, p.price, p.stock, &prefix));
    match inserted {
        Ok(_) => {}
        // The id is taken. Returning drops `tx`, which rolls back: the existing
        // product stays exactly as it was.
        Err(e) if is_primary_key_violation(&e) => return Err(ApiError::ProductExists),
        Err(e) => return Err(e.into()),
    }
    tx.prepare_cached(
        "INSERT INTO categories (category, total) VALUES (?1, 1)
         ON CONFLICT(category) DO UPDATE SET total = total + 1",
    )?
    .execute([&p.category])?;
    tx.commit()?;
    // Memory mode: update the cache after the commit, still holding the lock,
    // so caches see writes in commit order and never ahead of the database.
    if let Some(mem) = &state.mem {
        mem.insert_product(&p.id, &p.category, p.price, p.stock, prefix.clone());
    }
    drop(conn);

    let mut out = prefix;
    render::push_int(&mut out, p.stock);
    out.push('}');
    Ok(respond::json(StatusCode::CREATED, out))
}

/// `GET /products/{id}` -> 200 + product | 404 `PRODUCT_NOT_FOUND`.
/// Stock may be up to 2 s old per the spec; in both modes it is in fact current.
pub async fn get_product(
    state: web::Data<AppState>,
    req: HttpRequest,
) -> Result<HttpResponse, ApiError> {
    let id = http::path_param(&req, "id");
    let body = match &state.mem {
        Some(mem) => mem.get_product_json(id),
        None => with_reader(&state.db_path, |conn| {
            let mut stmt =
                conn.prepare_cached("SELECT json_prefix, stock FROM products WHERE id = ?1")?;
            let found = stmt
                .query_row([id], |r| {
                    Ok(render::product(r.get_ref(0)?.as_str()?, r.get(1)?))
                })
                .optional()?;
            Ok(found)
        })?,
    };
    match body {
        Some(body) => Ok(respond::json(StatusCode::OK, body)),
        None => Err(ApiError::ProductNotFound),
    }
}

/// `GET /products?category=&page=&limit=` -> 200 `{"items","page","limit","total"}`.
pub async fn list_products(
    state: web::Data<AppState>,
    req: HttpRequest,
) -> Result<HttpResponse, ApiError> {
    let q = query::list_query(req.query_string())?;
    let body = match &state.mem {
        Some(mem) => mem.list_json(&q.category, q.page, q.limit),
        None => with_reader(&state.db_path, |conn| {
            let mut body = ListBody::new(q.limit);
            // `categories.total` is kept in the same transaction as each product
            // insert, so it always equals COUNT(*) without counting on every read.
            let total: Option<i64> = conn
                .prepare_cached("SELECT total FROM categories WHERE category = ?1")?
                .query_row([&q.category], |r| r.get(0))
                .optional()?;
            let total = total.and_then(|t| u64::try_from(t).ok()).unwrap_or(0);
            let offset = q.offset();
            // Past the end (or unknown category): the answer is known, skip the query.
            if offset < total {
                let mut stmt = conn.prepare_cached(
                    "SELECT json_prefix, stock FROM products
                     WHERE category = ?1 ORDER BY price, id LIMIT ?2 OFFSET ?3",
                )?;
                let limit = i64::try_from(q.limit).unwrap_or(i64::MAX);
                let offset = i64::try_from(offset).unwrap_or(i64::MAX);
                let mut rows = stmt.query((&q.category, limit, offset))?;
                while let Some(row) = rows.next()? {
                    body.push(row.get_ref(0)?.as_str()?, row.get(1)?);
                }
            }
            Ok(body.finish(q.page, q.limit, total))
        })?,
    };
    Ok(respond::json(StatusCode::OK, body))
}
