//! HTTP contract: status codes, Content-Type and error bodies, exercised through
//! the real route table in-process.
//!
//! TODO(task-2): the `*_stub_*` tests assert 501 for valid input; replace them with
//! the real success/404/409 cases when the business logic lands.

use actix_web::body::to_bytes;
use actix_web::dev::{Service, ServiceResponse};
use actix_web::http::{Method, StatusCode, header};
use actix_web::{App, test, web};
use serde_json::{Value, json};
use tempfile::TempDir;

use lss_server::db::Db;
use lss_server::http;
use lss_server::state::AppState;

/// A fresh app on a temp database. The TempDir must outlive the app.
async fn app(
    ready: bool,
) -> (
    TempDir,
    web::Data<AppState>,
    impl Service<actix_http::Request, Response = ServiceResponse, Error = actix_web::Error>,
) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("lss.db")).unwrap();
    let state = web::Data::new(AppState::new(db));
    if ready {
        state.mark_ready();
    }
    let svc = test::init_service(
        App::new()
            .app_data(state.clone())
            .configure(http::configure),
    )
    .await;
    (dir, state, svc)
}

/// Sends a request, checks Content-Type is JSON, returns status + parsed body.
async fn call(
    svc: &impl Service<actix_http::Request, Response = ServiceResponse, Error = actix_web::Error>,
    req: test::TestRequest,
) -> (StatusCode, Value) {
    let resp = test::call_service(svc, req.to_request()).await;
    let status = resp.status();
    let ctype = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_owned());
    assert_eq!(
        ctype.as_deref(),
        Some("application/json"),
        "status {status}"
    );
    let bytes = to_bytes(resp.into_body()).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "body is not JSON ({e}): {:?}",
            String::from_utf8_lossy(&bytes)
        )
    });
    (status, body)
}

fn err(code: &str) -> Value {
    json!({ "error": code })
}

fn post_json(path: &str, body: impl Into<actix_web::web::Bytes>) -> test::TestRequest {
    test::TestRequest::post()
        .uri(path)
        .insert_header((header::CONTENT_TYPE, "application/json"))
        .set_payload(body.into())
}

fn valid_product() -> Value {
    json!({"id":"p3f9a0c2e71bd","name":"Pixel 10 Pro 256GB","category":"c8d21fa","price":79999,"stock":120})
}

// ---- /health ----------------------------------------------------------------

#[actix_web::test]
async fn health_is_503_until_ready_then_200() {
    let (_dir, state, svc) = app(false).await;
    let (status, body) = call(&svc, test::TestRequest::get().uri("/health")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, json!({"status":"starting"}));

    state.mark_ready();
    let (status, body) = call(&svc, test::TestRequest::get().uri("/health")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"status":"ok"}));
}

// ---- routing ----------------------------------------------------------------

#[actix_web::test]
async fn unknown_routes_are_json_404() {
    let (_dir, _state, svc) = app(true).await;
    for path in [
        "/",
        "/nope",
        "/api/products",
        "/products/a/b",
        "/users/u1",
        "/users/u1/orders/x",
    ] {
        let (status, body) = call(&svc, test::TestRequest::get().uri(path)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body, err("NOT_FOUND"), "{path}");
    }
}

#[actix_web::test]
async fn wrong_method_is_json_405() {
    let (_dir, _state, svc) = app(true).await;
    let cases = [
        (Method::POST, "/health"),
        (Method::DELETE, "/products"),
        (Method::PUT, "/products/p1"),
        (Method::GET, "/orders"),
        (Method::POST, "/users/u1/orders"),
    ];
    for (method, path) in cases {
        let req = test::TestRequest::default()
            .method(method.clone())
            .uri(path);
        let (status, body) = call(&svc, req).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {path}");
        assert_eq!(body, err("METHOD_NOT_ALLOWED"), "{method} {path}");
    }
}

// ---- POST /products: 400 cases ------------------------------------------------

#[actix_web::test]
async fn create_product_validation_errors() {
    let (_dir, _state, svc) = app(true).await;
    let mut bodies: Vec<Vec<u8>> = vec![
        b"".to_vec(),
        b"not json".to_vec(),
        b"[]".to_vec(),
        b"null".to_vec(),
        b"{\"id\":\"p1\"".to_vec(),
    ];
    // Each field missing, and each field with each wrong value.
    for key in ["id", "name", "category", "price", "stock"] {
        let mut v = valid_product();
        v.as_object_mut().unwrap().remove(key);
        bodies.push(serde_json::to_vec(&v).unwrap());
    }
    for (key, bad) in [
        ("id", json!("")),
        ("id", json!(123)),
        ("id", json!("x".repeat(65))),
        ("name", json!("")),
        ("name", json!(null)),
        ("name", json!("n".repeat(201))),
        ("category", json!("")),
        ("category", json!(["c"])),
        ("price", json!(-5)),
        ("price", json!(10.5)),
        ("price", json!("100")),
        ("price", json!(true)),
        ("price", json!(null)),
        ("stock", json!(-1)),
        ("stock", json!(1.5)),
        ("stock", json!("10")),
        ("stock", json!(false)),
    ] {
        let mut v = valid_product();
        v[key] = bad;
        bodies.push(serde_json::to_vec(&v).unwrap());
    }

    for body in bodies {
        let shown = String::from_utf8_lossy(&body).into_owned();
        let (status, resp) = call(&svc, post_json("/products", body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{shown}");
        assert_eq!(resp, err("VALIDATION_ERROR"), "{shown}");
    }
}

#[actix_web::test]
async fn oversized_body_is_validation_error() {
    let (_dir, _state, svc) = app(true).await;
    let mut v = valid_product();
    v["padding"] = json!("x".repeat(http::MAX_BODY_BYTES));
    let (status, body) = call(
        &svc,
        post_json("/products", serde_json::to_vec(&v).unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, err("VALIDATION_ERROR"));

    let (status, body) = call(
        &svc,
        post_json("/orders", vec![b' '; http::MAX_BODY_BYTES + 1]),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, err("VALIDATION_ERROR"));
}

#[actix_web::test]
async fn extra_fields_are_ignored_by_validation() {
    let (_dir, _state, svc) = app(true).await;
    let mut v = valid_product();
    v["_id"] = json!("65f0c0ffee");
    v["__v"] = json!(0);
    // Passes validation, so it reaches the stub rather than being a 400.
    let (status, _) = call(
        &svc,
        post_json("/products", serde_json::to_vec(&v).unwrap()),
    )
    .await;
    assert_ne!(status, StatusCode::BAD_REQUEST);
}

// ---- GET /products?category= : 400 cases --------------------------------------

#[actix_web::test]
async fn list_products_validation_errors() {
    let (_dir, _state, svc) = app(true).await;
    for q in [
        "/products",
        "/products?page=2",
        "/products?category=",
        "/products?category=c8d21fa&limit=80",
        "/products?category=c8d21fa&limit=0",
        "/products?category=c8d21fa&limit=abc",
        "/products?category=c8d21fa&page=0",
        "/products?category=c8d21fa&page=-1",
        "/products?category=c8d21fa&page=1.5",
        "/products?category=c8d21fa&page=",
        "/products?category=c8d21fa&limit=%2B5",
    ] {
        let (status, body) = call(&svc, test::TestRequest::get().uri(q)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{q}");
        assert_eq!(body, err("VALIDATION_ERROR"), "{q}");
    }
}

// ---- POST /orders: 400 cases ----------------------------------------------------

#[actix_web::test]
async fn create_order_validation_errors() {
    let (_dir, _state, svc) = app(true).await;
    for body in [
        &br""[..],
        br"{}",
        br"[]",
        br#"{"userId":"u7b0e44c19a2f"}"#,
        br#"{"productId":"p3f9a0c2e71bd"}"#,
        br#"{"userId":"","productId":"p1"}"#,
        br#"{"userId":"u1","productId":""}"#,
        br#"{"userId":5,"productId":"p1"}"#,
        br#"{"userId":"u1","productId":null}"#,
        br#"{"userId":"u1","productId":["p1"]}"#,
    ] {
        let shown = String::from_utf8_lossy(body).into_owned();
        let (status, resp) = call(&svc, post_json("/orders", body.to_vec())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{shown}");
        assert_eq!(resp, err("VALIDATION_ERROR"), "{shown}");
    }
}

// ---- task-1 stubs: valid input reaches the handler and answers JSON 501 ---------

#[actix_web::test]
async fn business_routes_stub_with_json_501() {
    let (_dir, _state, svc) = app(true).await;
    let reqs = [
        post_json("/products", serde_json::to_vec(&valid_product()).unwrap()),
        test::TestRequest::get().uri("/products/p3f9a0c2e71bd"),
        test::TestRequest::get().uri("/products?category=c8d21fa&page=1&limit=2"),
        post_json("/orders", br#"{"userId":"u1","productId":"p1"}"#.to_vec()),
        test::TestRequest::get().uri("/users/u7b0e44c19a2f/orders"),
    ];
    for req in reqs {
        let (status, body) = call(&svc, req).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(body, err("NOT_IMPLEMENTED"));
    }
}
