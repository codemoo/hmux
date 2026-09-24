.DEFAULT_GOAL := all
CARGO_HOME ?= /tmp/hmux-cargo
CARGO_DENY ?= cargo-deny
CARGO_AUDIT ?= cargo-audit
SHFMT_DIR ?= $(CURDIR)/.tools
RUST_TEST_THREADS ?= 2

.PHONY: all fmt fmt-check test lint shellcheck shfmt-tool shfmt-check integration check build clean web-check web-build native-build bundle-check
all: check build

fmt:
	CARGO_HOME=$(CARGO_HOME) cargo fmt --all

fmt-check:
	CARGO_HOME=$(CARGO_HOME) cargo fmt --all -- --check

lint:
	CARGO_HOME=$(CARGO_HOME) cargo clippy --workspace --all-targets --locked -- -D warnings

test:
	CARGO_HOME=$(CARGO_HOME) cargo test --workspace --all-targets --locked -- --test-threads=$(RUST_TEST_THREADS)

web-check:
	npm ci --prefix web
	npm run check --prefix web
	npm test --prefix web

web-build:
	npm ci --prefix web
	npm run build --prefix web

shellcheck:
	shellcheck -x scripts/*.sh tests/*.sh deploy/web/*.sh crates/hmux-home/src/provider_setup.sh

shfmt-tool:
	sh scripts/install-shfmt.sh "$(SHFMT_DIR)"

shfmt-check: shfmt-tool
	"$(SHFMT_DIR)/shfmt-v3.13.1" -d scripts tests deploy/web crates/hmux-home/src/provider_setup.sh

check: fmt-check lint test shellcheck web-check

native-build:
	CARGO_HOME=$(CARGO_HOME) cargo build --workspace --exclude hmux-protocol-gen --release --locked

# Build on the target host by default; explicit cross targets require their linker/SDK.
build:
	CARGO_HOME=$(CARGO_HOME) deploy/web/build.sh

bundle-check:
	@test -n "$${HMUX_RUST_BUNDLE:?set HMUX_RUST_BUNDLE to the built native bundle directory}"
	python3 tests/rust_native_bundle.py

integration: rust-native-e2e rust-web-cli
	tests/codex_workflow_hooks.sh
	$(MAKE) rust-home-terminal HMUX_TEST_TMUX="$$(command -v tmux)"

clean:
	rm -rf dist

# Compatibility command names for existing contributor workflows; all use Rust.
.PHONY: rust-check rust-build rust-package rust-bundle-check rust-native-cli rust-native-matrix
rust-check: fmt-check lint test
rust-build: native-build
rust-package: build
rust-bundle-check: bundle-check
rust-native-cli: rust-web-cli
	CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-agent -p hmux-web -p hmux-service -p hmux-install --locked -- --test-threads=$(RUST_TEST_THREADS)
rust-native-matrix: rust-native-e2e

.PHONY: rust-web-cli rust-native-e2e
rust-web-cli:
	CARGO_HOME=$(CARGO_HOME) cargo build -p hmux-web --locked
	HMUX_TEST_RUST_WEB="$(CURDIR)/target/debug/hmux-web" python3 tests/rust_web_cli.py

rust-native-e2e:
	CARGO_HOME=$(CARGO_HOME) cargo build -p hmux-web -p hmux-agent --locked
	HMUX_NATIVE_WEB_BIN="$(CURDIR)/target/debug/hmux-web" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-gateway --test native_pair --locked -- --ignored --test-threads=1
	HMUX_RUST_HOME_PRODUCTION=1 HMUX_RUST_HOME_BIN="$(CURDIR)/target/debug/hmux-web" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-home --test runtime_wss --locked -- --ignored --test-threads=1

.PHONY: rust-dependencies rust-notices-check
rust-dependencies:
	@test -n "$${HMUX_ADVISORY_DB:?set HMUX_ADVISORY_DB to a freshly fetched RustSec advisory database}"
	CARGO_HOME=$(CARGO_HOME) $(CARGO_AUDIT) audit --no-fetch --db "$${HMUX_ADVISORY_DB}"
	CARGO_HOME=$(CARGO_HOME) $(CARGO_DENY) --locked check licenses sources

rust-notices-check:
	python3 tests/rust_notices_test.py
	@set -e; hmux_notices_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_notices_dir"' EXIT HUP INT TERM; \
	CARGO_HOME=$(CARGO_HOME) cargo fetch --locked --target "$$(rustc -vV | sed -n 's/^host: //p')"; \
	CARGO_HOME=$(CARGO_HOME) python3 scripts/rust_notices.py \
		--target "$$(rustc -vV | sed -n 's/^host: //p')" --output "$$hmux_notices_dir/notices"


# Historical resource runners take retained external oracles, never a Go toolchain.
.PHONY: rust-native-stress rust-native-capacity rust-native-activity rust-native-perf rust-native-soak
rust-native-stress rust-native-capacity:
	@test -n "$${HMUX_GATEWAY_ORACLE_BIN:?set HMUX_GATEWAY_ORACLE_BIN to the retained baseline test binary; see tests/RUST.md}"
	@test -n "$${HMUX_STRESS_OUTPUT:?set HMUX_STRESS_OUTPUT to a new private result directory}"
	@if [ "$@" = rust-native-capacity ]; then \
		python3 tests/rust_native_stress.py --capacity --output-dir "$${HMUX_STRESS_OUTPUT}"; \
	else \
		python3 tests/rust_native_stress.py --cycles "$${HMUX_STRESS_CYCLES:-100}" --output-dir "$${HMUX_STRESS_OUTPUT}"; \
	fi

rust-native-activity:
	@test -n "$${HMUX_ACTIVITY_OUTPUT:?set HMUX_ACTIVITY_OUTPUT to a new private result directory}"
	python3 tests/rust_native_activity.py --output-dir "$${HMUX_ACTIVITY_OUTPUT}"

rust-native-perf:
	@test "$$(uname -s)" = Linux || { echo 'Native paired measurements require Linux.'; exit 1; }
	@test -n "$${HMUX_PERF_OUTPUT:?set HMUX_PERF_OUTPUT to a new private result directory}"
	python3 tests/rust_native_perf.py --go-bin "$${HMUX_GO_WEB_BIN:?retained baseline binary required}" \
		--rust-bin "$${HMUX_RUST_WEB_BIN:?native release binary required}" \
		--oracle-bin "$${HMUX_GATEWAY_ORACLE_BIN:?retained baseline test binary required}" --output-dir "$${HMUX_PERF_OUTPUT}" \
		--pairs "$${HMUX_PERF_PAIRS:-5}" --count "$${HMUX_PERF_COUNT:-1000}" \
		--idle-seconds "$${HMUX_PERF_IDLE_SECONDS:-10}" --rust-protobuf

rust-native-soak:
	@test -n "$${HMUX_SOAK_OUTPUT:?set HMUX_SOAK_OUTPUT to a new private result directory}"
	python3 tests/rust_native_soak.py --rust-bin "$${HMUX_RUST_WEB_BIN:?native release binary required}" \
		--oracle-bin "$${HMUX_GATEWAY_ORACLE_BIN:?retained baseline test binary required}" --output-dir "$${HMUX_SOAK_OUTPUT}" \
		--seconds "$${HMUX_SOAK_SECONDS:-86400}" --codec "$${HMUX_SOAK_CODEC:-protobuf}"

.PHONY: rust-home-terminal rust-proto-check
# Explicit native tmux acceptance on isolated disposable sockets only.
rust-home-terminal:
	@test -n "$(HMUX_TEST_TMUX)" || { echo 'Set HMUX_TEST_TMUX to the absolute tmux executable path' >&2; exit 1; }
	HMUX_TEST_TMUX="$(HMUX_TEST_TMUX)" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-home --lib --locked isolated_real_tmux_ownership_and_original_survival -- --ignored
	HMUX_TEST_TMUX="$(HMUX_TEST_TMUX)" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-home --test terminal_view --locked -- --ignored
	HMUX_TEST_TMUX="$(HMUX_TEST_TMUX)" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-home --test session_peer --locked -- --ignored

# Developer/CI only; normal native builds consume checked-in generated types.
rust-proto-check:
	@test "$$(protoc --version)" = 'libprotoc 35.1' || { echo 'protoc 35.1 is required for schema drift checks' >&2; exit 1; }
	@hmux_proto_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_proto_dir"' EXIT HUP INT TERM; \
	PROTOC="$$(command -v protoc)" CARGO_HOME=$(CARGO_HOME) cargo run --locked -p hmux-protocol-gen -- "$$hmux_proto_dir" && \
	rustfmt --edition 2021 "$$hmux_proto_dir/hmux.v2.rs" && \
	diff -u crates/hmux-protocol/src/generated/hmux.v2.rs "$$hmux_proto_dir/hmux.v2.rs"
