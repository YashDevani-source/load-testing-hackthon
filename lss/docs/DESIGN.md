# lss design

This document explains how the server keeps the books exact under load and why each piece is
built the way it is. It is written for the debrief and for anyone changing the order path.

## 1. The design in one paragraph

**SQLite is the only source of truth.** Every write, and every decision about an order, happens
inside one SQLite transaction on the server's single write connection (`AppState::writer`, behind
a `parking_lot::Mutex`). Reads never take that mutex. In `READ_SOURCE=sqlite` they run on a
read-only connection per actix worker, in parallel with the writer thanks to WAL. In
`READ_SOURCE=memory` they come from in-memory caches that are filled from SQLite at startup and
updated **after** each commit, **while the writer lock is still held**. Nothing is ever decided
from a cached number.

```
POST /products ─┐                               ┌─► COMMIT ─► (memory mode) update caches ─► 201
POST /orders  ──┼─► validate (no lock) ─► lock writer ─► IMMEDIATE tx ─┤
                │                               └─► error: drop tx = ROLLBACK ─► 400/404/409
GET  ...  ──────┴─► sqlite mode: per-worker read-only connection (WAL snapshot of last commit)
                    memory mode: DashMap/RwLock caches (never ahead of the database)
```

### How each read mode meets spec §09 rule 5 ("memory is fine as a cache, but not as the only copy")

| | `sqlite` (default) | `memory` |
|---|---|---|
| Where GETs read | SQLite, on a read-only connection per worker thread (`db/reader.rs`) | `MemoryStores` (`state/memory.rs`) |
| What's in memory | Only SQLite's own page cache and mmap, which are internals of the database. They hold file pages, SQLite invalidates them itself on commit, and they can't return data older than the last commit. | Products (pre-rendered JSON + `AtomicI64` stock), per-category lists sorted by `(price, id)`, per-user order lists |
| Where that data comes from | n/a | `MemoryStores::load` reads every row from SQLite at startup, before the server binds |
| How it stays correct | Each read starts a new WAL snapshot: it sees every committed write, immediately | Updated only after `tx.commit()` succeeds, inside the same writer-lock critical section, in commit order. It is never ahead of the DB and lags it by microseconds (the spec allows 2 s) |
| After a crash or restart | Nothing to rebuild | Rebuilt from SQLite. `tests/persistence.rs` checks the result is byte-identical, and so does a manual kill -9 check |
| Used for order decisions | Never | **Never** |

Both modes render through the same functions in `render.rs`, so the same data gives the same
bytes. `tests/modes_agree.rs` checks this across 120 products, 400 orders and more than 250 GETs.

## 2. The order flow (`routes/orders.rs`)

```text
1. Parse + validate the body                    -> 400 VALIDATION_ERROR (no lock taken)
2. Lock the writer, BEGIN IMMEDIATE
   a) UPDATE products SET stock = stock - 1
        WHERE id = ?1 AND stock > 0 RETURNING stock
      no row -> SELECT EXISTS(SELECT 1 FROM products WHERE id = ?1)
                false -> 404 PRODUCT_NOT_FOUND      (tx dropped = rollback)
                true  -> 409 SOLD_OUT               (tx dropped = rollback)
   b) INSERT INTO orders (user_id, product_id) VALUES (?1, ?2) RETURNING order_id
      UNIQUE violation -> 409 ALREADY_PURCHASED     (rollback also restores the unit from a)
   c) COMMIT
   d) memory mode, still holding the lock: mem.apply_order(product, new_stock, user, order_id)
3. 201 {"orderId":"o_<order_id>","userId":..,"productId":..}
```

The comments in the code give the reasons:

- **UPDATE before INSERT.** A flash drop sells out seconds into each level. After that, about 70%
  of orders are for a sold-out product. Checking stock first turns those into one indexed lookup
  with no write. When a user has already bought a product *and* it's sold out, the spec accepts
  either 409. A 404 is still decided before any 409, as the spec requires (400 → 404 → 409).
- **Exactness.** Both checks run in one transaction on the only write connection, so no other order
  can interleave. `WHERE stock > 0` makes overselling impossible, and the UNIQUE constraint makes a
  second purchase impossible, however simultaneous the requests are. `CHECK (stock >= 0)` is a
  second safety net: even a buggy unconditional decrement would fail instead of committing.
- **Errors change nothing.** Every error path returns while the transaction is still open.
  Dropping an uncommitted `rusqlite::Transaction` rolls it back.
- **Never decide an order from a cached number.** There is no "memory says 0, skip the DB"
  shortcut. A stale 0 would turn buyers away and leave stock in a flash drop (rule 7).

Why a mutex and one connection, rather than letting SQLite arbitrate between several writers?
SQLite allows only one writer at a time anyway. An uncontended `parking_lot` mutex costs
nanoseconds, while SQLite's busy-wait handoff between connections costs microseconds to
milliseconds and adds tail latency. Handlers call SQLite directly, with no `web::block`, because a
commit to the WAL takes microseconds and a thread hop would cost more.

`POST /products` follows the same pattern: `INSERT` (PRIMARY KEY violation → 409, rollback, the
existing product is untouched) → `INSERT … ON CONFLICT DO UPDATE SET total = total + 1` on
`categories` → `COMMIT` → (memory mode) insert into the caches under the lock.

### Durability

WAL with `synchronous=NORMAL`. A commit is an append to the WAL in the OS page cache, so a
process crash (`kill -9`, panic) loses nothing that got a 201: SQLite replays the WAL on the next
open. A *power loss* could lose commits made since the last WAL fsync, which happens at each
background checkpoint (every 5 s). The contest tests process crashes and reboots, not power cuts.
`synchronous=FULL` would close that gap at the cost of one fsync per order.

## 3. Data model (schema version 2)

```sql
CREATE TABLE products (
  id          TEXT    PRIMARY KEY,
  name        TEXT    NOT NULL,
  category    TEXT    NOT NULL,
  price       INTEGER NOT NULL CHECK (price >= 0),
  stock       INTEGER NOT NULL CHECK (stock >= 0),
  json_prefix TEXT    NOT NULL DEFAULT ''   -- {"id":..,"name":..,"category":..,"price":..,"stock":
);
CREATE INDEX products_category_price_id ON products (category, price, id);

CREATE TABLE orders (
  order_id   INTEGER PRIMARY KEY,           -- rowid: unique, never reused; rendered "o_<id>"
  user_id    TEXT    NOT NULL,
  product_id TEXT    NOT NULL,
  UNIQUE (user_id, product_id)              -- also serves "orders of one user"
);

CREATE TABLE categories (
  category TEXT PRIMARY KEY,
  total    INTEGER NOT NULL                 -- = COUNT(*) of products in the category
) WITHOUT ROWID;
```

- **`json_prefix`.** Everything about a product except stock never changes, so it is serialised
  once, by `render::build_json_prefix` (escaping done by `serde_json`), and stored. Every product
  response is `json_prefix + stock + "}"`, with no serialisation per request.
- **`categories.total`.** It is updated in the same transaction as each product insert, so a list
  page doesn't need `COUNT(*)`. A page past the end skips the item query entirely.
- **List order.** `ORDER BY price, id` uses SQLite's BINARY collation (byte order), which is the
  spec's tie-break. The memory store sorts by `(price, &str)`, and Rust also compares `str` by bytes.
- **Migrations** (`db/migrate.rs`). `PRAGMA user_version` tracks the version. All pending steps and
  the version bump run in one IMMEDIATE transaction. A task-1 database has `user_version = 0` but
  already has the tables; step 0→1 only uses `IF NOT EXISTS`, so it is a no-op there and the
  database then goes through 1→2. The 1→2 step backfills `json_prefix` in Rust with the same
  function `POST /products` uses. A database from a newer binary is refused rather than modified.

## 4. The seven rules of the sale → what guarantees them

| # | Rule (spec §05) | Guaranteed by | Tested by |
|---|---|---|---|
| 1 | **No overselling.** Successful orders ≤ starting stock | `UPDATE … WHERE id = ?1 AND stock > 0` in one IMMEDIATE transaction on the only write connection, under `AppState::writer` (`routes/orders.rs`). Backstop: `CHECK (stock >= 0)` (`db/schema.sql`) | `concurrency.rs::last_unit` (stock 1/5/150, 600 buyers at once → exactly that many 201s), smoke last-unit |
| 2 | **Stock adds up.** Final = start − successful orders | The decrement (a) and the order insert (b) commit together or not at all. If the INSERT fails, dropping `tx` rolls back the decrement. Stock is never changed anywhere else | `concurrency.rs::mixed` (final stock = 100000 − #201), `last_unit` (ends at 0, listed orders = stock), `persistence.rs` |
| 3 | **Never negative.** No response shows stock < 0 | `CHECK (stock >= 0)` in the database. Reads show either the DB value or, in memory mode, the value from `RETURNING stock` of a committed update, which is ≥ 0 | `concurrency.rs::watch_stock` reads the product and its list throughout every race and asserts 0 ≤ stock ≤ start |
| 4 | **One per customer.** One 201 per (user, product), even when requests are simultaneous | `UNIQUE (user_id, product_id)` on `orders`. The duplicate insert fails, `is_unique_violation` → 409 `ALREADY_PURCHASED`, and the rollback returns the unit | `concurrency.rs::double_click` (50× at once → one 201), `mixed` (2% double-clicks, 3% retries), smoke double-click |
| 5 | **No lost orders.** Every 201 appears in the user's list | 201 is sent only after `tx.commit()`. The order is in the WAL, so it survives kill -9. In memory mode it is appended to the user's list under the writer lock before the response is built | `concurrency.rs::mixed` (every 201 orderId is listed), `persistence.rs`, `deploy/crash-test.sh`, manual kill -9 check |
| 6 | **No ghost orders.** Every listed order was accepted | An order row exists only in a committed transaction that also took a unit. Errors roll back. The memory list is appended only after commit | `contract.rs::orders_flow` (snapshot of products and lists is unchanged after every error), `concurrency.rs::mixed` (lists contain exactly the 201s) |
| 7 | **Sell-through.** Every flash drop ends at stock 0 | Orders always decide on the database's current stock, never on a cache. A failed order never keeps a unit (rollback). No timeouts inside the transaction can leave a unit taken | `concurrency.rs::last_unit` (final stock 0 after the race) |

Caching (spec "2-second rule"): `name`/`category`/`price` are fixed in `json_prefix`. Stock in
reads and user order lists is current in sqlite mode and at most microseconds behind in memory
mode. The stock check in `POST /orders` always reads SQLite inside the transaction.

## 5. What happens when things go wrong

| Event | Behaviour |
|---|---|
| Bad input | Validated before the lock (`validate.rs`): 400 `VALIDATION_ERROR`, nothing touched |
| Unexpected SQLite error | `ApiError::Internal` → 500 `{"error":"INTERNAL"}`, logged once in `error_response`, transaction rolled back |
| Process crash / `kill -9` | systemd restarts after 200 ms (`Restart=always`, `StartLimitIntervalSec=0`). SQLite replays the WAL. Memory mode reloads its caches. `/health` is 200 once the server is listening, because everything is loaded before the bind |
| Graceful stop (SIGTERM) | actix drains in-flight requests (≤ 5 s), then the checkpoint thread stops and a final `wal_checkpoint(TRUNCATE)` runs |
| Panic in a handler | Prevented by clippy lints (`unwrap`/`expect`/`panic`/indexing denied). `panic = "unwind"` limits any that slip through to one worker task |
