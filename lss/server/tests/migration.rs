//! Upgrading an existing database to schema version 2.
//!
//! Two starting points exist in the wild: a version-1 database (`user_version =
//! 1`), and a database created by the task-1 binary, which has the same tables but
//! never set `user_version` (so it reads 0). Both must migrate in place, keep
//! every row, and then serve correctly in both modes.

mod common;

use common::*;
use lss_server::db::SCHEMA_V1;
use lss_server::render::build_json_prefix;
use rusqlite::{Connection, OpenFlags, params};
use serde_json::json;

const PRODUCTS: &[(&str, &str, &str, i64, i64)] = &[
    ("pm1", "Plain", "cm1", 349, 10),
    ("pm2", "Quote \" and \\ back", "cm1", 349, 0),
    ("pm3", "Cheap", "cm1", 100, 5),
    ("pm4", "हिन्दी ✓", "cm2", 7, 1),
];
const ORDERS: &[(&str, &str)] = &[("um1", "pm1"), ("um1", "pm3"), ("um2", "pm1")];

/// Builds a pre-v2 database by hand, as the task-1 binary would have left it.
fn build_old_db(user_version: i64) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let conn = Connection::open(dir.path().join(DB_FILE)).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL").unwrap();
    conn.execute_batch(SCHEMA_V1).unwrap();
    for (id, name, cat, price, stock) in PRODUCTS {
        conn.execute(
            "INSERT INTO products (id, name, category, price, stock) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, name, cat, price, stock],
        )
        .unwrap();
    }
    for (user, product) in ORDERS {
        conn.execute(
            "INSERT INTO orders (user_id, product_id) VALUES (?1, ?2)",
            params![user, product],
        )
        .unwrap();
    }
    conn.execute_batch(&format!("PRAGMA user_version = {user_version}"))
        .unwrap();
    dir
}

async fn migrates(mode: ReadSource, from_version: i64) {
    let srv = TestServer::start_with_dir(mode, build_old_db(from_version));

    // Inspect the migrated file directly (read-only, alongside the server).
    {
        let db =
            Connection::open_with_flags(srv.db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let v: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 2);
        for (id, name, cat, price, _) in PRODUCTS {
            let prefix: String = db
                .query_row(
                    "SELECT json_prefix FROM products WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(prefix, build_json_prefix(id, name, cat, *price));
        }
        let mut totals: Vec<(String, i64)> = db
            .prepare("SELECT category, total FROM categories ORDER BY category")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        totals.sort();
        assert_eq!(totals, vec![("cm1".to_owned(), 3), ("cm2".to_owned(), 1)]);
    }

    let c = client();
    for (id, name, cat, price, stock) in PRODUCTS {
        let r = get(&c, &srv.url(&format!("/products/{id}"))).await;
        assert_eq!(r.json(), product(id, name, cat, *price, *stock));
    }
    let r = get(&c, &srv.url("/products?category=cm1")).await;
    let ids: Vec<String> = r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["pm3", "pm1", "pm2"]);
    assert_eq!(r.json()["total"], 3);
    assert_eq!(
        get(&c, &srv.url("/users/um1/orders")).await.json(),
        json!({"userId":"um1","orders":[{"orderId":"o_1","productId":"pm1"},{"orderId":"o_2","productId":"pm3"}]})
    );

    // Old data keeps its rules, new writes work and count towards the totals.
    post(&c, &srv.url("/orders"), &order("um2", "pm1"))
        .await
        .assert_error(409, "ALREADY_PURCHASED");
    post(&c, &srv.url("/orders"), &order("um3", "pm2"))
        .await
        .assert_error(409, "SOLD_OUT");
    let r = post(&c, &srv.url("/orders"), &order("um3", "pm1")).await;
    assert_eq!(r.json()["orderId"], "o_4");
    assert_eq!(stock_of(&srv, &c, "pm1").await, 9);
    create(&srv, &c, &product("pm5", "New", "cm1", 1, 1)).await;
    assert_eq!(
        get(&c, &srv.url("/products?category=cm1")).await.json()["total"],
        4
    );
    drop(c);

    // A second start on the migrated database is a no-op migration.
    let srv = TestServer::start_with_dir(mode, srv.stop());
    let c = client();
    assert_eq!(
        get(&c, &srv.url("/products?category=cm1")).await.json()["total"],
        4
    );
    drop(c);
    srv.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v1_sqlite() {
    migrates(ReadSource::Sqlite, 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v1_memory() {
    migrates(ReadSource::Memory, 1).await;
}

/// A database written by the task-1 binary: v1 tables, user_version still 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task1_db_sqlite() {
    migrates(ReadSource::Sqlite, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task1_db_memory() {
    migrates(ReadSource::Memory, 0).await;
}

/// A database from a newer binary is refused, not modified.
#[test]
fn newer_version_is_refused() {
    let dir = build_old_db(1);
    let path = dir.path().join(DB_FILE);
    Connection::open(&path)
        .unwrap()
        .execute_batch("PRAGMA user_version = 3")
        .unwrap();
    let err = lss_server::db::open(&path).unwrap_err();
    assert!(
        format!("{err:#}").contains("newer than this binary"),
        "{err:#}"
    );
}
