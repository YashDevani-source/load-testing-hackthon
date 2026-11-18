#!/usr/bin/env bash
# remote-install.sh <release-name> [upload-dir]
#
# Runs ON THE BOX, as root. Installs an uploaded build as a new release and
# switches to it, rolling back automatically if it doesn't come up healthy.
#
#   upload-dir (default: this script's directory) must contain `lss-server`, and
#   may contain lss.service and the other deploy scripts.
#
# Steps:
#   1. copy the upload into /opt/lss/releases/<name>/
#   2. install the systemd unit if it changed (daemon-reload)
#   3. repoint /opt/lss/current atomically (temp symlink + mv -T)
#   4. restart, wait up to 30 s for /health = 200, run smoke.sh
#   5. on any failure: restore the previous unit + release, restart, exit 1
#   6. keep only the newest 5 releases
#
# Nothing here touches the network, so it also works after code freeze.
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
    echo "remote-install.sh: run as root (sudo)" >&2
    exit 2
fi

NAME="${1:?usage: remote-install.sh <release-name> [upload-dir]}"
UPLOAD="${2:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"

LSS_ROOT=/opt/lss
RELEASES="$LSS_ROOT/releases"
CURRENT="$LSS_ROOT/current"
UNIT_DST=/etc/systemd/system/lss.service
SERVICE=lss.service
BASE_URL=http://localhost:8080
HEALTH_TIMEOUT_SECS=30
KEEP_RELEASES=5

log() { printf '[remote-install] %s\n' "$*"; }
die() { log "ERROR: $*"; exit 1; }

# The name becomes a directory: allow only safe characters.
[[ "$NAME" =~ ^[A-Za-z0-9._-]+$ ]] || die "invalid release name: $NAME"
[[ -f "$UPLOAD/lss-server" ]] || die "no lss-server in $UPLOAD"

# Previous release, if any: the rollback target.
PREV=""
if [[ -L "$CURRENT" ]]; then
    PREV="$(readlink -f "$CURRENT")"
fi

# Redeploying the same git SHA must not overwrite the running release in place.
DEST="$RELEASES/$NAME"
if [[ -e "$DEST" ]]; then
    DEST="$RELEASES/$NAME-$(date +%Y%m%d%H%M%S)"
fi

# Points /opt/lss/current at $1 atomically: `ln -sfn` onto the existing link is
# two syscalls (unlink + symlink), so a restart in between would find no binary.
# Creating a temp link and renaming it over the old one is a single rename(2).
point_to() {
    ln -sfn "$1" "$CURRENT.tmp"
    mv -T "$CURRENT.tmp" "$CURRENT"
}

# 0 when /health answers 200 within $1 seconds.
wait_healthy() {
    local deadline=$((SECONDS + $1)) code
    while ((SECONDS < deadline)); do
        code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 1 "$BASE_URL/health" || true)"
        [[ "$code" == 200 ]] && return 0
        sleep 0.2
    done
    return 1
}

UNIT_BACKUP=""
UNIT_CHANGED=0

rollback() {
    log "FAILED: $1"
    journalctl -u "$SERVICE" -n 40 --no-pager || true
    if ((UNIT_CHANGED)); then
        if [[ -n "$UNIT_BACKUP" ]]; then
            install -m 0644 "$UNIT_BACKUP" "$UNIT_DST"
        fi
        systemctl daemon-reload
    fi
    if [[ -n "$PREV" && -x "$PREV/lss-server" ]]; then
        log "rolling back to $PREV"
        point_to "$PREV"
        systemctl restart "$SERVICE"
        if wait_healthy "$HEALTH_TIMEOUT_SECS"; then
            log "rollback is healthy"
        else
            log "ROLLBACK IS ALSO UNHEALTHY: check journalctl -u lss"
        fi
    else
        log "no previous release to roll back to; service left on the failed release"
    fi
    # Drop the failed release so pruning never keeps it over a good one.
    if [[ "$(readlink -f "$CURRENT" 2>/dev/null || true)" != "$DEST" ]]; then
        rm -rf "$DEST"
    fi
    exit 1
}

# ---- 1. stage the release ---------------------------------------------------------
log "installing $DEST"
install -d -m 0755 "$DEST"
install -m 0755 "$UPLOAD/lss-server" "$DEST/lss-server"
# Ship the scripts with the binary, so smoke/crash tests and rollback tooling
# are on the box after code freeze.
for f in smoke.sh crash-test.sh remote-install.sh setup-host.sh; do
    [[ -f "$UPLOAD/$f" ]] && install -m 0755 "$UPLOAD/$f" "$DEST/$f"
done
for f in lss.service journald-size.conf; do
    [[ -f "$UPLOAD/$f" ]] && install -m 0644 "$UPLOAD/$f" "$DEST/$f"
done

# ---- 2. systemd unit ------------------------------------------------------------------
if [[ -f "$UPLOAD/lss.service" ]] && ! cmp -s "$UPLOAD/lss.service" "$UNIT_DST"; then
    log "unit changed: installing + daemon-reload"
    if [[ -f "$UNIT_DST" ]]; then
        UNIT_BACKUP="$(mktemp)"
        cp "$UNIT_DST" "$UNIT_BACKUP"
    fi
    UNIT_CHANGED=1
    install -m 0644 "$UPLOAD/lss.service" "$UNIT_DST"
    systemctl daemon-reload
fi
[[ -f "$UNIT_DST" ]] || die "no unit at $UNIT_DST (run setup-host.sh first)"
systemctl enable "$SERVICE" --quiet

# ---- 3 + 4. switch, restart, verify ------------------------------------------------
point_to "$DEST"
log "current -> $DEST (previous: ${PREV:-none}); restarting"
systemctl restart "$SERVICE" || rollback "systemctl restart failed"
wait_healthy "$HEALTH_TIMEOUT_SECS" || rollback "/health not 200 within ${HEALTH_TIMEOUT_SECS}s"
log "healthy; running smoke tests"
if [[ -x "$DEST/smoke.sh" ]]; then
    "$DEST/smoke.sh" "$BASE_URL" || rollback "smoke.sh failed"
else
    log "WARNING: no smoke.sh uploaded; only /health was checked"
fi

# ---- 6. prune ---------------------------------------------------------------------------
# Newest first by mtime; never delete the current or previous release.
# (ls is fine here: release names are restricted to [A-Za-z0-9._-].)
# shellcheck disable=SC2012
mapfile -t old < <(ls -1dt "$RELEASES"/*/ 2>/dev/null | tail -n +$((KEEP_RELEASES + 1)))
for dir in "${old[@]}"; do
    dir="${dir%/}"
    [[ "$dir" == "$DEST" || "$dir" == "$PREV" ]] && continue
    log "pruning $dir"
    rm -rf "$dir"
done

[[ -n "$UNIT_BACKUP" ]] && rm -f "$UNIT_BACKUP"
log "OK: $(basename "$DEST") is live"
