# Common tasks. `make help` lists them.
#
#   make deploy TARGET=ubuntu@<ip> [BIN=path/to/linux/lss-server] [SSH_KEY_FILE=key.pem]
#
# Note: `make build` on a Mac produces a macOS binary for local testing only.
# Deployable binaries come from CI (ubuntu-24.04) or deploy/build-on-box.sh.

CARGO    ?= cargo
BASE_URL ?= http://localhost:8080
BIN      ?= target/release/lss-server
# Local runs bind to localhost and a throwaway DB under ./data (git-ignored).
RUN_ENV  ?= HOST=127.0.0.1 PORT=8080 WORKERS=2 DATABASE_PATH=./data/lss.db LOG_LEVEL=info

.PHONY: help fmt lint test build run smoke crash-test deploy check

help:
	@echo "fmt         format all Rust code"
	@echo "lint        rustfmt --check + clippy -D warnings + shellcheck"
	@echo "test        unit + integration tests"
	@echo "build       optimised release build of lss-server"
	@echo "run         build and run locally on 127.0.0.1:8080"
	@echo "smoke       run deploy/smoke.sh against BASE_URL ($(BASE_URL))"
	@echo "crash-test  kill -9 + restart check (on the box, needs sudo)"
	@echo "deploy      deploy BIN to TARGET=ubuntu@<ip> (before code freeze only)"
	@echo "check       lint + test + build: what CI runs before smoke"

fmt:
	$(CARGO) fmt --all

lint:
	$(CARGO) fmt --all --check
	$(CARGO) clippy --all-targets -- -D warnings
	@if command -v shellcheck >/dev/null; then shellcheck -x deploy/*.sh; else echo "shellcheck not installed; skipped"; fi

test:
	$(CARGO) test

build:
	$(CARGO) build --release --locked -p lss-server

run: build
	$(RUN_ENV) ./$(BIN)

smoke:
	deploy/smoke.sh $(BASE_URL)

crash-test:
	sudo deploy/crash-test.sh $(BASE_URL)

deploy:
	@test -n "$(TARGET)" || { echo "usage: make deploy TARGET=ubuntu@<ip> [BIN=...] [SSH_KEY_FILE=...]"; exit 2; }
	SSH_KEY_FILE="$(SSH_KEY_FILE)" deploy/deploy.sh "$(TARGET)" "$(BIN)"

check: lint test build
