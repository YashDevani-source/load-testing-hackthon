# Task 1 of 4 — Project structure & scaffold: "Last Server Standing" (Rust)

You are setting up the repository for a load-testing contest entry. The attached
`last-server-standing-rules.html` (Rules v1.1, 18 Nov 2026) is the official spec and the **source of
truth**. The key parts are summarised below so nothing gets missed; if anything here conflicts with
the HTML, the HTML wins and you must tell me about the conflict.

**Scope of this task:** build the **folder structure, tooling, database setup, validation, `/health`,
systemd deployment, scripts and CI/CD**. Business logic comes in task 2. Wire up every other route
with typed stubs marked `TODO(task-2)` that return 501 `{"error":"NOT_IMPLEMENTED"}`.
Fully working in this task: `/health`, `validate.rs`, the database layer, error rendering, query
parsing, all scripts, CI/CD.

---

## 1. Context

- **Machine:** AWS m7i.large. 2 vCPU = **one physical core with 2 hyperthreads**, 7.6 GiB RAM,
  30 GB gp3 disk. Box runs Ubuntu 26.04 (kernel 7.0, glibc 2.43); the spec says finals use
  Ubuntu 24.04, so the binary must run on both.
- **Goal:** pass 10,000 req/s (level 10) with p99 ≤ 100 ms, ≤ 1% errors, **zero wrong answers**.
  Stretch goal: 50,000 req/s (level 17).
- **Network:** server listens on **0.0.0.0:8080**, plain HTTP/1.1 keep-alive, up to 5,000 open
  connections from one IP. No answer within 2 s counts as an error.
- **Code freeze:** SSH and internet are cut. The service must restart itself after a crash
  (`kill -9`) and after a reboot, with **no network access** and no human help.
- **Persistence:** data must live in a database that writes to disk. Memory may only be a cache.
- **Who builds this:** the developer uses AI tools and must explain every design decision at the
  debrief. Keep code plain and well commented. Comments explain *why*, not *what*.

## 2. API contract (summary of spec section 03–04)

General rules:
- JSON in, JSON out. **Every** response has `Content-Type: application/json`.
- Routes start at `/` (no `/api` prefix). No auth.
- Errors: correct status + `{"error":"CODE"}`. Only these five codes exist:

| Status | `error` | When |
|---|---|---|
| 400 | `VALIDATION_ERROR` | Required field/query param missing, empty or wrong type |
| 404 | `PRODUCT_NOT_FOUND` | No product with that id |
| 409 | `PRODUCT_EXISTS` | `POST /products` with an id that's taken |
| 409 | `ALREADY_PURCHASED` | User already bought this product |
| 409 | `SOLD_OUT` | Product stock is 0 |

- Extra fields in request bodies are **ignored**. Missing fields are errors.
- Product object: `{"id": string 1–64, "name": string 1–200, "category": string 1–64,
  "price": integer ≥ 0, "stock": integer ≥ 0}`. Only `stock` ever changes, and only via orders.
  Nothing is ever deleted.

Endpoints:

| Route | Success | Errors / rules |
|---|---|---|
| `GET /health` | 200 `{"status":"ok"}` only when fully ready (DB open, state loaded) | Not ready → 503 `{"status":"starting"}` |
| `POST /products` | 201 + the product as stored | 400 bad input (incl. negative or non-integer price/stock); 409 `PRODUCT_EXISTS` and the existing product stays unchanged |
| `GET /products/{id}` | 200 + product (stock may be ≤ 2 s old) | 404 `PRODUCT_NOT_FOUND` |
| `GET /products?category=&page=&limit=` | 200 `{"items":[…],"page":n,"limit":n,"total":n}` | `category` required, non-empty; `page` default 1, integer ≥ 1; `limit` default 20, integer 1–50; else 400 |
| `POST /orders` body `{"userId","productId"}` | 201 `{"orderId","userId","productId"}`; stock goes down by exactly 1 | Check order: **400 → 404 → 409**. If already bought *and* sold out, either 409 is fine. **Any error changes nothing.** |
| `GET /users/{userId}/orders` | 200 `{"userId":…,"orders":[{"orderId","productId"},…]}` in any order | Unknown user → 200 with `"orders":[]` (never 404). May be ≤ 2 s old |

List rules: sort by `price` ascending, ties by `id` in plain byte order. `total` = all products in
the category. Page past the end → 200 with `items: []` and the real `total`. Unknown category → 200
with `items: []`, `total: 0`. `orderId` is any non-empty string, unique across all orders.

## 3. Rules of the sale (spec section 05 — breaking any one = disqualification)

1. No overselling. 2. Final stock = starting stock − successful orders, exactly.
3. Stock never shown below 0. 4. One 201 per (user, product), even for simultaneous duplicates.
5. Every 201 appears in that user's order list. 6. No order appears that wasn't accepted.
7. Every flash drop ends at stock 0 (never reject based on a cached number).

Caching: `name`/`category`/`price` forever; `stock` in reads and user order lists ≤ 2 s old;
**the stock check inside `POST /orders` must be exact.**

---

## 4. Fixed architecture (do not substitute anything)

- **Rust stable**, pinned in `rust-toolchain.toml` to `1.100.0`. Edition 2024. Commit `Cargo.lock`.
- **HTTP: actix-web 4.** One process, `WORKERS` actix workers (env, default 2).
- **Database: SQLite inside the process** via `rusqlite` with the `bundled` feature.
  - Pragmas at open: `journal_mode=WAL`, `synchronous=NORMAL`, `wal_autocheckpoint=0`,
    `temp_store=MEMORY`, `busy_timeout=5000`.
  - A background thread runs `PRAGMA wal_checkpoint(PASSIVE)` every 5 s, so no request ever pays
    for a checkpoint; a final `wal_checkpoint(TRUNCATE)` runs on graceful shutdown.
  - Exactly **one connection**, behind a `parking_lot::Mutex`; statements via `prepare_cached`.
  - Call SQLite **directly** in handlers. **Never** wrap it in `web::block` / `spawn_blocking`:
    it is in-process and takes microseconds; a thread hop would cost more than the query.
  - DB path from env `DATABASE_PATH` (default `./data/lss.db`; create the directory).
- **Schema** (`server/src/db/schema.sql`, applied idempotently at startup):
  - `products(id TEXT PRIMARY KEY, name TEXT NOT NULL, category TEXT NOT NULL,
     price INTEGER NOT NULL CHECK (price >= 0), stock INTEGER NOT NULL CHECK (stock >= 0))`
  - index on `products(category, price, id)`
  - `orders(order_id INTEGER PRIMARY KEY, user_id TEXT NOT NULL, product_id TEXT NOT NULL,
     UNIQUE (user_id, product_id))`
  - The CHECK on stock is a deliberate safety net: the database itself refuses negative stock.
    The UNIQUE constraint is what makes a duplicate purchase impossible.
- **In-memory state** (task 2 serves all reads from memory): create the `state/` modules with
  struct definitions and doc comments only. Do not implement them yet.
- **Allocator:** mimalloc as the global allocator.
- **Logging:** `tracing` + `tracing-subscriber` for startup, shutdown and errors only.
  **No per-request logging. Do not add actix's `Logger` middleware.**
- **No Redis, no MongoDB, no Docker, no nginx.**

## 5. HTTP rules (where AI-written actix code usually breaks the spec)

- **Do not use `web::Json<T>`, `web::Query<T>` or `web::Path<T>` with typed structs for input.**
  Their error responses are plain text and don't match the spec. Read bodies as `web::Bytes` and
  parse with `serde_json`; parse the query string by hand with `form_urlencoded`; read path params
  as plain strings.
- One `errors.rs` enum renders every error as `{"error":"CODE"}` with the right status.
- Body size limit 16 KB (`PayloadConfig`). Oversized, invalid or empty JSON body → 400
  `VALIDATION_ERROR`.
- `default_service` → 404 `{"error":"NOT_FOUND"}` for unknown routes.
- Keep-alive 75 s, listen backlog 65535, `max_connections` per worker ≥ 10,000.
- `/health` reads an `AtomicBool` set only after the DB is open and state is loaded.

## 6. Validation module — implement fully now: `server/src/validate.rs`

Pure functions, no I/O. Check `serde_json::Value` explicitly instead of typed deserialization, so
every wrong type is caught:
- `POST /products`: `id`, `category` are strings of 1–64 characters; `name` is a string of 1–200
  characters; `price` and `stock` satisfy `Value::is_u64()` and are ≤ `i64::MAX`. This rejects
  `"100"` (string), `10.5`, `-5`, `true`, `null`. Missing field → error. Extra fields → ignored
  (do **not** use `deny_unknown_fields`). Count characters, not bytes. Body that isn't a JSON
  object → error.
- `POST /orders`: `userId`, `productId` are non-empty strings; anything else → error.
- List query: `category` required, non-empty; `page` optional (default 1), digits only, ≥ 1;
  `limit` optional (default 20), digits only, 1–50. Reject empty values, signs, decimals, letters,
  leading `+`, values that overflow. If a parameter repeats, the first one wins.
- Unit tests for every rule above, including every rejected type.

## 7. Lints that make the compiler enforce safety

In the server crate root:
`#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing, clippy::todo, clippy::unimplemented)]`
Plus `clippy.toml`: `allow-unwrap-in-tests = true`, `allow-expect-in-tests = true`.
Request paths must never panic: every failure becomes a spec error or a logged 500.

## 8. Build settings

- `[profile.release]`: `opt-level = 3`, `lto = "fat"`, `codegen-units = 1`, `panic = "unwind"`,
  `debug = "line-tables-only"` (keeps symbols for `perf` profiling).
- `.cargo/config.toml`: `-C target-cpu=x86-64-v3` for `x86_64-unknown-linux-gnu` only, so builds
  on a Mac are unaffected.

## 9. Folder structure — create exactly this (explain any deviation)

```
lss/
├── Cargo.toml                    # workspace: server, tools
├── Cargo.lock
├── rust-toolchain.toml           # pins 1.100.0
├── clippy.toml
├── .cargo/config.toml            # target-cpu for Linux builds only
├── .gitignore                    # target/, *.pem, *.db, *.db-wal, *.db-shm, data/, .env
├── Makefile                      # fmt, lint, test, build, run, smoke, deploy
├── README.md
│
├── server/                       # the API server binary: lss-server
│   ├── Cargo.toml
│   ├── src/
│   │   ├── main.rs               # mimalloc, tracing, config, open DB, load state, start actix, shutdown
│   │   ├── config.rs             # HOST, PORT, WORKERS, DATABASE_PATH, LOG_LEVEL (env, validated)
│   │   ├── errors.rs             # the 5 spec errors (+ NOT_FOUND route, NOT_IMPLEMENTED stub) + rendering
│   │   ├── validate.rs           # fully implemented + unit tests
│   │   ├── http/
│   │   │   ├── mod.rs            # route table
│   │   │   ├── respond.rs        # JSON response helpers (always set Content-Type)
│   │   │   └── query.rs          # hand-written query string parsing
│   │   ├── routes/
│   │   │   ├── health.rs         # working
│   │   │   ├── products.rs       # stub: POST /products, GET /products/{id}, GET /products?category=
│   │   │   ├── orders.rs         # stub: POST /orders
│   │   │   └── users.rs          # stub: GET /users/{userId}/orders
│   │   ├── db/
│   │   │   ├── mod.rs            # open, pragmas, apply schema, Mutex<Connection>
│   │   │   ├── schema.sql
│   │   │   └── checkpoint.rs     # background WAL checkpoint thread
│   │   └── state/                # structs + docs only (task 2)
│   │       ├── mod.rs            # AppState: db handle, ready flag, the stores below
│   │       ├── products.rs       # product store: fixed fields + pre-built JSON + atomic stock
│   │       ├── categories.rs     # per-category list sorted by (price, id)
│   │       └── orders.rs         # per-user order lists
│   └── tests/
│       ├── http_contract.rs      # status codes, Content-Type, error bodies, 404 route, 400 cases
│       └── db_setup.rs           # pragmas applied, schema idempotent, CHECK rejects negative stock,
│                                 # UNIQUE rejects a duplicate (user, product)
│
├── tools/                        # helper binaries (task 3)
│   ├── Cargo.toml
│   └── src/bin/lss-sim.rs        # stub: mini load generator + audit of the 7 rules
│
├── load/
│   └── wrk2/README.md            # stub: wrk2 Lua scripts with the real traffic mix (task 3)
│
├── deploy/
│   ├── setup-host.sh             # idempotent host setup (section 11)
│   ├── lss.service               # systemd unit (section 10)
│   ├── journald-size.conf        # caps the journal at 200 MB
│   ├── remote-install.sh         # on the box: install release, flip symlink, restart, verify, rollback
│   ├── deploy.sh                 # laptop/CI: copy binary + scripts to the box, run remote-install
│   ├── build-on-box.sh           # fast dev loop on the box: git pull, cargo build --release, install
│   ├── smoke.sh                  # health + contract checks + last-unit + double-click
│   └── crash-test.sh             # kill -9, time the restart, verify data survived
│
└── .github/workflows/
    ├── ci.yml
    └── cd.yml
```

## 10. systemd unit (`deploy/lss.service`)

- `User=lss`, `Group=lss`, `WorkingDirectory=/opt/lss/current`,
  `ExecStart=/opt/lss/current/lss-server`
- `Environment=HOST=0.0.0.0 PORT=8080 WORKERS=2 DATABASE_PATH=/var/lib/lss/lss.db`
- `Restart=always`, `RestartSec=200ms`, **`StartLimitIntervalSec=0`** (otherwise systemd gives up
  after 5 quick crashes and the run is over)
- `LimitNOFILE=65535`, `After=network-online.target`, `Wants=network-online.target`,
  `WantedBy=multi-user.target`, `KillSignal=SIGTERM`, `TimeoutStopSec=10`

## 11. Scripts (all `set -euo pipefail`, idempotent, commented)

On-box layout already exists: system user `lss`, `/opt/lss/releases/`, `/var/lib/lss/`.
- **`setup-host.sh`** (already applied by hand; must be safe to re-run): `/etc/sysctl.d/99-lss.conf`
  (somaxconn 65535, tcp_max_syn_backlog 65535, netdev_max_backlog 16384, ip_local_port_range
  1024 65535, tcp_tw_reuse 1, tcp_fin_timeout 15, tcp_slow_start_after_idle 0),
  `/etc/security/limits.d/99-lss.conf` (nofile soft 65535 / hard 524288), user `lss`, directories,
  journald cap, apt timers + unattended-upgrades disabled, unit installed and enabled.
- **`remote-install.sh <release-name>`**: move the uploaded binary to `/opt/lss/releases/<name>/`,
  repoint `/opt/lss/current` atomically (temp symlink + `mv -T`), install the unit if changed
  (`daemon-reload`), restart, wait ≤ 30 s for `/health` = 200, run `smoke.sh`. On any failure:
  repoint to the previous release, restart, exit non-zero. Keep the last 5 releases.
- **`deploy.sh <ssh-target>`**: `scp` binary + unit + scripts, then run `remote-install.sh` with the
  short git SHA as release name.
- **`build-on-box.sh`**: pull, `cargo build --release`, install as `dev-<timestamp>`. Warn not to
  load-test while compiling.
- **`smoke.sh [base_url]`** (default `http://localhost:8080`): health; one contract check per
  endpoint; the spec's **last-unit test** (stock 1, 50 different users in parallel → exactly one
  201); the spec's **double-click test** (stock 1000, same order 20× in parallel → exactly one 201,
  stock 999, one order in the user's list). Unique ids per run. Exit non-zero on any mismatch.
  Until task 2, skip the business checks with a clear message.
- **`crash-test.sh`**: get MainPID via `systemctl show`, `kill -9` it, time until `/health` = 200,
  verify a product created before the kill still exists with the same stock.

## 12. CI/CD

- **`ci.yml`** (every push/PR, `ubuntu-24.04`): toolchain from `rust-toolchain.toml`,
  `Swatinem/rust-cache`, then `cargo fmt --check` → `cargo clippy --all-targets -- -D warnings` →
  `cargo test` → `cargo build --release` → start the binary on a temp DB and run `smoke.sh` →
  upload `lss-server` as an artifact. Building on 24.04 (glibc 2.39) is deliberate: it runs on the
  box's newer glibc. Never build on anything newer than the box.
- **`cd.yml`** (push to `main` + `workflow_dispatch`, after CI): deploy job in GitHub environment
  `production` (manual approval) downloads the artifact and runs `deploy.sh` with secrets
  `SSH_HOST`, `SSH_USER`, `SSH_KEY`. README must state deploys only work **before code freeze**.

## 13. README

ASCII architecture diagram; why each choice was made; data model; how to run, test, deploy and
roll back; the spec's pre-code-freeze checklist; troubleshooting (`ss -tlnp | grep 8080`,
`journalctl -u lss`, `ulimit -n`, inspecting the DB with `sqlite3`).

## 14. When done

1. Make `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` all pass.
2. Print the final tree.
3. List every assumption you made, any conflict you found between this prompt and the HTML spec,
   and every manual step I must do (GitHub secrets, `production` environment, first deploy).