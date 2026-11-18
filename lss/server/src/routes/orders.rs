//! `POST /orders`: buy one unit. The only write on the hot path, and the one
//! that decides disqualification.

use actix_web::{HttpResponse, web};

use crate::errors::ApiError;
use crate::state::AppState;
use crate::validate;

/// `POST /orders` -> 201 `{"orderId","userId","productId"}` | 400 | 404 | 409.
///
/// Error precedence is 400 -> 404 -> 409, and any error must change nothing.
pub async fn create_order(
    state: web::Data<AppState>,
    body: Result<web::Bytes, actix_web::Error>,
) -> Result<HttpResponse, ApiError> {
    let body = body.map_err(|_| ApiError::Validation)?;
    let _order = validate::new_order(&body)?;
    let _ = &state;
    // TODO(task-2): in ONE SQLite transaction under the connection mutex:
    //   1. product must exist                         else 404 PRODUCT_NOT_FOUND
    //   2. INSERT order; UNIQUE(user_id, product_id)  violated -> 409 ALREADY_PURCHASED
    //   3. UPDATE stock = stock - 1 WHERE stock > 0   0 rows -> rollback, 409 SOLD_OUT
    //   4. COMMIT, then update the in-memory stock and the user's order list.
    // The stock check must read the database (or an exact mirror updated under the
    // same lock), never a cached number.
    Err(ApiError::NotImplemented)
}
