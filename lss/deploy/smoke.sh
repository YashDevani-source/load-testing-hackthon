#!/usr/bin/env bash
# smoke.sh [base_url]          (default http://localhost:8080)
#
# Black-box checks against a running server:
#   - /health, unknown route, and the 400 cases of every endpoint (contract)
#   - one success/error check per endpoint (business)
#   - the spec's last-unit test:   stock 1, 50 different users at once -> one 201
#   - the spec's double-click test: stock 1000, same order 20x at once -> one 201,
#                                   stock 999, one order in the user's list
#
# Ids are unique per run, so it is safe to run repeatedly against the same
# database (the spec says ids and categories are never reused by the tester).
# Exit code is non-zero on any mismatch.
#
# Needs: bash, curl >= 7.68 (parallel mode), jq.
set -euo pipefail

BASE="${1:-http://localhost:8080}"
BASE="${BASE%/}"

for tool in curl jq; do
    command -v "$tool" >/dev/null 2>&1 || { echo "smoke: $tool is required" >&2; exit 2; }
done

# Lowercase letters and digits only, like the tester's ids.
RUN="$(date +%s)$((RANDOM % 1000))"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

PASS=0
FAIL=0
STATUS=""
CTYPE=""
RESP=""

ok() { PASS=$((PASS + 1)); printf '  ok    %s\n' "$1"; }
bad() {
    FAIL=$((FAIL + 1))
    printf '  FAIL  %s\n        got %s [%s] %s\n' "$1" "$STATUS" "$CTYPE" "${RESP:0:300}"
}

# req METHOD PATH [JSON_BODY]  -> sets STATUS, CTYPE, RESP
req() {
    local method="$1" path="$2" out
    local args=(-sS -o "$WORK/body" -w '%{http_code}|%{content_type}' -X "$method" --max-time 5)
    if (($# >= 3)); then
        args+=(-H 'content-type: application/json' --data-binary "$3")
    fi
    out="$(curl "${args[@]}" "$BASE$path" 2>/dev/null || true)"
    STATUS="${out%%|*}"
    CTYPE="${out#*|}"
    RESP="$(cat "$WORK/body" 2>/dev/null || true)"
    : > "$WORK/body"
}

# expect NAME STATUS JQ_FILTER [jq args...]
# Passes when the status matches, Content-Type is JSON and the jq filter is true
# for the response body.
expect() {
    local name="$1" want="$2" filter="$3"
    shift 3
    if [[ "$STATUS" == "$want" && "$CTYPE" == application/json* ]] &&
        jq -e "$@" "$filter" >/dev/null 2>&1 <<<"$RESP"; then
        ok "$name"
    else
        bad "$name (want $want)"
    fi
}

# Error bodies: an extra "message" field is allowed by the spec, so only the code
# is compared.
expect_error() { expect "$1" "$2" '.error == $code' --arg code "$3"; }

# Reads that may be up to 2 s stale (stock, order lists): retry for 3 s, which is
# how long the tester waits before its audit.
# eventually NAME PATH JQ_FILTER [jq args...]
eventually() {
    local name="$1" path="$2" filter="$3" deadline=$((SECONDS + 3))
    shift 3
    while :; do
        req GET "$path"
        if [[ "$STATUS" == 200 ]] && jq -e "$@" "$filter" >/dev/null 2>&1 <<<"$RESP"; then
            ok "$name"
            return 0
        fi
        ((SECONDS < deadline)) || break
        sleep 0.2
    done
    bad "$name"
}

# Every field of $want equals the same field of the input (extra fields allowed).
SAME_AS='. as $got | $want | to_entries | all(.value == $got[.key])'

product_json() { # id name category price stock
    jq -cn --arg id "$1" --arg name "$2" --arg cat "$3" --argjson price "$4" --argjson stock "$5" \
        '{id:$id, name:$name, category:$cat, price:$price, stock:$stock}'
}

# parallel_orders N BODY_FN -> prints one status code per line.
# Same technique as the spec's tests: curl -Z fires all N at once.
parallel_orders() {
    local n="$1" fn="$2" i args=()
    for ((i = 1; i <= n; i++)); do
        ((i > 1)) && args+=(--next)
        args+=(-s -o /dev/null -w '%{http_code}\n' --max-time 10 -X POST
            -H 'content-type: application/json' -d "$("$fn" "$i")" "$BASE/orders")
    done
    curl -Z --parallel-immediate --parallel-max "$n" "${args[@]}" || true
}

count() { grep -c "^$1\$" <<<"$2" || true; }

echo "smoke: $BASE (run $RUN)"

# ---- contract ---------------------------------------------------------------------
echo "contract"
req GET /health
expect "GET /health -> 200 ok" 200 '.status == "ok"'

req GET "/nope-$RUN"
expect "unknown route -> 404 JSON" 404 '.error | type == "string"'

req POST /products "$(product_json "pbad$RUN" "Electric Kettle" "csmk$RUN" -5 10)"
expect_error "POST /products negative price -> 400" 400 VALIDATION_ERROR
req POST /products '{"id":"x","name":"y","category":"z","price":"100","stock":1}'
expect_error "POST /products string price -> 400" 400 VALIDATION_ERROR
req POST /products 'not json'
expect_error "POST /products invalid JSON -> 400" 400 VALIDATION_ERROR

req GET "/products?page=2"
expect_error "GET /products without category -> 400" 400 VALIDATION_ERROR
req GET "/products?category=csmk$RUN&limit=80"
expect_error "GET /products limit=80 -> 400" 400 VALIDATION_ERROR
req GET "/products?category=csmk$RUN&page=0"
expect_error "GET /products page=0 -> 400" 400 VALIDATION_ERROR

req POST /orders "{\"userId\":\"usmk$RUN\"}"
expect_error "POST /orders missing productId -> 400" 400 VALIDATION_ERROR
req POST /orders "{\"userId\":\"usmk$RUN\",\"productId\":7}"
expect_error "POST /orders numeric productId -> 400" 400 VALIDATION_ERROR

# ---- business ---------------------------------------------------------------------
CAT="csmk$RUN"
P_CHEAP="psmk${RUN}c"   # price 100
P_A="psmk${RUN}a"       # price 349, sorts before P_B on id
P_B="psmk${RUN}b"       # price 349
CHEAP_JSON="$(product_json "$P_CHEAP" "Smoke Cheap" "$CAT" 100 50)"

echo "products"
req POST /products "$CHEAP_JSON"
expect "POST /products -> 201 + product" 201 "$SAME_AS" --argjson want "$CHEAP_JSON"
req POST /products "$(product_json "$P_CHEAP" "Other Name" "$CAT" 999 1)"
expect_error "POST /products duplicate id -> 409" 409 PRODUCT_EXISTS
req GET "/products/$P_CHEAP"
expect "GET /products/:id unchanged after 409" 200 "$SAME_AS" --argjson want "$CHEAP_JSON"
req GET "/products/pmissing$RUN"
expect_error "GET /products/:id unknown -> 404" 404 PRODUCT_NOT_FOUND

echo "list"
req POST /products "$(product_json "$P_B" "Smoke B" "$CAT" 349 50)"
expect "create B" 201 'true'
req POST /products "$(product_json "$P_A" "Smoke A" "$CAT" 349 50)"
expect "create A" 201 'true'
req GET "/products?category=$CAT&page=1&limit=2"
expect "list page 1: price asc, ties by id" 200 \
    '[.items[].id] == [$c, $a] and .page == 1 and .limit == 2 and .total == 3' \
    --arg c "$P_CHEAP" --arg a "$P_A"
req GET "/products?category=$CAT&page=2&limit=2"
expect "list page 2" 200 '[.items[].id] == [$b] and .total == 3' --arg b "$P_B"
req GET "/products?category=$CAT&page=9"
expect "list past the end -> items []" 200 '.items == [] and .total == 3 and .page == 9 and .limit == 20'
req GET "/products?category=cnone$RUN"
expect "list unknown category -> total 0" 200 '.items == [] and .total == 0 and .page == 1 and .limit == 20'

echo "orders"
USER="usmk$RUN"
req POST /orders "{\"userId\":\"$USER\",\"productId\":\"$P_CHEAP\"}"
expect "POST /orders -> 201" 201 \
    '(.orderId | type == "string" and length > 0) and .userId == $u and .productId == $p' \
    --arg u "$USER" --arg p "$P_CHEAP"
ORDER_ID="$(jq -r '.orderId // empty' <<<"$RESP" 2>/dev/null || true)"
req POST /orders "{\"userId\":\"$USER\",\"productId\":\"$P_CHEAP\"}"
expect_error "POST /orders again -> 409 ALREADY_PURCHASED" 409 ALREADY_PURCHASED
req POST /orders "{\"userId\":\"$USER\",\"productId\":\"pmissing$RUN\"}"
expect_error "POST /orders unknown product -> 404" 404 PRODUCT_NOT_FOUND
req POST /products "$(product_json "psmk${RUN}z" "Sold Out" "$CAT" 1 0)"
req POST /orders "{\"userId\":\"$USER\",\"productId\":\"psmk${RUN}z\"}"
expect_error "POST /orders stock 0 -> 409 SOLD_OUT" 409 SOLD_OUT
eventually "stock went down by exactly 1" "/products/$P_CHEAP" '.stock == 49'
eventually "order is in the user's list" "/users/$USER/orders" \
    '.userId == $u and (.orders | length == 1) and .orders[0].orderId == $o and .orders[0].productId == $p' \
    --arg u "$USER" --arg o "$ORDER_ID" --arg p "$P_CHEAP"
req GET "/users/unone$RUN/orders"
expect "unknown user -> 200 orders []" 200 '.orders == [] and .userId == $u' --arg u "unone$RUN"

echo "last unit (stock 1, 50 users at once)"
P_LAST="plast$RUN"
req POST /products "$(product_json "$P_LAST" "Last One" "ctest$RUN" 100 1)"
expect "create last-unit product" 201 'true'
last_body() { printf '{"userId":"u%s%s","productId":"%s"}' "$RUN" "$1" "$P_LAST"; }
codes="$(parallel_orders 50 last_body)"
c201="$(count 201 "$codes")"
c409="$(count 409 "$codes")"
if [[ "$c201" == 1 && "$c409" == 49 ]]; then
    ok "exactly one 201, 49 x 409"
else
    FAIL=$((FAIL + 1))
    printf '  FAIL  last unit: expected 1x201 + 49x409, got:\n%s\n' "$(sort <<<"$codes" | uniq -c)"
fi
eventually "last-unit product ends at stock 0" "/products/$P_LAST" '.stock == 0'

echo "double click (same order 20x at once)"
P_CLICK="pclick$RUN"
U_CLICK="udouble$RUN"
req POST /products "$(product_json "$P_CLICK" "Click Test" "ctest$RUN" 100 1000)"
expect "create double-click product" 201 'true'
click_body() { printf '{"userId":"%s","productId":"%s"}' "$U_CLICK" "$P_CLICK"; }
codes="$(parallel_orders 20 click_body)"
c201="$(count 201 "$codes")"
c409="$(count 409 "$codes")"
if [[ "$c201" == 1 && "$c409" == 19 ]]; then
    ok "exactly one 201, 19 x 409"
else
    FAIL=$((FAIL + 1))
    printf '  FAIL  double click: expected 1x201 + 19x409, got:\n%s\n' "$(sort <<<"$codes" | uniq -c)"
fi
eventually "double-click stock is 999" "/products/$P_CLICK" '.stock == 999'
eventually "double-click user has exactly one order" "/users/$U_CLICK/orders" \
    '.orders | length == 1'

echo
echo "smoke: $PASS passed, $FAIL failed"
if ((FAIL > 0)); then exit 1; fi
