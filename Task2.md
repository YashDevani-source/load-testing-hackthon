# Task 2 of 4 — Core logic: the order transaction, all 6 endpoints, two read modes

This builds on the task-1 scaffold in this repository (Rust, actix-web 4, SQLite in-process via
`rusqlite`, one process). The attached `last-server-standing-rules.html` is still the source of
truth. If the scaffold's file layout differs slightly from what's named here, adapt to it and tell me.

**Scope:** replace every `TODO(task-2)` stub with the real implementation, add a schema migration,
implement **two read modes behind one switch**, and add the correctness tests below. All task-1 rules
still apply: no typed actix extractors for input, no `unwrap`/`expect`/panics in request paths (the
clippy lints stay on), no per-request logging, no `web::block`/`spawn_blocking` around SQLite.

---

## 1. The design (also write this into `docs/DESIGN.md`)

- **SQLite on disk is the only source of truth.** Every write is committed to SQLite before the
  client gets a success response, and **every order is accepted or rejected inside a SQLite
  transaction**. Nothing is ever decided from a cached value.
- **Reads have two modes**, chosen at startup by the env var **`READ_SOURCE`**:
  - **`sqlite` (the default):** every read is a query on SQLite. No application-level copy of the
    data exists in memory. Use this until the organizers confirm in-memory caching is fine.
  - **`memory`:** reads are served from an in-memory copy that is updated right after each commit,
    while the write lock is still held. SQLite stays the source of truth; memory is only a cache
    (spec section 09: "Memory is fine as a cache, but not as the only copy").
- **Writes are identical in both modes.** In `memory` mode they also update the in-memory copy
  after committing.
- Log the active mode at startup. Add `READ_SOURCE=sqlite` to `deploy/lss.service`.

## 2. Schema migration (`PRAGMA user_version`)

Version the schema with `PRAGMA user_version`. Task 1's schema is version 1. Version 2 adds:
- `products.json_prefix TEXT NOT NULL`: the product's fixed fields, already serialised as JSON:
  `{"id":<id>,"name":<name>,"category":<category>,"price":<price>,"stock":`
  Strings are escaped with `serde_json`, never by hand (names can contain quotes, backslashes,
  unicode). A product response is `json_prefix + itoa(stock) + "}"`, so no read ever has to
  re-escape strings. This is a **precomputed column in the database**, not a memory cache.
- `categories(category TEXT PRIMARY KEY, total INTEGER NOT NULL)`, kept up to date in the same
  transaction as each product insert, so a list's `total` is one primary-key lookup.
- Keep the index on `products(category, price, id)`. The `UNIQUE (user_id, product_id)` on `orders`
  already gives an index that starts with `user_id`, which the user-orders query uses.

Migrations run at startup, inside a transaction, and are idempotent. If the DB is at version 1 and
already has products, backfill `json_prefix` and `categories`.

## 3. Connections

- **One write connection** behind a `parking_lot::Mutex`, as in task 1 (WAL, `synchronous=NORMAL`,
  `wal_autocheckpoint=0`, background `wal_checkpoint(PASSIVE)` thread).
- **`sqlite` mode: one read-only connection per actix worker thread**, held in a `thread_local!`
  and opened lazily on first use (`SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX`,
  `PRAGMA query_only=1`, `mmap_size=268435456`, `cache_size=-65536`). WAL mode lets these readers
  run in parallel with the writer, so reads never wait for orders, and each reader sees the latest
  committed data. Use `try_borrow_mut` on the `RefCell` and map a failure to a logged 500, never a
  panic. Use `prepare_cached` everywhere.
- Explain in a comment that SQLite's own page cache and mmap are the database's internal memory,
  just like any database server's, and are not an application cache.

## 4. `POST /products`

1. Validate with `validate.rs` → 400 `VALIDATION_ERROR` on any failure.
2. Build `json_prefix`.
3. Lock the write mutex, open a transaction (`TransactionBehavior::Immediate`):
   - `INSERT INTO products (id, name, category, price, stock, json_prefix) VALUES (…)`
     → primary-key conflict (`SQLITE_CONSTRAINT_PRIMARYKEY`) → roll back → 409 `PRODUCT_EXISTS`;
     nothing else changes.
   - `INSERT INTO categories (category, total) VALUES (?, 1) ON CONFLICT(category) DO UPDATE SET total = total + 1`
   - Commit.
   - `memory` mode only, still holding the mutex: add the product to the in-memory stores.
4. Release the mutex, return 201 with the product JSON. Any other SQLite error → logged 500
   `{"error":"INTERNAL"}`.

## 5. `POST /orders` — the transaction that decides disqualification

1. Validate → 400.
2. Product exists?
   - `sqlite` mode: checked inside the transaction (step 3a below).
   - `memory` mode: look it up in the in-memory store first → not found → 404. This is exact:
     products are never deleted and every creation goes through this process.
3. Lock the write mutex, open a transaction (`TransactionBehavior::Immediate`):
   a. `UPDATE products SET stock = stock - 1 WHERE id = ?1 AND stock > 0 RETURNING stock`
      - No row returned → distinguish the two cases with
        `SELECT 1 FROM products WHERE id = ?1`: no product → roll back → 404 `PRODUCT_NOT_FOUND`;
        product exists → roll back → 409 `SOLD_OUT`.
   b. `INSERT INTO orders (user_id, product_id) VALUES (?1, ?2)`
      - UNIQUE violation (`SQLITE_CONSTRAINT_UNIQUE`) → roll back (this restores the unit taken in
        step a) → 409 `ALREADY_PURCHASED`.
   c. Commit. `order_id = last_insert_rowid()`.
   d. `memory` mode only, still holding the mutex: store the new stock (from `RETURNING`) in the
      product's `AtomicI64`, append the order to the user's in-memory list.
4. Release the mutex, return 201 `{"orderId":"o_<order_id>","userId":…,"productId":…}`
   (strings escaped with serde_json).

Write these explanations as comments in the code:
- **Why UPDATE before INSERT:** a few seconds into each level the flash drop sells out, and then
  ~70% of all orders are for a sold-out product. Checking stock first makes those orders one
  indexed lookup with no write. When a user already bought *and* it's sold out, the spec accepts
  either 409, so this order of checks is legal. The 404 check happens before any 409 is returned,
  as the spec requires (400 → 404 → 409).
- **Why the decision is exact:** both checks run inside one transaction on the only write
  connection; `WHERE stock > 0` and the UNIQUE constraint make overselling and double purchases
  impossible; the `CHECK (stock >= 0)` in the schema is a second safety net.
- **Why errors change nothing:** every error path rolls back before returning.
- **Never** accept or reject an order based on a cached number. In `memory` mode, do NOT add a
  "skip the database if memory says stock is 0" shortcut.
- Never hold the mutex across an `.await` (there are none here; keep it that way).

## 6. Read endpoints

| Endpoint | `sqlite` mode | `memory` mode |
|---|---|---|
| `GET /products/{id}` | `SELECT json_prefix, stock FROM products WHERE id = ?` | Store lookup |
| `GET /products?category=&page=&limit=` | `SELECT total FROM categories WHERE category = ?` + `SELECT json_prefix, stock FROM products WHERE category = ? ORDER BY price, id LIMIT ? OFFSET ?` | Category index lookup + slice |
| `GET /users/{userId}/orders` | `SELECT order_id, product_id FROM orders WHERE user_id = ?` | Per-user list lookup |

Shared rules for both modes:
- Product found → 200 `json_prefix + stock + "}"`; not found → 404 `PRODUCT_NOT_FOUND`.
- List: parse with the task-1 query parser (400 on any invalid value). Unknown category → 200
  `{"items":[],"page":p,"limit":l,"total":0}`. `offset = (page-1) * limit` with **checked or
  saturating arithmetic** (a huge `page` must not overflow or panic); page past the end →
  `items: []` with the real `total`. `page` and `limit` in the response are the parsed values
  (defaults 1 and 20). Sort is `price`, then `id` in byte order (SQLite's default `BINARY`
  collation and Rust's `str` ordering both match the spec's tie-break).
- User orders: 200 `{"userId":…,"orders":[{"orderId":"o_…","productId":…},…]}`; unknown user →
  `"orders":[]`, never 404.
- Build bodies into a `String` with a capacity estimate; use `itoa` for integers.

## 7. In-memory stores (`server/src/state/`, used only in `memory` mode)

- `products.rs`: `dashmap::DashMap<Box<str>, Arc<Product>>`,
  `Product { id, category, price, json_prefix, stock: AtomicI64 }`. Stock is written only while
  the write mutex is held; reads use `Ordering::Relaxed` (explain why that is enough).
- `categories.rs`: `DashMap<Box<str>, parking_lot::RwLock<Vec<Arc<Product>>>>`, each Vec sorted by
  `(price, id)`, inserted with `partition_point`; `total` = the Vec's length. Slice with
  `.iter().skip().take()`, never with indexing.
- `orders.rs`: `DashMap<Box<str>, Vec<OrderEntry>>`, `OrderEntry { order_id: i64, product_id: Arc<str> }`.
- Startup (memory mode only): load all products ordered by `category, price, id` and all orders
  ordered by `order_id`, then set the ready flag. Log counts and load time.
- `/health` is 200 only once the DB is open, migrations are done and (in memory mode) the stores
  are loaded.

## 8. Errors and dependencies

- Keep task-1 `errors.rs`; add `Internal` → 500 `{"error":"INTERNAL"}` that logs the cause with
  `tracing::error!`. A 500 counts as an error in scoring, but a panic or a wrong body is worse.
- Add `dashmap`, `itoa`. Dev-dependencies: `tokio` (macros, rt-multi-thread), `reqwest` with
  `default-features = false, features = ["json"]` (plain HTTP, no TLS), `futures`, `tempfile`.
  Nothing else without telling me why.

## 9. Tests — every test runs in BOTH modes (none may be skipped)

Write a helper that starts the app with a given `READ_SOURCE` and a temp DB, and run each test
for `sqlite` and `memory`.

**`tests/contract.rs`** — reproduce the spec's "sample shop" from section 04 and assert exact
status codes and bodies:
- Pixel (79999, stock 120), Steel Bottle (349, 100000), Phone Case (349, 100000) in one category →
  list page 1 limit 2 returns Bottle then Case (same price → ordered by id); `total` correct.
- Page past the end → `items: []` with the real `total`. Empty category → `total: 0`.
  `limit=80`, no `category`, `page=0`, `page=abc`, `page=1.5`, `limit=0`, `limit=` → 400.
- Create with a taken id → 409, original product unchanged (name, price, stock).
- Create with `price: -5`, `price: "100"`, `price: 10.5`, missing `name` → 400.
- Orders: 201 (stock 120 → 119, response fields correct); missing `productId` → 400; unknown
  product → 404; same user again → 409 `ALREADY_PURCHASED`; sold-out product → 409 `SOLD_OUT`;
  after each error, stock and the user's order list are unchanged.
- User with orders → both listed; user without → `orders: []`.
- Every response has `Content-Type: application/json`. Names with quotes and unicode round-trip
  correctly.

**`tests/concurrency.rs`** — start the **real** server (2 workers) on `127.0.0.1:0` with a temp DB
and fire requests concurrently with `reqwest` on a multi-thread tokio runtime:
- **Last unit:** for stock = 1, 5 and 150, 600 orders from distinct users at once → exactly
  `stock` 201s, the rest `SOLD_OUT`, final stock 0, exactly `stock` orders across all user lists.
- **Double-click:** the same user + product 50× at once, stock 1000 → exactly one 201, final stock
  999, one order in the list.
- **Mixed:** stock 100000, 2000 orders with ~3% repeats and ~2% simultaneous duplicates → final
  stock = 100000 − (number of 201s); no user has two orders for it; every 201's `orderId` is in
  that user's list; no other orders exist.
- **Never negative:** no response ever shows stock < 0.

**`tests/persistence.rs`** — create products and orders, shut the app down, start a new app on the
same DB file (in each mode) → every product, stock, list order and user order list is identical.

**`tests/migration.rs`** — a version-1 DB with products and orders migrates to version 2 with
correct `json_prefix` and `categories` totals.

Also enable the business checks in `deploy/smoke.sh` (last-unit, double-click).

## 10. When done

1. `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test` all pass.
2. Write `docs/DESIGN.md`: section 1, the order flow from section 5 with its "why" comments, the
   data model, the two read modes and how each complies with spec section 09 rule 5, and a table
   mapping each of the spec's 7 rules of the sale to the exact code or schema that guarantees it.
   (I will use this at the debrief.)
3. List anything you were unsure about, and anything in this prompt you think is wrong.