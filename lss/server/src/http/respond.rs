//! JSON response helpers.
//!
//! Every response goes through here so `Content-Type: application/json` can't be
//! forgotten on some path (the spec requires it on every response, errors included).

use actix_web::HttpResponse;
use actix_web::http::StatusCode;
use actix_web::http::header::{CONTENT_TYPE, HeaderValue};
use actix_web::web::Bytes;

use crate::errors::ApiError;

const APPLICATION_JSON: HeaderValue = HeaderValue::from_static("application/json");

/// A JSON body that is already bytes (pre-built product JSON, for example).
pub fn json_bytes(status: StatusCode, body: impl Into<Bytes>) -> HttpResponse {
    HttpResponse::build(status)
        .insert_header((CONTENT_TYPE, APPLICATION_JSON))
        .body(body.into())
}

/// A constant JSON body. `Bytes::from_static` points at the string in the binary,
/// so this neither allocates nor copies.
pub fn json_static(status: StatusCode, body: &'static str) -> HttpResponse {
    json_bytes(status, Bytes::from_static(body.as_bytes()))
}

/// Serialises a `serde_json::Value`. Meant for cold paths; hot paths should build
/// their bytes directly.
pub fn json_value(status: StatusCode, value: &serde_json::Value) -> Result<HttpResponse, ApiError> {
    match serde_json::to_vec(value) {
        Ok(bytes) => Ok(json_bytes(status, bytes)),
        Err(err) => {
            tracing::error!(error = %err, "failed to serialise response");
            Err(ApiError::Internal)
        }
    }
}
