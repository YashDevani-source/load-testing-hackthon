//! `GET /health`: the load generator polls this once a second and starts testing
//! the moment it says ok, so it must only say ok when we really are ready.

use actix_web::http::StatusCode;
use actix_web::{HttpResponse, web};

use crate::http::respond;
use crate::state::AppState;

/// 200 `{"status":"ok"}` once the database is open and state is loaded;
/// 503 `{"status":"starting"}` before that. Reads one atomic: no DB call, so a
/// health poll can never queue behind a slow query.
pub async fn health(state: web::Data<AppState>) -> HttpResponse {
    if state.is_ready() {
        respond::json_static(StatusCode::OK, r#"{"status":"ok"}"#)
    } else {
        respond::json_static(StatusCode::SERVICE_UNAVAILABLE, r#"{"status":"starting"}"#)
    }
}
