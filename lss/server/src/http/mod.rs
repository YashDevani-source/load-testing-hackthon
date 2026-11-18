//! HTTP wiring: the route table and request-level settings shared by `main.rs` and
//! the integration tests.

pub mod query;
pub mod respond;

use actix_web::{HttpRequest, web};

use crate::errors;
use crate::routes::{health, orders, products, users};

/// Request bodies above this size are rejected with `VALIDATION_ERROR`. The
/// largest valid body (64 + 200 + 64 characters plus two numbers) is well under
/// 2 KB even at 4 bytes per character; 16 KB leaves room for ignored extra fields.
pub const MAX_BODY_BYTES: usize = 16 * 1024;

/// Registers every route. Used as `App::new().app_data(state).configure(http::configure)`.
///
/// Each resource gets a `default_service` so a wrong method returns a JSON 405
/// instead of actix's empty-bodied one; unknown paths get a JSON 404.
pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.app_data(web::PayloadConfig::new(MAX_BODY_BYTES))
        .service(
            web::resource("/health")
                .route(web::get().to(health::health))
                .default_service(web::to(errors::method_not_allowed)),
        )
        .service(
            web::resource("/products")
                .route(web::post().to(products::create_product))
                .route(web::get().to(products::list_products))
                .default_service(web::to(errors::method_not_allowed)),
        )
        .service(
            web::resource("/products/{id}")
                .route(web::get().to(products::get_product))
                .default_service(web::to(errors::method_not_allowed)),
        )
        .service(
            web::resource("/orders")
                .route(web::post().to(orders::create_order))
                .default_service(web::to(errors::method_not_allowed)),
        )
        .service(
            web::resource("/users/{userId}/orders")
                .route(web::get().to(users::user_orders))
                .default_service(web::to(errors::method_not_allowed)),
        )
        .default_service(web::to(errors::route_not_found));
}

/// A path parameter as a plain string. The router only matches `{name}` against a
/// non-empty segment, so a registered parameter is always present; the empty
/// fallback just keeps this panic-free.
pub fn path_param<'a>(req: &'a HttpRequest, name: &str) -> &'a str {
    req.match_info().get(name).unwrap_or("")
}
