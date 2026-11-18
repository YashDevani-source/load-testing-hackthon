//! `POST /products`, `GET /products/{id}`, `GET /products?category=&page=&limit=`.
//!
//! Task 1: input is fully validated (so every 400 case already matches the spec);
//! the business logic answers 501 until task 2.

use actix_web::{HttpRequest, HttpResponse, web};

use crate::errors::ApiError;
use crate::http::{self, query};
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
    let _product = validate::new_product(&body)?;
    let _ = &state;
    // TODO(task-2): INSERT ... ON CONFLICT DO NOTHING; 0 rows changed -> 409
    // PRODUCT_EXISTS (existing row untouched). Then add to the in-memory product
    // store and category index, and answer 201 with the stored product.
    Err(ApiError::NotImplemented)
}

/// `GET /products/{id}` -> 200 + product | 404 `PRODUCT_NOT_FOUND`.
pub async fn get_product(
    state: web::Data<AppState>,
    req: HttpRequest,
) -> Result<HttpResponse, ApiError> {
    let _id = http::path_param(&req, "id");
    let _ = &state;
    // TODO(task-2): look up the in-memory product store; stock may be <= 2 s old.
    Err(ApiError::NotImplemented)
}

/// `GET /products?category=&page=&limit=` -> 200 `{"items","page","limit","total"}`.
pub async fn list_products(
    state: web::Data<AppState>,
    req: HttpRequest,
) -> Result<HttpResponse, ApiError> {
    let _query = query::list_query(req.query_string())?;
    let _ = &state;
    // TODO(task-2): slice the category's (price, id)-sorted list at
    // offset()..offset()+limit; unknown category or page past the end -> items [].
    Err(ApiError::NotImplemented)
}
