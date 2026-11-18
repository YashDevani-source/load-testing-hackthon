//! Database setup: pragmas, idempotent schema/migrations, and the two constraints
//! that make overselling and double-buying impossible at the storage level.
//! (DB layer only: no HTTP, so no read mode applies.)

use std::path::PathBuf;
use std::time::Duration;

use lss_server::db::{self, checkpoint};
use rusqlite::{Connection, ErrorCode, params};
use tempfile::TempDir;

fn temp_db() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    // A nested path also proves open() creates missing directories.
    let path = dir.path().join("nested").join("lss.db");
    (dir, path)
}

fn pragma_i64(conn: &Connection, name: &str) -> i64 {
    conn.query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
        .unwrap()
}

fn insert_product(conn: &Connection, id: &str, stock: i64) {
    conn.execute(
        "INSERT INTO products (id, name, category, price, stock) VALUES (?1, 'n', 'c', 100, ?2)",
        params![id, stock],
    )
    .unwrap();
}

fn constraint_code(err: &rusqlite::Error) -> Option<ErrorCode> {
    err.sqlite_error_code()
}

#[test]
fn pragmas_applied() {
    let (_dir, path) = temp_db();
    let conn = db::open(&path).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    assert_eq!(pragma_i64(&conn, "synchronous"), 1, "NORMAL");
    assert_eq!(pragma_i64(&conn, "wal_autocheckpoint"), 0);
    assert_eq!(pragma_i64(&conn, "temp_store"), 2, "MEMORY");
    assert_eq!(pragma_i64(&conn, "busy_timeout"), 5000);
}

#[test]
fn schema_is_idempotent_and_data_survives_reopen() {
    let (_dir, path) = temp_db();
    {
        let conn = db::open(&path).unwrap();
        insert_product(&conn, "p1", 7);
        // Re-applying the v1 schema on a live database is a no-op.
        conn.execute_batch(db::SCHEMA_V1).unwrap();
    }
    // Reopen: schema applied again, existing row untouched.
    let conn = db::open(&path).unwrap();
    let stock: i64 = conn
        .query_row("SELECT stock FROM products WHERE id = 'p1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(stock, 7);
    let indexes: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = 'products_category_price_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(indexes, 1);
}

#[test]
fn check_constraint_rejects_negative_stock() {
    let (_dir, path) = temp_db();
    let conn = db::open(&path).unwrap();
    insert_product(&conn, "p1", 0);

    // An unconditional decrement on a sold-out product must fail in the database.
    let err = conn
        .execute("UPDATE products SET stock = stock - 1 WHERE id = 'p1'", [])
        .unwrap_err();
    assert_eq!(constraint_code(&err), Some(ErrorCode::ConstraintViolation));
    let stock: i64 = conn
        .query_row("SELECT stock FROM products WHERE id = 'p1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(stock, 0);

    // Inserting with negative stock or price fails too.
    let err = conn
        .execute(
            "INSERT INTO products (id, name, category, price, stock) VALUES ('p2', 'n', 'c', 1, -1)",
            [],
        )
        .unwrap_err();
    assert_eq!(constraint_code(&err), Some(ErrorCode::ConstraintViolation));
    let err = conn
        .execute(
            "INSERT INTO products (id, name, category, price, stock) VALUES ('p3', 'n', 'c', -1, 1)",
            [],
        )
        .unwrap_err();
    assert_eq!(constraint_code(&err), Some(ErrorCode::ConstraintViolation));
}

#[test]
fn primary_key_rejects_duplicate_product() {
    let (_dir, path) = temp_db();
    let conn = db::open(&path).unwrap();
    insert_product(&conn, "p1", 5);
    let err = conn
        .execute(
            "INSERT INTO products (id, name, category, price, stock) VALUES ('p1', 'other', 'c', 9, 9)",
            [],
        )
        .unwrap_err();
    assert_eq!(constraint_code(&err), Some(ErrorCode::ConstraintViolation));
}

#[test]
fn unique_rejects_duplicate_user_product_order() {
    let (_dir, path) = temp_db();
    let conn = db::open(&path).unwrap();
    let insert = "INSERT INTO orders (user_id, product_id) VALUES (?1, ?2)";
    conn.execute(insert, params!["u1", "p1"]).unwrap();
    let first_id = conn.last_insert_rowid();

    let err = conn.execute(insert, params!["u1", "p1"]).unwrap_err();
    assert_eq!(constraint_code(&err), Some(ErrorCode::ConstraintViolation));

    // Same user, other product; other user, same product: both fine.
    conn.execute(insert, params!["u1", "p2"]).unwrap();
    conn.execute(insert, params!["u2", "p1"]).unwrap();
    assert!(conn.last_insert_rowid() > first_id, "order ids increase");

    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM orders WHERE user_id = 'u1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn checkpoints_run_and_truncate_wal() {
    let (_dir, path) = temp_db();
    let conn = db::open(&path).unwrap();
    for i in 0..100 {
        insert_product(&conn, &format!("p{i}"), 1);
    }
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    assert!(wal.metadata().unwrap().len() > 0, "writes went to the WAL");

    // The background thread uses its own connection and must not disturb ours.
    let cp = checkpoint::Checkpointer::spawn(&path, Duration::from_millis(20)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    insert_product(&conn, "after", 1);
    cp.stop();

    let (busy, _, _) = checkpoint::run(&conn, checkpoint::Mode::Truncate).unwrap();
    assert_eq!(busy, 0);
    assert_eq!(wal.metadata().unwrap().len(), 0, "TRUNCATE empties the WAL");
    let count: i64 = conn
        .query_row("SELECT count(*) FROM products", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 101);
}
