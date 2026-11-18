# Task 2 of 4 — Core logic (detailed spec): order transaction, all 6 endpoints, two read modes

You are continuing the Rust project scaffolded in task 1 (actix-web 4, SQLite in-process via
`rusqlite` with `bundled`, one process, systemd). The attached `last-server-standing-rules.html`
(Rules v1.1) is the source of truth for behaviour.

**This prompt is deliberately exact so you can implement it directly.** Every SQL statement, type,
function signature and test is specified. Do not redesign anything, do not ask questions, do not
add features. If something in the existing task-1 code has a different name, adapt to it with the
smallest change and note it in your final report.

---

## 0. Ground rules (unchanged from task 1 — keep them)

- No typed actix extractors for input (`web::Json`, `web::Query`, typed `web::Path`).
- No `unwrap` / `expect` / `panic!` / slice indexing in non-test code. The clippy deny lints stay on.
- No per-request logging. `tracing` only for startup, shutdown, errors.
- SQLite is called **directly** in handlers. Never `web::block` / `spawn_blocking`.
- Never hold a mutex guard across an `.await`. (There are no awaits after reading the body.)
- Every response has `Content-Type: application/json`.

## 1. Work in this order, and run `cargo test` after each step

1. `lib.rs` refactor + test harness (section 3)
2. Schema migration to version 2 (section 4)
3. Error type + response helpers (section 5)
4. Write path: `POST /products`, `POST /orders` (sections 7, 8)
5. Read path in `sqlite` mode (section 9)
6. Contract, concurrency, persistence, migration tests passing in `sqlite` mode (section 12)
7. `memory` mode (section 10), then run every test in both modes
8. `smoke.sh` business checks, `docs/DESIGN.md`, final report (sections 13, 14)

**Out of scope for this task:** load testing, performance tuning beyond what is written here, new
endpoints, middleware, auth, metrics, Docker.

## 2. Behaviour summary (from the spec)

| Route | Success | Errors |
|---|---|---|
| `GET /health` | 200 `{"status":"ok"}` once ready | 503 `{"status":"starting"}` |
| `POST /products` | 201 + product as stored | 400 `VALIDATION_ERROR`, 409 `PRODUCT_EXISTS` (existing product unchanged) |
| `GET /products/{id}` | 200 + product | 404 `PRODUCT_NOT_FOUND` |
| `GET /products?category=&page=&limit=` | 200 `{"items":[…],"page":p,"limit":l,"total":t}` | 400 `VALIDATION_ERROR` |
| `POST /orders` | 201 `{"orderId","userId","productId"}`, stock −1 | 400 → 404 → 409 (`ALREADY_PURCHASED` / `SOLD_OUT`); any error changes nothing |
| `GET /users/{userId}/orders` | 200 `{"userId":…,"orders":[{"orderId","productId"}]}`; unknown user → `[]` | — |

List: sort by `price` asc, then `id` in byte order; page past end → `items: []` with real `total`;
unknown category → `items: []`, `total: 0`; `page` default 1, `limit` default 20 (1–50).

## 3. `lib.rs` refactor + test harness

Move everything except the `#[global_allocator]` and `fn main` into `server/src/lib.rs` so tests
can start the real server.

```rust
// server/src/lib.rs
pub struct RunningServer {
    pub addr: std::net::SocketAddr,
    pub handle: actix_web::dev::ServerHandle,
    pub server: actix_web::dev::Server,   // the future to run
}

/// Opens the DB, runs migrations, loads memory stores if READ_SOURCE=memory,
/// sets the ready flag, binds, and returns the server without running it.
pub fn build_server(config: Config) -> anyhow::Result<RunningServer>;
```

- `main.rs`: parse `Config` from env, `build_server`, log the address and `READ_SOURCE`, then
  `server.await` inside `#[actix_web::main]`.
- `Config` gains `read_source: ReadSource` from env `READ_SOURCE` (`sqlite` default | `memory`;
  anything else → startup error).
- HttpServer settings: `.workers(config.workers)`, `.backlog(65535)`,
  `.max_connections(25_000)`, `.keep_alive(Duration::from_secs(75))`,
  `.client_request_timeout(Duration::from_secs(5))`, `.shutdown_timeout(5)`.

Test harness in `server/tests/common/mod.rs`:

```rust
pub struct TestServer { pub base: String, handle: ServerHandle, _dir: tempfile::TempDir, thread: Option<std::thread::JoinHandle<()>> }
impl TestServer {
    /// Starts the real server on 127.0.0.1:0 with 2 workers and a fresh temp DB.
    pub fn start(mode: ReadSource) -> TestServer;
    /// Same, but reuses an existing DB directory (for persistence tests).
    pub fn start_with_dir(mode: ReadSource, dir: tempfile::TempDir) -> TestServer;
    /// Stops the server gracefully and returns the DB directory.
    pub fn stop(self) -> tempfile::TempDir;
}
```

`start` spawns a `std::thread` that creates an actix system
(`actix_web::rt::System::new().block_on(async { … })`), calls `build_server` **inside** that
system, sends `(addr, handle)` back over a `std::sync::mpsc` channel, then awaits the server.
`start` then waits until `GET /health` returns 200. Tests use `reqwest` on
`#[tokio::test(flavor = "multi_thread", worker_threads = 4)]`.

A macro or helper runs each test body for both modes, e.g.
`both_modes!(test_name, |srv: &TestServer| async { … })` producing `test_name_sqlite` and
`test_name_memory`. Until step 7 the memory variant may be `#[ignore]`; at the end **nothing** is
ignored.

## 4. Schema version 2 (`PRAGMA user_version`)

`server/src/db/migrate.rs`:

```rust
pub fn migrate(conn: &mut rusqlite::Connection) -> anyhow::Result<()>;
```

Read `PRAGMA user_version`. Apply steps in one `Immediate` transaction, then set
`user_version = 2`.

- **0 → 1** (fresh DB): the task-1 schema (`products`, index, `orders`).
- **1 → 2:**
```sql
ALTER TABLE products ADD COLUMN json_prefix TEXT NOT NULL DEFAULT '';
CREATE TABLE IF NOT EXISTS categories (
  category TEXT PRIMARY KEY,
  total    INTEGER NOT NULL
) WITHOUT ROWID;
INSERT INTO categories (category, total)
  SELECT category, COUNT(*) FROM products GROUP BY category;
```
  (The table was just created inside the same transaction, so a plain INSERT is enough.)
  Then, in Rust, backfill `json_prefix` for every row where it is `''`
  (`SELECT id, name, category, price FROM products WHERE json_prefix = ''`, then
  `UPDATE products SET json_prefix = ?2 WHERE id = ?1`).

`json_prefix` is built by **one** function, used everywhere:

```rust
/// {"id":<id>,"name":<name>,"category":<category>,"price":<price>,"stock":
pub fn build_json_prefix(id: &str, name: &str, category: &str, price: i64) -> String;
```

Strings are escaped with `serde_json::to_string(&str)` (never by hand). A product response is
always `json_prefix + itoa(stock) + "}"`.

## 5. Errors and responses

`server/src/errors.rs`:

```rust
#[derive(Debug)]
pub enum ApiError {
    Validation,        // 400 VALIDATION_ERROR
    ProductNotFound,   // 404 PRODUCT_NOT_FOUND
    ProductExists,     // 409 PRODUCT_EXISTS
    AlreadyPurchased,  // 409 ALREADY_PURCHASED
    SoldOut,           // 409 SOLD_OUT
    RouteNotFound,     // 404 NOT_FOUND (unknown routes)
    Internal(String),  // 500 INTERNAL — logged with tracing::error!
}
impl actix_web::ResponseError for ApiError { /* status_code + error_response with {"error":"CODE"} */ }
impl From<rusqlite::Error> for ApiError { /* → Internal(e.to_string()) */ }
```

Handlers return `Result<HttpResponse, ApiError>`.

`server/src/http/respond.rs`:

```rust
pub fn json(status: StatusCode, body: String) -> HttpResponse; // sets Content-Type: application/json
pub fn push_json_str(out: &mut String, s: &str);                 // appends s as an escaped JSON string
```

SQLite constraint helpers (`server/src/db/errors.rs`):

```rust
pub fn is_primary_key_violation(e: &rusqlite::Error) -> bool; // extended code SQLITE_CONSTRAINT_PRIMARYKEY
pub fn is_unique_violation(e: &rusqlite::Error) -> bool;      // extended code SQLITE_CONSTRAINT_UNIQUE
```

Match `rusqlite::Error::SqliteFailure(err, _)` and compare `err.extended_code` with
`rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY` / `rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE`.

**Body reading:** if task 1 used the `web::Bytes` extractor, check that an oversized body returns
our 400 JSON (actix's default is a plain-text 413). If not, read `web::Payload` manually with a
16 KB limit in a helper `read_body(payload) -> Result<Bytes, ApiError>`.

## 6. App state and connections

```rust
pub struct AppState {
    pub writer: parking_lot::Mutex<rusqlite::Connection>,
    pub db_path: std::path::PathBuf,
    pub read_source: ReadSource,
    pub ready: std::sync::atomic::AtomicBool,
    pub mem: Option<MemoryStores>,   // Some only in memory mode
}
```

Shared with `web::Data<AppState>`.

**Read connections (sqlite mode)** — `server/src/db/reader.rs`:

```rust
thread_local! {
    static READER: std::cell::RefCell<Option<rusqlite::Connection>> = const { std::cell::RefCell::new(None) };
}
/// Runs `f` with this worker thread's read-only connection, opening it on first use.
pub fn with_reader<T>(db_path: &Path, f: impl FnOnce(&rusqlite::Connection) -> Result<T, ApiError>) -> Result<T, ApiError>;
```

Open with `OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX`, then
`PRAGMA query_only = 1; PRAGMA mmap_size = 268435456; PRAGMA cache_size = -65536;`.
Use `try_borrow_mut` and map a failure to `ApiError::Internal`. Add a comment: WAL lets these
readers run in parallel with the writer and see the latest committed data; SQLite's page cache and
mmap are the database's own internals, not an application cache.

All statements everywhere use `prepare_cached`.

## 7. `POST /products` — `server/src/routes/products.rs`

```rust
pub async fn create_product(state: web::Data<AppState>, body: web::Bytes /* or Payload */) -> Result<HttpResponse, ApiError>;
```

1. Parse JSON → `validate::product(&Value) -> Result<NewProduct, ApiError>` (task-1 function;
   adapt the name). `NewProduct { id, name, category, price: i64, stock: i64 }`.
2. `prefix = build_json_prefix(...)`.
3. Lock `state.writer`, `let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;`
```sql
INSERT INTO products (id, name, category, price, stock, json_prefix) VALUES (?1, ?2, ?3, ?4, ?5, ?6);
```
   - `is_primary_key_violation` → return `ApiError::ProductExists` (dropping `tx` rolls back).
```sql
INSERT INTO categories (category, total) VALUES (?1, 1)
  ON CONFLICT(category) DO UPDATE SET total = total + 1;
```
   - `tx.commit()?`
   - Memory mode, **still holding the lock**: `mem.insert_product(...)`.
4. Return 201 with `prefix + stock + "}"`.

## 8. `POST /orders` — `server/src/routes/orders.rs`

```rust
pub async fn create_order(state: web::Data<AppState>, body: web::Bytes) -> Result<HttpResponse, ApiError>;
```

1. Parse JSON → `validate::order(&Value) -> Result<NewOrder, ApiError>`;
   `NewOrder { user_id, product_id }`.
2. Lock `state.writer`, open an `Immediate` transaction, then:

```sql
-- a) take one unit, only if one is left
UPDATE products SET stock = stock - 1 WHERE id = ?1 AND stock > 0 RETURNING stock;
```
   Use `.query_row(...).optional()?` (`rusqlite::OptionalExtension`).
   - `None` → run `SELECT EXISTS(SELECT 1 FROM products WHERE id = ?1)`:
     `false` → `ApiError::ProductNotFound`; `true` → `ApiError::SoldOut`. (Drop `tx` → rollback.)

```sql
-- b) record the order; the UNIQUE(user_id, product_id) makes a 2nd purchase impossible
INSERT INTO orders (user_id, product_id) VALUES (?1, ?2) RETURNING order_id;
```
   - `is_unique_violation` → `ApiError::AlreadyPurchased`. Dropping `tx` rolls back, which also
     restores the unit taken in (a).

   c) `tx.commit()?`.
   d) Memory mode, **still holding the lock**: `mem.apply_order(product_id, new_stock, user_id, order_id)`.

3. Return 201 `{"orderId":"o_<order_id>","userId":<escaped>,"productId":<escaped>}`.

Put these comments in the code:
- **UPDATE before INSERT:** once a flash drop sells out (seconds into each level), ~70% of orders are
  for a sold-out product; checking stock first makes them one indexed lookup with no write. When a
  user already bought *and* it's sold out, the spec accepts either 409. 404 is still decided before
  any 409, as required (400 → 404 → 409).
- **Exactness:** both checks run in one transaction on the only write connection; `WHERE stock > 0`
  and the UNIQUE constraint make overselling and double purchases impossible; `CHECK (stock >= 0)`
  is a second safety net.
- **Errors change nothing:** every error path drops the transaction before returning.
- **Never** decide an order from a cached number. No "memory says 0, skip the DB" shortcut, ever.

## 9. Read endpoints, `sqlite` mode

All through `with_reader`.

**`GET /products/{id}`** — `req.match_info().get("id")`:
```sql
SELECT json_prefix, stock FROM products WHERE id = ?1;
```
Found → 200 `prefix + stock + "}"`; none → 404.

**`GET /products?…`** — `query::list_params(req.query_string()) -> Result<ListParams, ApiError>`
(task-1 parser; `ListParams { category, page: u64, limit: u64 }`).
```sql
SELECT total FROM categories WHERE category = ?1;
```
- No row → 200 `{"items":[],"page":p,"limit":l,"total":0}`.
- `offset = (page - 1).saturating_mul(limit)`; if `offset >= total` → `items: []` (skip the query).
- Otherwise:
```sql
SELECT json_prefix, stock FROM products WHERE category = ?1 ORDER BY price, id LIMIT ?2 OFFSET ?3;
```
  Bind `limit` and `offset` as `i64` (clamp `offset` to `i64::MAX`).
- Body: `{"items":[` + items joined by `,` + `],"page":p,"limit":l,"total":t}`.

**`GET /users/{userId}/orders`**:
```sql
SELECT order_id, product_id FROM orders WHERE user_id = ?1 ORDER BY order_id;
```
Body: `{"userId":<escaped>,"orders":[{"orderId":"o_<id>","productId":<escaped>},…]}`; none →
`"orders":[]`.

Build bodies in a `String::with_capacity(…)`; integers with `itoa::Buffer`.

## 10. `memory` mode — `server/src/state/`

```rust
pub struct MemoryStores {
    pub products:   dashmap::DashMap<Box<str>, Arc<MemProduct>>,
    pub categories: dashmap::DashMap<Box<str>, parking_lot::RwLock<Vec<Arc<MemProduct>>>>,
    pub orders:     dashmap::DashMap<Box<str>, Vec<MemOrder>>,
}
pub struct MemProduct { pub id: Box<str>, pub price: i64, pub json_prefix: Box<str>, pub stock: std::sync::atomic::AtomicI64 }
pub struct MemOrder   { pub order_id: i64, pub product_id: Arc<str> }

impl MemoryStores {
    pub fn load(conn: &rusqlite::Connection) -> anyhow::Result<Self>;  // startup
    pub fn insert_product(&self, id: &str, category: &str, price: i64, stock: i64, json_prefix: String);
    pub fn apply_order(&self, product_id: &str, new_stock: i64, user_id: &str, order_id: i64);
    pub fn get_product_json(&self, id: &str) -> Option<String>;
    pub fn list_json(&self, category: &str, page: u64, limit: u64) -> String;
    pub fn user_orders_json(&self, user_id: &str) -> String;
}
```

- `load`: `SELECT id, category, price, stock, json_prefix FROM products ORDER BY category, price, id`
  and `SELECT order_id, user_id, product_id FROM orders ORDER BY order_id`. Log counts + time.
- Category Vecs are sorted by `(price, id)`; insert with `partition_point`. Rust `str` order is byte
  order, the same as SQLite's `BINARY` collation and the spec's tie-break.
- Stock: `store`/`load` with `Ordering::Relaxed`. Comment why: it is only written while the writer
  mutex is held, and readers may see a value a moment old, which the spec allows (≤ 2 s).
- Slicing: `.iter().skip(offset).take(limit)`, never indexing; offset with `saturating_mul`.
- Do not hold a DashMap guard while doing anything slow; clone the `Arc` and drop the guard.
- The memory read functions produce exactly the same bytes as the sqlite-mode functions for the
  same data (a test checks this, section 12).

`/health` sets `ready = true` only after migrations (and, in memory mode, `load`) succeed.

## 11. Dependencies

Add: `dashmap`, `itoa`, `anyhow` (if missing). Dev: `tokio` (macros, rt-multi-thread),
`reqwest` (`default-features = false`, `features = ["json"]`), `futures`, `tempfile`, `serde_json`.
Nothing else.

## 12. Tests — every test runs in both modes; none ignored at the end

**`tests/contract.rs`** — the spec's sample shop (section 04):
- Create Pixel (`p3f9a0c2e71bd`, 79999, 120), Steel Bottle (`p0c4be91a3f25`, 349, 100000),
  Phone Case (`p7d2e05b1c9aa`, 349, 100000) in `c8d21fa` → 201 bodies equal the input.
- `GET /products?category=c8d21fa&page=1&limit=2` → Bottle then Case; `total` = 3.
- `page=20` → `items: []`, `total: 3`. `category=c000000` → `total: 0`.
- 400 for: `limit=80`, no `category`, `page=0`, `page=abc`, `page=1.5`, `limit=0`, `limit=`.
- Re-create Pixel with different name/price/stock → 409; Pixel unchanged.
- Create with `price: -5`, `price: "100"`, `price: 10.5`, `stock: null`, missing `name`,
  body `[]`, body `not json` → 400 `{"error":"VALIDATION_ERROR"}`.
- Orders: `u7b0e44c19a2f` buys Pixel → 201, fields correct, Pixel stock 119; missing `productId` →
  400; `productId: 123` → 400; unknown product → 404; same order again → 409 `ALREADY_PURCHASED`;
  a product with stock 1 bought once, then by another user → 409 `SOLD_OUT`; after every error,
  stock and both users' order lists are unchanged.
- `GET /users/u7b0e44c19a2f/orders` lists the order; `GET /users/u3c9a1f08d7e2/orders` →
  `"orders":[]`.
- Unknown route → 404 JSON. Every response has `Content-Type: application/json`.
- A name with `"`, `\`, a newline and unicode round-trips exactly.

**`tests/concurrency.rs`** — real server, 2 workers. Fire requests with
`futures::stream::iter(…).buffer_unordered(200)` (200 in flight at once is plenty to expose races,
and stays under macOS's default limit of 256 open files on a laptop):
- Last unit: stock ∈ {1, 5, 150}; 600 distinct users at once → exactly `stock` × 201, rest 409
  `SOLD_OUT`, final stock 0, sum of matching orders across those users' lists = `stock`.
- Double-click: same user + product 50× at once, stock 1000 → exactly one 201, stock 999, one order.
- Mixed: stock 100000, 2000 orders (3% repeats of earlier winners, 2% sent twice at once) →
  final stock = 100000 − count(201); no user has 2 orders for it; every 201 `orderId` is in its
  user's list; the lists contain nothing else.
- Every product body seen during these tests has `stock >= 0`.

**`tests/persistence.rs`** — create data, `stop()`, `start_with_dir` on the same DB (same mode) →
products, stock, list pages and user order lists are byte-identical.

**`tests/migration.rs`** — build a version-1 DB by hand (task-1 schema, a few products and orders,
`user_version = 1`), start the server → `user_version = 2`, `json_prefix` correct, `categories`
totals correct, all endpoints answer correctly.

**`tests/modes_agree.rs`** — run the same sequence of creates and orders against a sqlite-mode and
a memory-mode server; every GET response must be byte-identical between the two.

## 13. Deployment bits

- `deploy/lss.service`: add `READ_SOURCE=sqlite` to `Environment=`.
- `deploy/smoke.sh`: enable the last-unit and double-click checks.
- `Makefile`: `test` runs everything.

## 14. When done

1. `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test` all pass, nothing
   `#[ignore]`d.
2. Write `docs/DESIGN.md`:
   - the design: SQLite is the only source of truth; every order decided in a transaction; two
     read modes and how each satisfies spec section 09 rule 5 ("memory is fine as a cache, but not
     as the only copy");
   - the order flow from section 8 with its "why" comments;
   - the data model (schema v2);
   - a table mapping each of the spec's **7 rules of the sale** to the exact code/schema that
     guarantees it.
3. Final report: every name you changed from this prompt, anything you were unsure about, and
   anything here you believe is wrong.