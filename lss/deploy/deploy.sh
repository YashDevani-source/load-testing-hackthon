#!/usr/bin/env bash
# deploy.sh <ssh-target> [binary]
#
# Runs on your laptop or in CI. Copies a Linux x86-64 lss-server plus the unit
# and scripts to the box, then runs remote-install.sh there (which restarts,
# verifies and rolls back on failure).
#
#   ssh-target   e.g. ubuntu@203.0.113.10
#   binary       default target/release/lss-server; must be an x86-64 ELF built
#                on Ubuntu <= the box's (the CI artifact is exactly that)
#
# Env:
#   SSH_KEY_FILE   private key to use (e.g. your .pem)
#   RELEASE_NAME   override the release name (default: short git SHA)
#
# Only works BEFORE code freeze: afterwards SSH is closed.
set -euo pipefail

TARGET="${1:?usage: deploy.sh <ssh-target> [binary]}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${2:-$REPO_ROOT/target/release/lss-server}"

log() { printf '[deploy] %s\n' "$*"; }
die() { log "ERROR: $*"; exit 1; }

[[ -f "$BIN" ]] || die "binary not found: $BIN"
# A macOS build (Mach-O arm64) would install fine and then fail to start.
# Catch it here instead of relying on the rollback.
if ! file -b "$BIN" | grep -q 'ELF 64-bit.*x86-64'; then
    die "$BIN is not a Linux x86-64 binary ($(file -b "$BIN")). Use the CI artifact or deploy/build-on-box.sh."
fi

if [[ -n "${RELEASE_NAME:-}" ]]; then
    NAME="$RELEASE_NAME"
else
    NAME="$(git -C "$REPO_ROOT" rev-parse --short HEAD)"
    # Make it obvious in /opt/lss/releases when a build had uncommitted changes.
    if [[ -n "$(git -C "$REPO_ROOT" status --porcelain)" ]]; then
        NAME="$NAME-dirty"
    fi
fi

SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=15 -o StrictHostKeyChecking=accept-new)
if [[ -n "${SSH_KEY_FILE:-}" ]]; then
    SSH_OPTS+=(-i "$SSH_KEY_FILE")
fi

log "release $NAME -> $TARGET"
REMOTE_DIR="$(ssh "${SSH_OPTS[@]}" "$TARGET" 'mktemp -d /tmp/lss-upload.XXXXXX')"
[[ "$REMOTE_DIR" == /tmp/lss-upload.* ]] || die "unexpected remote temp dir: $REMOTE_DIR"

scp "${SSH_OPTS[@]}" -q "$BIN" "$TARGET:$REMOTE_DIR/lss-server"
scp "${SSH_OPTS[@]}" -q \
    "$REPO_ROOT/deploy/lss.service" \
    "$REPO_ROOT/deploy/journald-size.conf" \
    "$REPO_ROOT/deploy/remote-install.sh" \
    "$REPO_ROOT/deploy/setup-host.sh" \
    "$REPO_ROOT/deploy/smoke.sh" \
    "$REPO_ROOT/deploy/crash-test.sh" \
    "$TARGET:$REMOTE_DIR/"

# Always clean up the upload dir, and pass remote-install's exit code through.
log "running remote-install.sh"
ssh "${SSH_OPTS[@]}" "$TARGET" \
    "sudo bash $REMOTE_DIR/remote-install.sh $(printf '%q' "$NAME") $REMOTE_DIR; rc=\$?; rm -rf $REMOTE_DIR; exit \$rc"
log "done"
