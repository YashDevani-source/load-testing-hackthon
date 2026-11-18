//! Every error the API can return, and how it is rendered.
//!
//! The spec allows exactly five error codes. We add three that the load generator
//! never sees from a correct client: `NOT_FOUND` / `METHOD_NOT_ALLOWED` for routes
//! that don't exist, `NOT_IMPLEMENTED` for task-1 stubs, and `INTERNAL_ERROR` for a
//! 500 we logged. Bodies are `&'static str` so rendering an error never allocates
//! or serialises anything.

use std::fmt;

use actix_web::http::StatusCode;
use actix_web::{HttpResponse, ResponseError};

use crate::http::respond;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiError {
    /// 400: a required field or query parameter is missing, empty or the wrong type.
    Validation,
    /// 404: no product with that id.
    ProductNotFound,
    /// 409: `POST /products` with an id that's already taken.
    ProductExists,
    /// 409: this user already bought this product.
    AlreadyPurchased,
    /// 409: the product's stock is 0.
    SoldOut,
    /// 404: no route matches the path. Not a spec code.
    RouteNotFound,
    /// 405: the path exists but not with this method. Not a spec code.
    MethodNotAllowed,
    /// 501: route is wired but its logic arrives in task 2. Not a spec code.
    NotImplemented,
    /// 500: something we didn't expect (e.g. a database I/O error). The cause is
    /// logged where it happens; the client only sees the code.
    Internal,
}

impl ApiError {
    pub fn status(self) -> StatusCode {
        match self {
            Self::Validation => StatusCode::BAD_REQUEST,
            Self::ProductNotFound | Self::RouteNotFound => StatusCode::NOT_FOUND,
            Self::ProductExists | Self::AlreadyPurchased | Self::SoldOut => StatusCode::CONFLICT,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::NotImplemented => StatusCode::NOT_IMPLEMENTED,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The exact spelling the spec requires.
    pub fn code(self) -> &'static str {
        match self {
            Self::Validation => "VALIDATION_ERROR",
            Self::ProductNotFound => "PRODUCT_NOT_FOUND",
            Self::ProductExists => "PRODUCT_EXISTS",
            Self::AlreadyPurchased => "ALREADY_PURCHASED",
            Self::SoldOut => "SOLD_OUT",
            Self::RouteNotFound => "NOT_FOUND",
            Self::MethodNotAllowed => "METHOD_NOT_ALLOWED",
            Self::NotImplemented => "NOT_IMPLEMENTED",
            Self::Internal => "INTERNAL_ERROR",
        }
    }

    /// The full JSON body. Written out by hand (instead of `format!`) so it is a
    /// compile-time constant; a unit test checks it matches `code()`.
    pub fn body(self) -> &'static str {
        match self {
            Self::Validation => r#"{"error":"VALIDATION_ERROR"}"#,
            Self::ProductNotFound => r#"{"error":"PRODUCT_NOT_FOUND"}"#,
            Self::ProductExists => r#"{"error":"PRODUCT_EXISTS"}"#,
            Self::AlreadyPurchased => r#"{"error":"ALREADY_PURCHASED"}"#,
            Self::SoldOut => r#"{"error":"SOLD_OUT"}"#,
            Self::RouteNotFound => r#"{"error":"NOT_FOUND"}"#,
            Self::MethodNotAllowed => r#"{"error":"METHOD_NOT_ALLOWED"}"#,
            Self::NotImplemented => r#"{"error":"NOT_IMPLEMENTED"}"#,
            Self::Internal => r#"{"error":"INTERNAL_ERROR"}"#,
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for ApiError {}

/// Lets handlers return `Result<HttpResponse, ApiError>` and use `?`.
impl ResponseError for ApiError {
    fn status_code(&self) -> StatusCode {
        self.status()
    }

    fn error_response(&self) -> HttpResponse {
        respond::json_static(self.status(), self.body())
    }
}

/// Any SQLite error that reaches a handler is unexpected (constraint violations
/// that mean something, like a duplicate order, are matched explicitly before
/// this). Log the cause, answer 500.
impl From<rusqlite::Error> for ApiError {
    fn from(err: rusqlite::Error) -> Self {
        tracing::error!(error = %err, "database error");
        Self::Internal
    }
}

/// Default handler for paths that match no route.
pub async fn route_not_found() -> HttpResponse {
    ApiError::RouteNotFound.error_response()
}

/// Default handler for known paths called with the wrong method.
pub async fn method_not_allowed() -> HttpResponse {
    ApiError::MethodNotAllowed.error_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [ApiError; 9] = [
        ApiError::Validation,
        ApiError::ProductNotFound,
        ApiError::ProductExists,
        ApiError::AlreadyPurchased,
        ApiError::SoldOut,
        ApiError::RouteNotFound,
        ApiError::MethodNotAllowed,
        ApiError::NotImplemented,
        ApiError::Internal,
    ];

    #[test]
    fn bodies_are_valid_json_matching_code() {
        for e in ALL {
            let v: serde_json::Value = serde_json::from_str(e.body()).unwrap();
            assert_eq!(v, serde_json::json!({ "error": e.code() }), "{e:?}");
        }
    }

    #[test]
    fn spec_status_codes() {
        assert_eq!(ApiError::Validation.status().as_u16(), 400);
        assert_eq!(ApiError::ProductNotFound.status().as_u16(), 404);
        assert_eq!(ApiError::ProductExists.status().as_u16(), 409);
        assert_eq!(ApiError::AlreadyPurchased.status().as_u16(), 409);
        assert_eq!(ApiError::SoldOut.status().as_u16(), 409);
    }
}
