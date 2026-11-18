# wrk2 load scripts (task 3)

TODO(task-3): Lua scripts for [wrk2](https://github.com/giltene/wrk2) that
reproduce the spec's traffic mix at a fixed arrival rate (`-R`), so p99 is
measured without coordinated omission:

| Request                   | Share | Notes                                              |
|---------------------------|-------|----------------------------------------------------|
| `GET /products/:id`       | 60%   | skewed popularity, ~2% unknown ids (404)           |
| `GET /products?category=` | 15%   | mostly page 1, up to page 10; limit 10, 20 or 50   |
| `POST /orders`            | 20%   | ~70% to the current flash drop, 3% retries, 2% double-clicks |
| `GET /users/:id/orders`   | 5%    | mostly users with orders                           |

Run on the box against localhost, never while `build-on-box.sh` is compiling.
wrk2 shares the same 2 vCPUs as the server, so treat the numbers as a lower
bound. The audit of the seven rules lives in `tools/src/bin/lss-sim.rs`.
