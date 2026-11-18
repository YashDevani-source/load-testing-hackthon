//! Data survives a restart: every read answers byte-for-byte the same after the
//! server is stopped and started again on the same database, in both modes. In
//! memory mode this proves the caches are rebuilt from SQLite, i.e. memory was
//! never the only copy.

mod common;

use common::*;

/// Every GET whose answer must survive the restart.
fn read_paths() -> Vec<String> {
    let mut paths = vec![];
    for i in 0..30 {
        paths.push(format!("/products/pp{i}"));
    }
    for cat in ["cpa", "cpb", "cpnone"] {
        for (page, limit) in [(1, 20), (2, 20), (1, 7), (3, 7), (5, 7), (1, 50)] {
            paths.push(format!(
                "/products?category={cat}&page={page}&limit={limit}"
            ));
        }
    }
    for u in 0..12 {
        paths.push(format!("/users/up{u}/orders"));
    }
    paths.push("/users/unobody/orders".into());
    paths
}

async fn snapshot(srv: &TestServer, c: &reqwest::Client) -> Vec<(String, u16, String)> {
    let mut out = vec![];
    for p in read_paths() {
        let r = get(c, &srv.url(&p)).await;
        out.push((p, r.status.as_u16(), r.text));
    }
    out
}

async fn survives_restart(mode: ReadSource) {
    let srv = TestServer::start(mode);
    let before = {
        let c = client();
        for i in 0..30i64 {
            let cat = if i % 2 == 0 { "cpa" } else { "cpb" };
            create(
                &srv,
                &c,
                &product(
                    &format!("pp{i}"),
                    &format!("Item \"{i}\""),
                    cat,
                    (i % 4) * 100,
                    3,
                ),
            )
            .await;
        }
        // Users buy overlapping products; some sell out (stock 3).
        for u in 0..12 {
            for p in [u % 5, (u + 1) % 5, 10 + u % 3] {
                let r = post(
                    &c,
                    &srv.url("/orders"),
                    &order(&format!("up{u}"), &format!("pp{p}")),
                )
                .await;
                assert!([201, 409].contains(&r.status.as_u16()), "{}", r.text);
            }
        }
        snapshot(&srv, &c).await
        // client dropped here, so graceful stop doesn't wait on keep-alives
    };
    let dir = srv.stop();

    let srv = TestServer::start_with_dir(mode, dir);
    let c = client();
    let after = snapshot(&srv, &c).await;
    for (b, a) in before.iter().zip(&after) {
        assert_eq!(b, a);
    }

    // Writing continues correctly: no reused order id, rules still enforced.
    // up0 bought pp0, which has since sold out: the spec accepts either 409.
    let r = post(&c, &srv.url("/orders"), &order("up0", "pp0")).await;
    assert_eq!(r.status.as_u16(), 409);
    assert!(["ALREADY_PURCHASED", "SOLD_OUT"].contains(&r.json()["error"].as_str().unwrap()));
    let r = post(&c, &srv.url("/orders"), &order("unew", "pp20")).await;
    assert_eq!(r.status.as_u16(), 201);
    let new_id = r.json()["orderId"].as_str().unwrap().to_owned();
    for u in 0..12 {
        for o in orders_of(&srv, &c, &format!("up{u}")).await {
            assert_ne!(o["orderId"], new_id.as_str());
        }
    }
    drop(c);
    srv.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn survives_restart_sqlite() {
    survives_restart(ReadSource::Sqlite).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn survives_restart_memory() {
    survives_restart(ReadSource::Memory).await;
}

/// Restarting in the *other* mode reads the same data too: the modes share one
/// database and differ only in where reads come from.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn switching_modes_keeps_data() {
    let srv = TestServer::start(ReadSource::Sqlite);
    let before = {
        let c = client();
        create(&srv, &c, &product("psw1", "Switch", "csw", 10, 5)).await;
        assert_eq!(
            post(&c, &srv.url("/orders"), &order("usw", "psw1"))
                .await
                .status
                .as_u16(),
            201
        );
        vec![
            get(&c, &srv.url("/products/psw1")).await.text,
            get(&c, &srv.url("/products?category=csw")).await.text,
            get(&c, &srv.url("/users/usw/orders")).await.text,
        ]
    };
    let srv = TestServer::start_with_dir(ReadSource::Memory, srv.stop());
    let c = client();
    let after = vec![
        get(&c, &srv.url("/products/psw1")).await.text,
        get(&c, &srv.url("/products?category=csw")).await.text,
        get(&c, &srv.url("/users/usw/orders")).await.text,
    ];
    assert_eq!(before, after);
    drop(c);
    srv.stop();
}
