#!/usr/bin/env bash
# build-on-box.sh: fast dev loop ON THE BOX (before code freeze).
#
#   git pull -> cargo build --release -> install as release dev-<timestamp>
#
# Skips the CI round trip when iterating on performance. Needs the repo cloned
# on the box and Rust installed for the current user (rustup); the toolchain
# version comes from rust-toolchain.toml.
#
# WARNING: a fat-LTO release build saturates both vCPUs for minutes. Never run
# a load test while this is compiling: the numbers will be meaningless.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

echo "WARNING: compiling uses both vCPUs; don't load-test until this finishes." >&2

command -v cargo >/dev/null || { echo "cargo not found: install rustup first" >&2; exit 2; }

git pull --ff-only
# nice: keep the running server (and your SSH session) responsive meanwhile.
nice -n 10 cargo build --release --locked -p lss-server

NAME="dev-$(date +%Y%m%d-%H%M%S)"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
cp target/release/lss-server "$STAGE/"
cp deploy/lss.service deploy/journald-size.conf deploy/*.sh "$STAGE/"

sudo bash "$STAGE/remote-install.sh" "$NAME" "$STAGE"
