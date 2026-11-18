#!/usr/bin/env bash
# crash-test.sh [base_url]          (default http://localhost:8080)
#
# The spec's "Test 3 · The crash", automated. Run ON THE BOX with sudo:
#   1. create a product and buy one unit of it (proves orders survive too)
#   2. kill -9 the service's main process (no graceful shutdown, like a real crash)
#   3. time how long until a NEW process answers /health = 200
#   4. check the product still has the same stock and the order is still listed
#
# Exit code is non-zero if the restart takes longer than MAX_WAIT_SECS (default
# 15) or any data is missing.
set -euo pipefail

BASE="${1:-http://localhost:8080}"
BASE="${BASE%/}"
SERVICE="${LSS_SERVICE:-lss}"
MAX_WAIT_SECS="${MAX_WAIT_SECS:-15}"

if [[ $EUID -ne 0 ]]; then
    echo "crash-test.sh: run with sudo (the service runs as user lss)" >&2
    exit 2
fi
for tool in curl jq systemctl; do
    command -v "$tool" >/dev/null 2>&1 || { echo "crash-test: $tool is required" >&2; exit 2; }
done

RUN="$(date +%s)$((RANDOM % 1000))"
PRODUCT="pcrash$RUN"
USER="ucrash$RUN"

die() { echo "FAIL: $*" >&2; exit 1; }
health_code() { curl -s -o /dev/null -w '%{http_code}' --max-time 1 "$BASE/health" || true; }
main_pid() { systemctl show -p MainPID --value "$SERVICE"; }

[[ "$(health_code)" == 200 ]] || die "server not healthy before the test"

# ---- 1. data to look for afterwards ---------------------------------------------
code="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/products" \
    -H 'content-type: application/json' \
    -d "{\"id\":\"$PRODUCT\",\"name\":\"Crash Test\",\"category\":\"ccrash$RUN\",\"price\":100,\"stock\":5}")"
[[ "$code" == 201 ]] || die "creating $PRODUCT returned $code"

ORDER="$(curl -s -X POST "$BASE/orders" -H 'content-type: application/json' \
    -d "{\"userId\":\"$USER\",\"productId\":\"$PRODUCT\"}" | jq -r '.orderId // empty')"
[[ -n "$ORDER" ]] || die "order on $PRODUCT failed"
# Stock reads may lag up to 2 s; wait until the read shows the order.
for _ in $(seq 30); do
    BEFORE="$(curl -s "$BASE/products/$PRODUCT")"
    [[ "$(jq -r .stock <<<"$BEFORE")" == 4 ]] && break
    sleep 0.1
done
[[ "$(jq -r .stock <<<"$BEFORE")" == 4 ]] || die "stock never showed 4 before the crash: $BEFORE"
echo "before: $BEFORE, order $ORDER"

# ---- 2. crash ----------------------------------------------------------------------
OLD_PID="$(main_pid)"
[[ "$OLD_PID" =~ ^[0-9]+$ && "$OLD_PID" -gt 0 ]] || die "no MainPID for $SERVICE"
RESTARTS_BEFORE="$(systemctl show -p NRestarts --value "$SERVICE")"
echo "kill -9 $OLD_PID"
START_NS="$(date +%s%N)"
kill -9 "$OLD_PID"

# ---- 3. wait for a new process to be healthy -----------------------------------------
deadline=$((SECONDS + MAX_WAIT_SECS))
while :; do
    pid="$(main_pid)"
    # The PID check stops us from timing the dying old process.
    if [[ "$pid" != "$OLD_PID" && "$pid" != 0 && "$(health_code)" == 200 ]]; then
        break
    fi
    ((SECONDS < deadline)) || die "not healthy again within ${MAX_WAIT_SECS}s"
    sleep 0.05
done
ELAPSED_MS=$((($(date +%s%N) - START_NS) / 1000000))
echo "back up as PID $pid after ${ELAPSED_MS} ms (NRestarts $RESTARTS_BEFORE -> $(systemctl show -p NRestarts --value "$SERVICE"))"

# ---- 4. data survived ---------------------------------------------------------------
AFTER="$(curl -s "$BASE/products/$PRODUCT")"
jq -e --argjson want "$BEFORE" '. == $want' >/dev/null <<<"$AFTER" ||
    die "product changed across the crash: before $BEFORE, after $AFTER"
curl -s "$BASE/users/$USER/orders" |
    jq -e --arg o "$ORDER" 'any(.orders[]; .orderId == $o)' >/dev/null ||
    die "order $ORDER missing after the crash"
echo "data intact: $AFTER, order $ORDER still listed"

echo "PASS"
