//! The spec's sample shop (section 04), end to end over real HTTP, in both read
//! modes.

#[macro_use]
mod common;

use common::*;
use serde_json::{Value, json};

const CAT: &str = "c8d21fa";
const PIXEL: &str = "p3f9a0c2e71bd";
const BOTTLE: &str = "p0c4be91a3f25";
const CASE: &str = "p7d2e05b1c9aa";
const BUYER: &str = "u7b0e44c19a2f";
const NOBODY: &str = "u3c9a1f08d7e2";

fn pixel() -> Value {
    product(PIXEL, "Pixel 10 Pro 256GB", CAT, 79999, 120)
}

async fn seed(srv: &TestServer, c: &reqwest::Client) {
    for p in [
        pixel(),
        product(BOTTLE, "Steel Bottle 1L", CAT, 349, 100_000),
        product(CASE, "Phone Case", CAT, 349, 100_000),
    ] {
        let r = create(srv, c, &p).await;
        assert_eq!(r.json(), p, "201 body is the product as stored");
    }
}

async fn health_and_unknown_routes(srv: &TestServer) {
    let c = client();
    let r = get(&c, &srv.url("/health")).await;
    assert_eq!(r.status.as_u16(), 200);
    assert_eq!(r.json(), json!({"status":"ok"}));

    for path in ["/", "/nope", "/api/products", "/products/a/b", "/users/u1"] {
        get(&c, &srv.url(path)).await.assert_error(404, "NOT_FOUND");
    }
}

async fn create_and_read_products(srv: &TestServer) {
    let c = client();
    seed(srv, &c).await;
    let r = get(&c, &srv.url(&format!("/products/{PIXEL}"))).await;
    assert_eq!(r.status.as_u16(), 200);
    assert_eq!(r.json(), pixel());
    get(&c, &srv.url("/products/p000000000000"))
        .await
        .assert_error(404, "PRODUCT_NOT_FOUND");
}

async fn list_products(srv: &TestServer) {
    let c = client();
    seed(srv, &c).await;

    let r = get(
        &c,
        &srv.url(&format!("/products?category={CAT}&page=1&limit=2")),
    )
    .await;
    assert_eq!(r.status.as_u16(), 200);
    let v = r.json();
    let ids: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    // Same price 349: ordered by id. Pixel (79999) is last.
    assert_eq!(ids, [BOTTLE, CASE]);
    assert_eq!(
        (v["page"].as_u64(), v["limit"].as_u64(), v["total"].as_u64()),
        (Some(1), Some(2), Some(3))
    );
    assert_eq!(
        v["items"][0],
        product(BOTTLE, "Steel Bottle 1L", CAT, 349, 100_000)
    );

    let r = get(
        &c,
        &srv.url(&format!("/products?category={CAT}&page=2&limit=2")),
    )
    .await;
    assert_eq!(r.json()["items"], json!([pixel()]));

    let r = get(&c, &srv.url(&format!("/products?category={CAT}&page=20"))).await;
    assert_eq!(r.json(), json!({"items":[],"page":20,"limit":20,"total":3}));

    let r = get(&c, &srv.url("/products?category=c000000")).await;
    assert_eq!(r.json(), json!({"items":[],"page":1,"limit":20,"total":0}));

    for q in [
        format!("category={CAT}&limit=80"),
        "page=2".into(),
        format!("category={CAT}&page=0"),
        format!("category={CAT}&page=abc"),
        format!("category={CAT}&page=1.5"),
        format!("category={CAT}&limit=0"),
        format!("category={CAT}&limit="),
    ] {
        get(&c, &srv.url(&format!("/products?{q}")))
            .await
            .assert_error(400, "VALIDATION_ERROR");
    }
}

async fn duplicate_product_is_409_and_unchanged(srv: &TestServer) {
    let c = client();
    seed(srv, &c).await;
    post(
        &c,
        &srv.url("/products"),
        &product(PIXEL, "Pixel 10 Pro 512GB", CAT, 89999, 50),
    )
    .await
    .assert_error(409, "PRODUCT_EXISTS");
    let r = get(&c, &srv.url(&format!("/products/{PIXEL}"))).await;
    assert_eq!(r.json(), pixel());
    // The failed create didn't count towards the category either.
    let r = get(&c, &srv.url(&format!("/products?category={CAT}"))).await;
    assert_eq!(r.json()["total"], 3);
}

async fn create_product_validation(srv: &TestServer) {
    let c = client();
    let url = srv.url("/products");
    let mut bad: Vec<Vec<u8>> = Vec::new();
    for (k, v) in [
        ("price", json!(-5)),
        ("price", json!("100")),
        ("price", json!(10.5)),
        ("stock", json!(null)),
    ] {
        let mut p = product("pv1", "Kettle", CAT, 1, 1);
        p[k] = v;
        bad.push(serde_json::to_vec(&p).unwrap());
    }
    let mut missing = product("pv1", "Kettle", CAT, 1, 1);
    missing.as_object_mut().unwrap().remove("name");
    bad.push(serde_json::to_vec(&missing).unwrap());
    bad.push(b"[]".to_vec());
    bad.push(b"not json".to_vec());
    for body in bad {
        post_raw(&c, &url, body)
            .await
            .assert_error(400, "VALIDATION_ERROR");
    }
    // None of them created anything.
    get(&c, &srv.url("/products/pv1"))
        .await
        .assert_error(404, "PRODUCT_NOT_FOUND");
}

async fn orders_flow(srv: &TestServer) {
    let c = client();
    seed(srv, &c).await;
    let orders_url = srv.url("/orders");

    let r = post(&c, &orders_url, &order(BUYER, PIXEL)).await;
    assert_eq!(r.status.as_u16(), 201, "{}", r.text);
    let v = r.json();
    let order_id = v["orderId"].as_str().unwrap().to_owned();
    assert!(!order_id.is_empty());
    assert_eq!(
        (v["userId"].as_str(), v["productId"].as_str()),
        (Some(BUYER), Some(PIXEL))
    );
    assert_eq!(stock_of(srv, &c, PIXEL).await, 119);

    // A product with one unit, bought once.
    create(srv, &c, &product("plast1", "Last One", "ctest", 100, 1)).await;
    assert_eq!(
        post(&c, &orders_url, &order(NOBODY, "plast1"))
            .await
            .status
            .as_u16(),
        201
    );

    let snapshot = |srv: &TestServer, c: &reqwest::Client| {
        let (srv, c) = (srv.base.clone(), c.clone());
        async move {
            let c = &c;
            let mut out = Vec::new();
            for path in [
                format!("/products/{PIXEL}"),
                "/products/plast1".to_owned(),
                format!("/users/{BUYER}/orders"),
                format!("/users/{NOBODY}/orders"),
                format!("/products?category={CAT}"),
            ] {
                out.push(get(c, &format!("{srv}{path}")).await.text);
            }
            out
        }
    };
    let before = snapshot(srv, &c).await;

    // Every error, in spec order 400 -> 404 -> 409, and none of them changes anything.
    post(&c, &orders_url, &json!({"userId": BUYER}))
        .await
        .assert_error(400, "VALIDATION_ERROR");
    post(&c, &orders_url, &json!({"userId": BUYER, "productId": 123}))
        .await
        .assert_error(400, "VALIDATION_ERROR");
    post(&c, &orders_url, &order(BUYER, "p000000000000"))
        .await
        .assert_error(404, "PRODUCT_NOT_FOUND");
    post(&c, &orders_url, &order(BUYER, PIXEL))
        .await
        .assert_error(409, "ALREADY_PURCHASED");
    post(&c, &orders_url, &order(BUYER, "plast1"))
        .await
        .assert_error(409, "SOLD_OUT");
    // Already bought *and* sold out: either 409 is accepted.
    let r = post(&c, &orders_url, &order(NOBODY, "plast1")).await;
    assert_eq!(r.status.as_u16(), 409);
    assert!(["ALREADY_PURCHASED", "SOLD_OUT"].contains(&r.json()["error"].as_str().unwrap()));

    assert_eq!(snapshot(srv, &c).await, before, "errors changed nothing");

    let orders = orders_of(srv, &c, BUYER).await;
    assert_eq!(
        orders,
        vec![json!({"orderId": order_id, "productId": PIXEL})]
    );
    assert_eq!(orders_of(srv, &c, NOBODY).await.len(), 1);
    let r = get(&c, &srv.url("/users/u000000000000/orders")).await;
    assert_eq!(r.json(), json!({"userId":"u000000000000","orders":[]}));
}

async fn unknown_user_has_no_orders(srv: &TestServer) {
    let c = client();
    let r = get(&c, &srv.url(&format!("/users/{NOBODY}/orders"))).await;
    assert_eq!(r.status.as_u16(), 200);
    assert_eq!(r.json(), json!({"userId": NOBODY, "orders": []}));
}

async fn awkward_strings_round_trip(srv: &TestServer) {
    let c = client();
    let name = "Quote \" backslash \\ new\nline tab\t हिन्दी ✓ 😀";
    let p = product("pweird1", name, "c\"weird\\", 5, 3);
    let r = create(srv, &c, &p).await;
    assert_eq!(r.json(), p);
    assert_eq!(get(&c, &srv.url("/products/pweird1")).await.json(), p);
    let r = get(&c, &srv.url("/products?category=c%22weird%5C")).await;
    assert_eq!(r.json()["items"], json!([p]));

    // Ids with JSON-special characters in the user id survive too.
    let user = "u\"q";
    let r = post(&c, &srv.url("/orders"), &order(user, "pweird1")).await;
    assert_eq!(r.status.as_u16(), 201);
    assert_eq!(r.json()["userId"], user);
    let r = get(&c, &srv.url("/users/u%22q/orders")).await;
    assert_eq!(r.json()["userId"], user);
    assert_eq!(r.json()["orders"][0]["productId"], "pweird1");
}

both_modes!(
    health_and_unknown_routes,
    create_and_read_products,
    list_products,
    duplicate_product_is_409_and_unchanged,
    create_product_validation,
    orders_flow,
    unknown_user_has_no_orders,
    awkward_strings_round_trip,
);
