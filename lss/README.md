# Last Server Standing: `lss`

Our entry for **Last Server Standing** (Rules v1.1, 18 Nov 2026). It is a flash-sale API that has to
survive a rising load on one AWS m7i.large (2 vCPU = **one physical core**, 8 GiB) with
p99 ≤ 100 ms, ≤ 1% errors and **zero wrong answers**. Target: level 10 (10,000 req/s).
Stretch: level 17 (50,000 req/s).

> **Status: task 2 done.** All six endpoints work in both read modes (`READ_SOURCE=sqlite|memory`).
> Design and the mapping of the spec's seven rules to code: [docs/DESIGN.md](docs/DESIGN.md).

## Architecture

```
                 load generator (same AZ, one IP, up to 5,000 keep-alive conns)
                                        │  HTTP/1.1, port 8080
┌───────────────────────────────────────▼──────────────────────────────────────────┐
│ m7i.large · Ubuntu 24.04 · systemd unit `lss` (Restart=always, enabled at boot)  │
│                                                                                  │
│  lss-server (one process, Rust, mimalloc)                                        │
│  ┌──────────────────────────────────────────────────────────────────────────┐    │
│  │ actix-web: accept thread + WORKERS=2 workers (one per hyperthread)       │    │
│  │   routes ─► validate.rs (pure)  ─► errors.rs ({"error":"CODE"})          │    │
│  │      │                                                                    │    │
│  │      ├─ reads (80%) ─► READ_SOURCE=sqlite: per-worker read-only conn    │    │
│  │      │                 READ_SOURCE=memory: caches rebuilt from SQLite     │    │
│  │      │                                                                    │    │
│  │      └─ writes ──────► parking_lot::Mutex<rusqlite::Connection> (writer) │    │
│  │                        every order decided in one SQLite transaction     │    │
│  └────────────────────────────────────────┬─────────────────────────────────┘    │
│     lss-checkpoint thread (own connection) │  PRAGMA wal_checkpoint(PASSIVE)/5 s │
│                                            ▼                                     │
│     /var/lib/lss/lss.db  +  lss.db-wal   (SQLite, WAL, synchronous=NORMAL)       │
└──────────────────────────────────────────────────────────────────────────────────┘
```

### Why each choice

| Choice | Why |
|---|---|
| **Rust + actix-web 4** | No GC pauses (p99 is what's scored), small per-request cost, and actix is among the fastest HTTP/1.1 servers. |
| **2 workers** | One per hyperthread. More threads on one physical core just add context switches. |
| **SQLite in-process (`rusqlite`, bundled)** | The data has to be on disk, but a separate DB server would compete for our one core and add a network hop. `bundled` compiles SQLite into the binary, so nothing needs installing after code freeze. |
| **One connection behind a `parking_lot::Mutex`** | SQLite allows one writer at a time anyway. An uncontended mutex costs nanoseconds; SQLite's file-lock handoff between connections costs far more. |
| **No `web::block` around SQLite** | A query takes microseconds. Moving it to the blocking pool costs more than running it. |
| **WAL + `synchronous=NORMAL`** | A commit is an append to the WAL with no fsync. `kill -9` loses nothing (the data is already in the OS page cache); only a power cut could lose the last few commits. |
| **`wal_autocheckpoint=0` + background checkpoint** | With auto-checkpoint, whichever request commits the 1000th page pays for the checkpoint, which shows up as p99 spikes. A background thread with **its own connection** checkpoints instead, so it never holds the request mutex. |
| **`CHECK (stock >= 0)`, `UNIQUE (user_id, product_id)`** | These are the rules of the sale enforced by the database itself. Even with a logic bug, an oversell or a double purchase fails to commit. |
| **Two read modes** | `sqlite` (default): reads run on a read-only connection per worker, in parallel with the writer thanks to WAL. `memory`: reads come from caches that are loaded from SQLite at startup and updated after each commit. Either way, reads never take the writer mutex, and the **order decision always uses the database**. |
| **Hand-checked JSON / query parsing** | actix's typed extractors answer bad input with plain-text errors and accept/reject shapes that don't match the spec. Checking `serde_json::Value` by hand makes every rule explicit and unit-tested. |
| **mimalloc** | Fast small allocations and no glibc arena contention between workers. |
| **No per-request logging** | 10k log lines/s would cost real CPU. We log startup, shutdown and errors only. |
| **clippy `deny(unwrap, expect, panic, indexing, todo, unimplemented)`** | A panic in a request drops connections. The compiler forces every failure to become a spec error or a logged 500. |
| **systemd, no Docker** | Restart on crash and on boot with no network: `Restart=always`, `StartLimitIntervalSec=0`, `WantedBy=multi-user.target`. |
| **Fat LTO, `target-cpu=x86-64-v3`, line tables** | The fastest code for the box's CPU, while keeping symbols readable in `perf`. |

## Data model

Schema version 2 (`PRAGMA user_version = 2`):

```sql
products(id TEXT PRIMARY KEY, name TEXT NOT NULL, category TEXT NOT NULL,
         price INTEGER NOT NULL CHECK (price >= 0),
         stock INTEGER NOT NULL CHECK (stock >= 0),
         json_prefix TEXT NOT NULL DEFAULT '')      -- {"id":..,"name":..,"category":..,"price":..,"stock":
INDEX products_category_price_id ON products(category, price, id)   -- list order

orders(order_id INTEGER PRIMARY KEY,                -- rowid: unique, monotonic -> "o_<id>"
       user_id TEXT NOT NULL, product_id TEXT NOT NULL,
       UNIQUE (user_id, product_id))                -- one purchase per user/product

categories(category TEXT PRIMARY KEY, total INTEGER NOT NULL) WITHOUT ROWID
```

Version 1 is [`server/src/db/schema.sql`](server/src/db/schema.sql). [`migrate.rs`](server/src/db/migrate.rs)
upgrades any older database in place, in one transaction, at startup. `id`, `name`, `category`
and `price` never change. `stock` changes only through orders. Nothing is ever deleted.

**Order transaction:** `UPDATE … SET stock = stock - 1 WHERE id = ? AND stock > 0 RETURNING stock`
(no row: 404 if the product doesn't exist, else 409 `SOLD_OUT`) → `INSERT` the order (UNIQUE
violation → 409 `ALREADY_PURCHASED`) → `COMMIT`. Every error drops the transaction, which rolls it
back, so nothing changes. Full reasoning: [docs/DESIGN.md](docs/DESIGN.md).

## Repository layout

```
server/        lss-server: the API (lib + thin main.rs so tests can build the app in-process)
tools/         lss-sim: load generator + rule audit (task 3, stub)
load/wrk2/     wrk2 Lua scripts for the traffic mix (task 3, stub)
deploy/        systemd unit, host setup, install/rollback, smoke + crash tests
.github/       CI (lint/test/build/smoke/artifact) and CD (approved deploy)
```

## Run and test locally

Requires [rustup](https://rustup.rs). The pinned toolchain (1.100.0) installs automatically from
`rust-toolchain.toml`. The scripts also need `curl` and `jq`.

```bash
make lint        # rustfmt --check, clippy -D warnings, shellcheck
make test        # every test; each HTTP test runs in both read modes
make run         # release build, serves on 127.0.0.1:8080, DB in ./data/lss.db
make smoke       # in another terminal: deploy/smoke.sh against localhost:8080
```

Environment variables (validated at startup; a bad value exits with a message):

| Variable | Default | Notes |
|---|---|---|
| `HOST` | `0.0.0.0` | must be an IP |
| `PORT` | `8080` | 1–65535 |
| `WORKERS` | `2` | 1–64 |
| `DATABASE_PATH` | `./data/lss.db` | parent directory is created |
| `LOG_LEVEL` | `info` | any `tracing` filter, e.g. `warn` or `lss_server=debug` |
| `READ_SOURCE` | `sqlite` | `sqlite` or `memory`: where GETs read from (writes always go to SQLite) |

## Deploy

**Deploys only work before code freeze.** After that SSH is closed and the box runs whatever
release is current. It restarts itself after a crash or a reboot with no network.

On-box layout:

```
/opt/lss/releases/<name>/   lss-server + lss.service + smoke.sh, crash-test.sh, ... (last 5 kept)
/opt/lss/current            symlink to the active release (flipped atomically)
/var/lib/lss/lss.db         the database (owned by user lss)
/etc/systemd/system/lss.service
```

1. **One-time host setup** (already applied, safe to re-run):
   `scp -r deploy ubuntu@<ip>:` and then `ssh ubuntu@<ip> sudo bash deploy/setup-host.sh`
2. **Normal path: CI/CD.** Push to `main`. CD runs CI, waits for approval on the `production`
   environment, then deploys the exact binary CI built and tested (release name = short SHA).
3. **Manual deploy from a laptop** (needs a Linux x86-64 binary, e.g. the CI artifact):
   ```bash
   gh run download <run-id> -n lss-server -D dist
   SSH_KEY_FILE=~/keys/lss.pem make deploy TARGET=ubuntu@<ip> BIN=dist/lss-server
   ```
   A `make build` on a Mac produces a macOS binary. `deploy.sh` refuses to ship it.
4. **Fast loop on the box:** `deploy/build-on-box.sh` (git pull → release build → install as
   `dev-<timestamp>`). Needs rustup on the box. **Don't load-test while it compiles.**

`remote-install.sh` (on the box) installs the release, repoints `current`, restarts, waits ≤ 30 s
for `/health`, runs `smoke.sh` and **rolls back automatically** if any step fails.

### Roll back by hand

```bash
ls -1t /opt/lss/releases/                       # newest first
sudo ln -sfn /opt/lss/releases/<good> /opt/lss/current.tmp
sudo mv -T /opt/lss/current.tmp /opt/lss/current
sudo systemctl restart lss && curl -s localhost:8080/health
```

## Pre-code-freeze checklist (spec §11)

Run all of this **on the box**:

- [ ] `ss -tlnp | grep 8080` shows `0.0.0.0:8080` (not `127.0.0.1`)
- [ ] `curl -s localhost:8080/health` → `{"status":"ok"}`
- [ ] Database is local: `DATABASE_PATH=/var/lib/lss/lss.db`, with no remote URLs anywhere
- [ ] `sudo /opt/lss/current/crash-test.sh`: comes back on its own within seconds, data intact (test 3)
- [ ] `sudo reboot`, then `curl localhost:8080/health` → ok without logging in to start anything
- [ ] Restart needs no internet: no git pull, no downloads, no installs on start (true by design)
- [ ] No dev server, watcher or per-request logging (true by design)
- [ ] `/opt/lss/current/smoke.sh` passes. It includes every status/error code, the
      **last-unit test** (test 1: exactly one 201) and the **double-click test**
      (test 2: exactly one 201, stock 999, one order listed)
- [ ] `systemctl is-enabled lss` → `enabled`; `systemctl is-active apt-daily.timer` → inactive
- [ ] Disk has room: `df -h /var/lib/lss`; the journal is capped at 200 MB

## Troubleshooting

```bash
ss -tlnp | grep 8080                       # listening? on 0.0.0.0?
systemctl status lss                       # running? restart count?
journalctl -u lss -n 100 --no-pager        # startup errors, panics, 500 causes
journalctl -u lss -f                       # follow live
cat /proc/$(systemctl show -p MainPID --value lss)/limits | grep 'open files'   # LimitNOFILE applied?
ulimit -n                                  # your shell's limit (for load tools), see limits.d
sysctl net.core.somaxconn                  # 65535 after setup-host.sh
readlink -f /opt/lss/current               # which release is live
```

Inspect the database (safe while the server runs, because WAL lets readers in):

```bash
sudo -u lss sqlite3 /var/lib/lss/lss.db
sqlite> PRAGMA journal_mode;                              -- wal
sqlite> SELECT count(*) FROM products;
sqlite> SELECT id, price, stock FROM products WHERE category = 'c8d21fa' ORDER BY price, id LIMIT 5;
sqlite> SELECT * FROM orders WHERE user_id = 'u7b0e44c19a2f';
sqlite> PRAGMA user_version;                             -- 2
sqlite> SELECT p.id, p.stock, count(o.order_id) AS sold
        FROM products p LEFT JOIN orders o ON o.product_id = p.id GROUP BY p.id LIMIT 10;
```

Common failures:

- **Nothing listens on 8080 after a deploy:** startup failed (migration, memory load, bind) and
  systemd keeps restarting it. `journalctl -u lss` shows the reason.
- **Exits immediately with `invalid PORT=...` (or another variable):** fix the `Environment=` line in the unit.
- **`journal_mode is "delete"`:** the DB path is on a filesystem without shared-memory support. Keep it on local disk.
- **Connections refused under load:** check `LimitNOFILE` (above) and `somaxconn`, and whether
  `systemctl status lss` shows restarts.
