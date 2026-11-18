//! `GET /users/{userId}/orders`.

use actix_web::http::StatusCode;
use actix_web::{HttpRequest, HttpResponse, web};

use crate::db::reader::with_reader;
use crate::errors::ApiError;
use crate::http::{self, respond};
use crate::render::OrdersBody;
use crate::state::AppState;

/// `GET /users/{userId}/orders` -> 200 `{"userId","orders":[{"orderId","productId"}]}`.
/// An unknown user is 200 with `"orders": []`, never 404.
pub async fn user_orders(
    state: web::Data<AppState>,
    req: HttpRequest,
) -> Result<HttpResponse, ApiError> {
    let user_id = http::path_param(&req, "userId");
    let body = match &state.mem {
        Some(mem) => mem.user_orders_json(user_id),
        None => with_reader(&state.db_path, |conn| {
            let mut body = OrdersBody::new(user_id);
            // Served by the UNIQUE(user_id, product_id) index (user_id is its
            // prefix); ordering by order_id matches the memory store's
            // append order, so both modes return identical bytes.
            let mut stmt = conn.prepare_cached(
                "SELECT order_id, product_id FROM orders WHERE user_id = ?1 ORDER BY order_id",
            )?;
            let mut rows = stmt.query([user_id])?;
            while let Some(row) = rows.next()? {
                body.push(row.get(0)?, row.get_ref(1)?.as_str()?);
            }
            Ok(body.finish())
        })?,
    };
    Ok(respond::json(StatusCode::OK, body))
}
