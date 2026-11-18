//! Races: the rules of the sale under simultaneous requests, against the real
//! server (2 workers) over real sockets, in both read modes.
//!
//! 200 requests in flight at once is plenty to expose a race on a 2-worker
//! server, and keeps client + server sockets well inside per-process fd limits.

#[macro_use]
mod common;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use common::*;
use futures::StreamExt;
use futures::stream;
use serde_json::Value;

const IN_FLIGHT: usize = 200;

/// Sends every (user, product) order concurrently; returns (user, response) pairs.
async fn fire(
    srv: &TestServer,
    c: &reqwest::Client,
    orders: Vec<(String, String)>,
) -> Vec<(String, Resp)> {
    let url = srv.url("/orders");
    stream::iter(orders)
        .map(|(user, product)| {
            let (c, url) = (c.clone(), url.clone());
            async move {
                let r = post(&c, &url, &order(&user, &product)).await;
                (user, r)
            }
        })
        .buffer_unordered(IN_FLIGHT)
        .collect()
        .await
}

/// While `stop` is false, keeps reading the product (and its category list) and
/// asserts no response ever shows stock below 0 or above the starting stock.
fn watch_stock(
    srv: &TestServer,
    c: &reqwest::Client,
    id: &str,
    category: &str,
    start: i64,
    stop: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<usize> {
    let (c, product_url, list_url) = (
        c.clone(),
        srv.url(&format!("/products/{id}")),
        srv.url(&format!("/products?category={category}&limit=50")),
    );
    tokio::spawn(async move {
        let mut seen = 0;
        while !stop.load(Ordering::Relaxed) {
            let stock = get(&c, &product_url).await.json()["stock"]
                .as_i64()
                .unwrap();
            assert!((0..=start).contains(&stock), "product showed stock {stock}");
            for item in get(&c, &list_url).await.json()["items"].as_array().unwrap() {
                let s = item["stock"].as_i64().unwrap();
                assert!((0..=start).contains(&s), "list showed stock {s}");
            }
            seen += 1;
        }
        seen
    })
}

fn codes(results: &[(String, Resp)]) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    for (_, r) in results {
        let key = match r.status.as_u16() {
            201 => "201".to_owned(),
            s => format!("{s} {}", r.json()["error"].as_str().unwrap_or("?")),
        };
        *m.entry(key).or_default() += 1;
    }
    m
}

/// Spec test 1, scaled up: `stock` units, 600 different users at the same time.
async fn last_unit(srv: &TestServer) {
    let c = client();
    for stock in [1i64, 5, 150] {
        let id = format!("plast{stock}");
        let cat = format!("clast{stock}");
        create(srv, &c, &product(&id, "Last Units", &cat, 100, stock)).await;

        let stop = Arc::new(AtomicBool::new(false));
        let watcher = watch_stock(srv, &c, &id, &cat, stock, Arc::clone(&stop));
        let users: Vec<(String, String)> = (0..600)
            .map(|i| (format!("u{stock}x{i}"), id.clone()))
            .collect();
        let results = fire(srv, &c, users).await;
        stop.store(true, Ordering::Relaxed);
        assert!(watcher.await.unwrap() > 0);

        let counts = codes(&results);
        let wins = stock as usize;
        assert_eq!(counts.get("201"), Some(&wins), "{counts:?}");
        assert_eq!(
            counts.get("409 SOLD_OUT"),
            Some(&(600 - wins)),
            "{counts:?}"
        );
        assert_eq!(stock_of(srv, &c, &id).await, 0);

        // Sum of orders for this product across those users' lists == stock.
        let mut listed = 0;
        for (user, _) in &results {
            listed += orders_of(srv, &c, user)
                .await
                .iter()
                .filter(|o| o["productId"] == id.as_str())
                .count();
        }
        assert_eq!(listed, wins);
    }
}

/// Spec test 2, scaled up: the same order 50 times at once.
async fn double_click(srv: &TestServer) {
    let c = client();
    create(
        srv,
        &c,
        &product("pclick1", "Click Test", "cclick", 100, 1000),
    )
    .await;
    let results = fire(srv, &c, vec![("udouble1".into(), "pclick1".into()); 50]).await;
    let counts = codes(&results);
    assert_eq!(counts.get("201"), Some(&1), "{counts:?}");
    assert_eq!(counts.get("409 ALREADY_PURCHASED"), Some(&49), "{counts:?}");
    assert_eq!(stock_of(srv, &c, "pclick1").await, 999);
    assert_eq!(orders_of(srv, &c, "udouble1").await.len(), 1);
}

/// The ramp's order mix on a regular product: first-time buyers, 2% sent twice
/// at the same instant, then 3% retries by earlier winners.
async fn mixed(srv: &TestServer) {
    let c = client();
    let id = "pmix1";
    let start = 100_000i64;
    create(srv, &c, &product(id, "Mixed", "cmix", 349, start)).await;

    let stop = Arc::new(AtomicBool::new(false));
    let watcher = watch_stock(srv, &c, id, "cmix", start, Arc::clone(&stop));

    // Phase 1: 1900 first-time buyers; every 50th is sent twice (2%), adjacent in
    // the stream so both copies are in flight together. 1938 requests.
    let mut phase1 = Vec::new();
    for i in 0..1900 {
        let user = format!("um{i}");
        if i % 50 == 0 {
            phase1.push((user.clone(), id.to_owned()));
        }
        phase1.push((user, id.to_owned()));
    }
    let mut results = fire(srv, &c, phase1).await;

    // Phase 2: 62 retries (3% of 2000) of orders that already won. 2000 total.
    let winners: Vec<String> = results
        .iter()
        .filter(|(_, r)| r.status.as_u16() == 201)
        .map(|(u, _)| u.clone())
        .take(62)
        .collect();
    let retries = fire(
        srv,
        &c,
        winners.iter().map(|u| (u.clone(), id.to_owned())).collect(),
    )
    .await;
    for (_, r) in &retries {
        r.assert_error(409, "ALREADY_PURCHASED");
    }
    results.extend(retries);
    assert_eq!(results.len(), 2000);
    stop.store(true, Ordering::Relaxed);
    watcher.await.unwrap();

    // Regular product, not sold out: every distinct user won exactly once, and
    // every other answer was ALREADY_PURCHASED (never SOLD_OUT).
    let counts = codes(&results);
    let wins = counts.get("201").copied().unwrap_or(0);
    assert_eq!(wins, 1900, "{counts:?}");
    assert_eq!(
        counts.get("409 ALREADY_PURCHASED"),
        Some(&100),
        "{counts:?}"
    );
    assert_eq!(stock_of(srv, &c, id).await, start - wins as i64);

    // Every 201 orderId is in its user's list, and the lists hold nothing else.
    let mut won: HashMap<String, HashSet<String>> = HashMap::new();
    for (user, r) in &results {
        if r.status.as_u16() == 201 {
            let v: Value = r.json();
            assert_eq!(v["userId"], user.as_str());
            won.entry(user.clone())
                .or_default()
                .insert(v["orderId"].as_str().unwrap().to_owned());
        }
    }
    let users: HashSet<&String> = results.iter().map(|(u, _)| u).collect();
    for user in users {
        let listed: HashSet<String> = orders_of(srv, &c, user)
            .await
            .iter()
            .map(|o| {
                assert_eq!(o["productId"], id);
                o["orderId"].as_str().unwrap().to_owned()
            })
            .collect();
        assert_eq!(
            listed.len(),
            1,
            "user {user} has {} orders for {id}",
            listed.len()
        );
        assert_eq!(Some(&listed), won.get(user), "user {user}");
    }
}

both_modes!(last_unit, double_click, mixed);
