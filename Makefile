GOCACHE ?= /tmp/hmux-go-cache
GOPATH ?= /tmp/hmux-go
SHFMT_DIR ?= $(CURDIR)/.tools

.PHONY: all fmt fmt-check test race vet shellcheck shfmt-check integration check build clean web-check web-build

web-check:
	npm ci --prefix web
	npm run check --prefix web
	npm test --prefix web
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test -race ./internal/webgateway ./internal/home ./cmd/hmux-web

web-build:
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) deploy/web/build.sh

all: web-check web-build

fmt:
	gofmt -w cmd internal bench/hmux/go-driver tools
	gofmt -w third_party/token-terrier-server/cmd third_party/token-terrier-server/internal third_party/token-terrier-server/stream

fmt-check:
	test -z "$$(gofmt -l cmd internal bench/hmux/go-driver tools)"
	test -z "$$(gofmt -l third_party/token-terrier-server/cmd third_party/token-terrier-server/internal third_party/token-terrier-server/stream)"

test:
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test ./...
	cd third_party/token-terrier-server && GOCACHE=$(GOCACHE)-usage GOPATH=$(GOPATH)-usage go test ./...

race:
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test -race ./...
	cd third_party/token-terrier-server && GOCACHE=$(GOCACHE)-usage GOPATH=$(GOPATH)-usage go test -race ./...

vet:
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go vet ./...
	cd third_party/token-terrier-server && GOCACHE=$(GOCACHE)-usage GOPATH=$(GOPATH)-usage go vet ./...

shellcheck:
	shellcheck -x scripts/*.sh tests/*.sh deploy/web/*.sh

.PHONY: shfmt-tool
shfmt-tool:
	sh scripts/install-shfmt.sh "$(SHFMT_DIR)"

shfmt-check: shfmt-tool
	"$(SHFMT_DIR)/shfmt-v3.13.1" -d scripts tests deploy/web

integration:
	python3 tests/home_install_test.py
	tests/codex_workflow_hooks.sh
	HMUX_RUN_TMUX_CREATE_TEST=1 GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test ./internal/agent -run 'TestCreateSessionWithIsolatedTmux|TestProvidersExitToShellWithIsolatedTmux' -count=1
	HMUX_RUN_WEB_TMUX_TEST=1 GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test ./internal/home -run TestWebTerminalViewWithIsolatedTmux -count=1

check: fmt-check test race vet shellcheck web-check

build: web-build

clean:
	rm -rf dist

# Rust remains a candidate during migration; build never deploys or replaces Go.
CARGO_HOME ?= /tmp/hmux-cargo
CARGO_DENY ?= cargo-deny
CARGO_AUDIT ?= cargo-audit
# Bound test subprocess contention without relaxing runtime deadlines.
RUST_TEST_THREADS ?= 2
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

.PHONY: rust-check rust-compat rust-build rust-gateway-candidate rust-full-gateway-candidate rust-gateway-e2e rust-home-peer-compat rust-home-lock-compat rust-home-state-compat rust-home-terminal rust-proto-check
rust-check:
	CARGO_HOME=$(CARGO_HOME) cargo fmt --all -- --check
	CARGO_HOME=$(CARGO_HOME) cargo clippy --workspace --all-targets --locked -- -D warnings
	CARGO_HOME=$(CARGO_HOME) cargo test --workspace --all-targets --locked -- --test-threads=$(RUST_TEST_THREADS)

rust-compat: rust-home-peer-compat rust-home-lock-compat rust-home-state-compat rust-home-continuity-compat
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test ./internal/config ./internal/model ./internal/catalog ./internal/sharedworkspace ./internal/webgateway -run 'TestRust|TestWireV1' -count=1
	@hmux_wire_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_wire_dir"' EXIT HUP INT TERM; \
	HMUX_RUST_WIRE_OUTPUT="$$hmux_wire_dir" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-protocol --test go_wire --locked && \
	HMUX_RUST_WIRE_OUTPUT="$$hmux_wire_dir" GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test ./internal/webgateway -run TestWireV1CompatibilityCorpus -count=1
	@hmux_lock_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_lock_dir"' EXIT HUP INT TERM; \
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go build -o "$$hmux_lock_dir/go-flock" ./tools/hmux-flock-oracle && \
	HMUX_GO_FLOCK_HELPER="$$hmux_lock_dir/go-flock" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-core --test go_lock --locked -- --ignored
	@hmux_auth_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_auth_dir"' EXIT HUP INT TERM; \
	HMUX_RUST_AUTH_HANDOFF="$$hmux_auth_dir" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-gateway --test http_auth --locked auth_http_restart_revocation_cookie_and_csrf_contracts && \
	HMUX_RUST_AUTH_HANDOFF="$$hmux_auth_dir" GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test ./internal/webgateway -run '^TestRustAuthStoreHandoff$$' -count=1 && \
	HMUX_RUST_AUTH_HANDOFF="$$hmux_auth_dir" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-gateway --test http_auth --locked reload_current_state_after_go_logout -- --ignored
	@hmux_preferences_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_preferences_dir"' EXIT HUP INT TERM; \
	HMUX_RUST_PREFERENCES_HANDOFF="$$hmux_preferences_dir" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-gateway --lib --locked settings_survive_restart_and_remain_account_and_profile_scoped && \
	HMUX_RUST_PREFERENCES_HANDOFF="$$hmux_preferences_dir" GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test ./internal/webgateway -run '^TestRustUsagePreferenceHandoff$$' -count=1 && \
	HMUX_RUST_PREFERENCES_HANDOFF="$$hmux_preferences_dir" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-gateway --lib --locked reload_current_preferences_after_go_update -- --ignored
	@hmux_workspace_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_workspace_dir"' EXIT HUP INT TERM; \
	HMUX_RUST_WORKSPACE_HANDOFF="$$hmux_workspace_dir" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-core --lib --locked restart_scopes_and_current_workspace_handoff && \
	HMUX_RUST_WORKSPACE_HANDOFF="$$hmux_workspace_dir" GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test ./internal/sharedworkspace -run '^TestRustWorkspaceHandoff$$' -count=1 && \
	HMUX_RUST_WORKSPACE_HANDOFF="$$hmux_workspace_dir" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-core --lib --locked reload_workspace_after_current_go_write -- --ignored

	@hmux_diagnostics_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_diagnostics_dir"' EXIT HUP INT TERM; \
	HMUX_RUST_DIAGNOSTICS_HANDOFF="$$hmux_diagnostics_dir" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-gateway --lib --locked account_exports_omit_private_fields_and_shutdown_hands_current_state_to_go && \
	HMUX_RUST_DIAGNOSTICS_HANDOFF="$$hmux_diagnostics_dir" GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test ./internal/webgateway -run '^TestRustDiagnosticsHandoff$$' -count=1 && \
	HMUX_RUST_DIAGNOSTICS_HANDOFF="$$hmux_diagnostics_dir" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-gateway --lib --locked reload_current_diagnostics_after_go_append -- --ignored
	@hmux_gateway_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_gateway_dir"' EXIT HUP INT TERM; \
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test -c -o "$$hmux_gateway_dir/go-gateway" ./internal/webgateway && \
	HMUX_GO_UPLOAD_HELPER="$$hmux_gateway_dir/go-gateway" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-gateway --test http_auth --locked upload_actual_go_home_commits_binary_files_and_three_hour_expiry -- --ignored && \
	HMUX_GO_PUSH_HELPER="$$hmux_gateway_dir/go-gateway" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-gateway --lib --locked current_push_state_survives_go_handoff_and_lock_conflict -- --ignored && \
	HMUX_GO_PUSH_CRYPTO_HELPER="$$hmux_gateway_dir/go-gateway" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-gateway --lib --locked actual_go_and_rust_push_crypto_interoperate -- --ignored
	@hmux_delivery_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_delivery_dir"' EXIT HUP INT TERM; \
	HMUX_PUSH_DELIVERY_CAPTURE="$$hmux_delivery_dir/request.json" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-gateway --lib --locked browser_oracle_and_completion_delivery_preserve_scope_presence_and_deep_links && \
	HMUX_PUSH_DELIVERY_CAPTURE="$$hmux_delivery_dir/request.json" GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test ./internal/webgateway -run '^TestRustPushDeliveredRequest$$' -count=1

rust-build:
	CARGO_HOME=$(CARGO_HOME) cargo build --workspace --exclude hmux-protocol-gen --release --locked

# Opt-in native binaries and web assets, in separate candidate bundles.
.PHONY: rust-package rust-bundle-check rust-web-cli rust-native-cli rust-native-e2e
rust-package:
	CARGO_HOME=$(CARGO_HOME) deploy/web/build-rust.sh

rust-bundle-check:
	HMUX_RUST_BUNDLE="$${HMUX_RUST_BUNDLE:-$(CURDIR)/dist/rust-web-$$(rustc -vV | sed -n 's/^host: //p')}" python3 tests/rust_native_bundle.py

rust-web-cli:
	CARGO_HOME=$(CARGO_HOME) cargo build -p hmux-web --locked
	HMUX_TEST_RUST_WEB="$(CURDIR)/target/debug/hmux-web" python3 tests/rust_web_cli.py

rust-native-cli: rust-web-cli
	CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-agent -p hmux-web -p hmux-service -p hmux-install --locked -- --test-threads=2

.PHONY: rust-native-helper-compat
rust-native-helper-compat:
	CARGO_HOME=$(CARGO_HOME) cargo build -p hmux-agent --locked
	@hmux_cli_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_cli_dir"' EXIT HUP INT TERM; \
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go build -o "$$hmux_cli_dir/hmux-agent" ./cmd/hmux-agent && \
	HMUX_GO_AGENT_BIN="$$hmux_cli_dir/hmux-agent" HMUX_RUST_AGENT_BIN="$(CURDIR)/target/debug/hmux-agent" python3 tests/rust_helper_compat.py

.PHONY: rust-native-installed-pairs
rust-native-installed-pairs:
	CARGO_HOME=$(CARGO_HOME) cargo build -p hmux-web -p hmux-agent --locked
	@set -e; hmux_pair_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_pair_dir"' EXIT HUP INT TERM; \
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go build -o "$$hmux_pair_dir/hmux-web" ./cmd/hmux-web; \
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go build -o "$$hmux_pair_dir/hmux-agent" ./cmd/hmux-agent; \
	HMUX_GO_WEB_BIN="$$hmux_pair_dir/hmux-web" HMUX_GO_AGENT_BIN="$$hmux_pair_dir/hmux-agent" HMUX_RUST_WEB_BIN="$(CURDIR)/target/debug/hmux-web" HMUX_RUST_AGENT_BIN="$(CURDIR)/target/debug/hmux-agent" python3 tests/rust_installed_pairs.py

rust-native-e2e:
	CARGO_HOME=$(CARGO_HOME) cargo build -p hmux-web -p hmux-agent --locked
	HMUX_RUST_GATEWAY_PRODUCTION=1 HMUX_RUST_GATEWAY_BIN="$(CURDIR)/target/debug/hmux-web" GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test -race ./internal/webgateway -run '^TestRustFullGatewayWithGoHome$$' -count=1 -timeout 2m
	HMUX_RUST_HOME_PRODUCTION=1 HMUX_RUST_HOME_BIN="$(CURDIR)/target/debug/hmux-web" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-home --test runtime_wss --locked -- --ignored

.PHONY: rust-native-matrix rust-native-stress rust-native-capacity
rust-native-matrix rust-native-stress rust-native-capacity:
	@if [ "$@" != rust-native-matrix ]; then test -n "$${HMUX_STRESS_OUTPUT:?set HMUX_STRESS_OUTPUT to a new private result directory}"; fi
	@if [ -z "$${HMUX_RUST_WEB_BIN:-}" ]; then CARGO_HOME=$(CARGO_HOME) cargo build -p hmux-web --locked; fi
	@set -e; hmux_matrix_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_matrix_dir"' EXIT HUP INT TERM; \
	if [ "$@" = rust-native-matrix ]; then GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go build -o "$$hmux_matrix_dir/hmux-web" ./cmd/hmux-web; fi; \
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test -race -c -o "$$hmux_matrix_dir/gateway-oracle" ./internal/webgateway; \
	if [ "$@" = rust-native-capacity ]; then \
		HMUX_RUST_WEB_BIN="$${HMUX_RUST_WEB_BIN:-$(CURDIR)/target/debug/hmux-web}" HMUX_GATEWAY_ORACLE_BIN="$$hmux_matrix_dir/gateway-oracle" python3 tests/rust_native_stress.py --capacity --output-dir "$${HMUX_STRESS_OUTPUT:?set HMUX_STRESS_OUTPUT to a new private result directory}"; \
	elif [ "$@" = rust-native-stress ]; then \
		HMUX_RUST_WEB_BIN="$${HMUX_RUST_WEB_BIN:-$(CURDIR)/target/debug/hmux-web}" HMUX_GATEWAY_ORACLE_BIN="$$hmux_matrix_dir/gateway-oracle" python3 tests/rust_native_stress.py --cycles "$${HMUX_STRESS_CYCLES:-100}" --output-dir "$${HMUX_STRESS_OUTPUT:?set HMUX_STRESS_OUTPUT to a new private result directory}"; \
	else \
		HMUX_RUST_WEB_BIN="$${HMUX_RUST_WEB_BIN:-$(CURDIR)/target/debug/hmux-web}" HMUX_GO_WEB_BIN="$$hmux_matrix_dir/hmux-web" HMUX_GATEWAY_ORACLE_BIN="$$hmux_matrix_dir/gateway-oracle" python3 tests/rust_native_matrix.py; \
	fi

.PHONY: rust-native-activity
rust-native-activity:
	@test -n "$${HMUX_ACTIVITY_OUTPUT:?set HMUX_ACTIVITY_OUTPUT to a new private result directory}"
	@if [ -z "$${HMUX_RUST_WEB_BIN:-}" ]; then CARGO_HOME=$(CARGO_HOME) cargo build -p hmux-web --locked; fi
	@set -e; hmux_activity_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_activity_dir"' EXIT HUP INT TERM; \
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test -race -c -o "$$hmux_activity_dir/gateway-oracle" ./internal/webgateway; \
	HMUX_RUST_WEB_BIN="$${HMUX_RUST_WEB_BIN:-$(CURDIR)/target/debug/hmux-web}" HMUX_GATEWAY_ORACLE_BIN="$$hmux_activity_dir/gateway-oracle" python3 tests/rust_native_activity.py --output-dir "$${HMUX_ACTIVITY_OUTPUT}"

.PHONY: rust-native-perf
rust-native-perf:
	@test "$$(uname -s)" = Linux || { echo 'Native paired measurements require Linux.'; exit 1; }
	@test -n "$${HMUX_PERF_OUTPUT:?set HMUX_PERF_OUTPUT to a new private result directory}"
	@if [ -z "$${HMUX_RUST_WEB_BIN:-}" ]; then CARGO_HOME=$(CARGO_HOME) cargo build --release -p hmux-web --locked; fi
	@set -e; hmux_perf_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_perf_dir"' EXIT HUP INT TERM; \
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go build -o "$$hmux_perf_dir/hmux-web" ./cmd/hmux-web; \
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test -c -o "$$hmux_perf_dir/gateway-oracle" ./internal/webgateway; \
	python3 tests/rust_native_perf.py --go-bin "$$hmux_perf_dir/hmux-web" \
		--rust-bin "$${HMUX_RUST_WEB_BIN:-$(CURDIR)/target/release/hmux-web}" \
		--oracle-bin "$$hmux_perf_dir/gateway-oracle" --output-dir "$${HMUX_PERF_OUTPUT}" \
		--pairs "$${HMUX_PERF_PAIRS:-5}" --count "$${HMUX_PERF_COUNT:-1000}" \
		--idle-seconds "$${HMUX_PERF_IDLE_SECONDS:-10}" --rust-protobuf

.PHONY: rust-native-soak
rust-native-soak:
	@test -n "$${HMUX_SOAK_OUTPUT:?set HMUX_SOAK_OUTPUT to a new private result directory}"
	@if [ -z "$${HMUX_RUST_WEB_BIN:-}" ]; then CARGO_HOME=$(CARGO_HOME) cargo build --release -p hmux-web --locked; fi
	@set -e; hmux_soak_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_soak_dir"' EXIT HUP INT TERM; \
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test -c -o "$$hmux_soak_dir/gateway-oracle" ./internal/webgateway; \
	python3 tests/rust_native_soak.py --rust-bin "$${HMUX_RUST_WEB_BIN:-$(CURDIR)/target/release/hmux-web}" \
		--oracle-bin "$$hmux_soak_dir/gateway-oracle" --output-dir "$${HMUX_SOAK_OUTPUT}" \
		--seconds "$${HMUX_SOAK_SECONDS:-86400}" --codec "$${HMUX_SOAK_CODEC:-protobuf}"

# Isolated auth-only example; never used as a production deployment input.
rust-gateway-candidate:
	CARGO_HOME=$(CARGO_HOME) cargo build -p hmux-gateway --example auth_gateway --release --locked

# Full isolated candidate; normal Go build/deployment paths are unchanged.
rust-full-gateway-candidate:
	CARGO_HOME=$(CARGO_HOME) cargo build -p hmux-gateway --example gateway_candidate --release --locked

.PHONY: rust-home-candidate rust-home-candidate-e2e
rust-home-candidate:
	CARGO_HOME=$(CARGO_HOME) cargo build -p hmux-home --example home_candidate --release --locked

rust-home-candidate-e2e: rust-home-candidate
	HMUX_RUST_HOME_BIN="$(CURDIR)/target/release/examples/home_candidate" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-home --test runtime_wss --locked -- --ignored

# Actual Go/Rust state exchange on synthetic recovery/workflow files only.
.PHONY: rust-home-continuity-compat
rust-home-continuity-compat:
	CARGO_HOME=$(CARGO_HOME) cargo build -p hmux-home --example recovery_oracle --example workflow_oracle --locked
	HMUX_RUST_RECOVERY_ORACLE="$(CURDIR)/target/debug/examples/recovery_oracle" HMUX_RUST_WORKFLOW_ORACLE="$(CURDIR)/target/debug/examples/workflow_oracle" GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test -race ./internal/recovery ./internal/workflow -run '^TestRustCurrent' -count=1

.PHONY: rust-home-recovery-e2e
rust-home-recovery-e2e:
	@test -n "$(HMUX_TEST_TMUX)" || { echo 'Set HMUX_TEST_TMUX to an absolute tmux executable.'; exit 2; }
	CARGO_HOME=$(CARGO_HOME) cargo build -p hmux-home --example recovery_oracle --locked
	HMUX_RUST_RECOVERY_ORACLE="$(CURDIR)/target/debug/examples/recovery_oracle" HMUX_TEST_TMUX="$(HMUX_TEST_TMUX)" GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test -race ./internal/recovery -run '^TestRustRecoveryWithIsolatedTmuxAndFakeProviders$$' -count=1

# Actual Go Home workers/PTY with synthetic process tools and loopback sockets.
rust-gateway-e2e: rust-full-gateway-candidate
	HMUX_RUST_GATEWAY_BIN="$(CURDIR)/target/release/examples/gateway_candidate" GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test -race ./internal/webgateway -run '^TestRustFullGatewayWithGoHome$$' -count=1 -timeout 2m

# Rust Home peer against the actual Go connector endpoint and hub.
rust-home-peer-compat:
	@hmux_peer_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_peer_dir"' EXIT HUP INT TERM; \
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test -race -c -o "$$hmux_peer_dir/go-gateway" ./internal/webgateway && \
	HMUX_GO_HOME_PEER_HELPER="$$hmux_peer_dir/go-gateway" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-home --test go_peer --locked -- --ignored

# Actual Go Home singleton versus the Rust lifetime lock, using private fixtures.
rust-home-lock-compat:
	@hmux_home_lock_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_home_lock_dir"' EXIT HUP INT TERM; \
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go build -race -o "$$hmux_home_lock_dir/go-home-lock" ./tools/hmux-home-lock-oracle && \
	HMUX_GO_HOME_LOCK_ORACLE="$$hmux_home_lock_dir/go-home-lock" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-home --test singleton --locked -- --ignored

# Current session state and transaction locks across Go/Rust process handoff.
rust-home-state-compat:
	@hmux_state_dir=$$(mktemp -d); trap 'rm -rf "$$hmux_state_dir"' EXIT HUP INT TERM; \
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test -race -c -o "$$hmux_state_dir/go-state" ./internal/sessionstate && \
	HMUX_GO_SESSIONSTATE_HELPER="$$hmux_state_dir/go-state" CARGO_HOME=$(CARGO_HOME) cargo test -p hmux-home --test go_sessionstate --locked -- --ignored

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
