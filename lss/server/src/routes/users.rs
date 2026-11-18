//! `GET /users/{userId}/orders`.

use actix_web::{HttpRequest, HttpResponse, web};

use crate::errors::ApiError;
use crate::http;
use crate::state::AppState;

/// `GET /users/{userId}/orders` -> 200 `{"userId","orders":[{"orderId","productId"}]}`.
/// An unknown user is 200 with `"orders": []`, never 404.
pub async fn user_orders(
    state: web::Data<AppState>,
    req: HttpRequest,
) -> Result<HttpResponse, ApiError> {
    let _user_id = http::path_param(&req, "userId");
    let _ = &state;
    // TODO(task-2): read the user's list from the in-memory order store.
    Err(ApiError::NotImplemented)
}
