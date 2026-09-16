VERSION ?= $(shell sed -n '1p' VERSION)
GOCACHE ?= /tmp/hmux-go-cache
GOPATH ?= /tmp/hmux-go
GOFLAGS := -trimpath
LDFLAGS := -s -w -X main.version=$(VERSION)

.PHONY: legacy-check all fmt fmt-check test race vet shellcheck shfmt-check integration native-smoke check build clean web-check web-build

web-check:
	npm ci --prefix web
	npm run check --prefix web
	npm test --prefix web
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test -race ./internal/webgateway ./internal/client ./cmd/hmux-web

web-build:
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) deploy/web/build.sh

all: web-check web-build

fmt:
	gofmt -w cmd internal archive/terminal/ui archive/terminal/frame
	gofmt -w third_party/token-terrier-server/cmd third_party/token-terrier-server/internal third_party/token-terrier-server/stream

fmt-check:
	test -z "$$(gofmt -l cmd internal archive/terminal/ui archive/terminal/frame)"
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
	shellcheck -x scripts/*.sh tests/*.sh archive/terminal/scripts/*.sh archive/terminal/tests/*.sh scripts/hmux-bootstrap macos/HMux/scripts/*.sh

shfmt-check:
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go run mvdan.cc/sh/v3/cmd/shfmt@v3.13.1 -d scripts tests archive/terminal/scripts archive/terminal/tests

integration:
	tests/bootstrap_first_run.sh
	tests/bootstrap_source_runtime.sh
	tests/codex_workflow_hooks.sh
	tests/external_bootstrap_flow.sh
	tests/external_preflight.sh
	archive/terminal/tests/font_setup.sh
	tests/e2e_handoff.sh
	HMUX_RUN_TMUX_CREATE_TEST=1 GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test ./internal/agent -run TestCreateSessionWithIsolatedTmux -count=1
	archive/terminal/tests/frame_render_test.sh
	HMUX_RUN_TMUX_RESIZE_TEST=1 GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test ./archive/terminal/frame -run TestFramedPaneLayoutTracksClientResize -count=1
	archive/terminal/tests/frame_drag_copy.sh
	archive/terminal/tests/frame_pty_e2e.sh
	archive/terminal/tests/launcher_transition_test.sh
	archive/terminal/tests/keybindings_test.sh
	tests/live_target_cleanup.sh
	archive/terminal/tests/selector_input_focus.sh
	archive/terminal/tests/shell_entrypoint.sh
	archive/terminal/tests/ui_config_sync.sh
	archive/terminal/tests/zsh_autostart.sh
	tests/uninstall_test.sh

native-smoke:
	macos/HMux/scripts/test-app-config.sh
	macos/HMux/scripts/test-models.sh
	macos/HMux/scripts/test-catalog-mutations.sh
	macos/HMux/scripts/test-catalog-store.sh
	macos/HMux/scripts/test-workspace-state.sh
	sh macos/HMux/scripts/test-shared-workspace.sh
	macos/HMux/scripts/test-surface-deck.sh
	macos/HMux/scripts/test-surface-input.sh
	macos/HMux/scripts/test-search-field.sh
	macos/HMux/scripts/test-conversation.sh
	macos/HMux/scripts/test-catalog-stream.sh
	macos/HMux/scripts/test-usage-models.sh
	macos/HMux/scripts/test-host-metrics.sh
	macos/HMux/scripts/test-file-stage.sh
	macos/HMux/scripts/test-backend-lifecycle.sh
	macos/HMux/scripts/test-restart-readiness.sh

check: fmt-check test race vet web-check

legacy-check: shellcheck shfmt-check integration

build:
	mkdir -p dist/darwin-arm64 dist/darwin-amd64 dist/linux-amd64 dist/linux-arm64
	GOOS=darwin GOARCH=arm64 GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go build $(GOFLAGS) -ldflags "$(LDFLAGS)" -o dist/darwin-arm64/hmux ./cmd/hmux
	GOOS=darwin GOARCH=arm64 GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go build $(GOFLAGS) -ldflags "$(LDFLAGS)" -o dist/darwin-arm64/hmux-agent ./cmd/hmux-agent
	GOOS=darwin GOARCH=arm64 GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go build $(GOFLAGS) -ldflags "$(LDFLAGS)" -o dist/darwin-arm64/hmux-control ./cmd/hmux-control
	GOOS=darwin GOARCH=amd64 GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go build $(GOFLAGS) -ldflags "$(LDFLAGS)" -o dist/darwin-amd64/hmux ./cmd/hmux
	GOOS=darwin GOARCH=amd64 GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go build $(GOFLAGS) -ldflags "$(LDFLAGS)" -o dist/darwin-amd64/hmux-agent ./cmd/hmux-agent
	GOOS=darwin GOARCH=amd64 GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go build $(GOFLAGS) -ldflags "$(LDFLAGS)" -o dist/darwin-amd64/hmux-control ./cmd/hmux-control
	GOOS=linux GOARCH=amd64 GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go build $(GOFLAGS) -ldflags "$(LDFLAGS)" -o dist/linux-amd64/hmux-control ./cmd/hmux-control
	GOOS=linux GOARCH=arm64 GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go build $(GOFLAGS) -ldflags "$(LDFLAGS)" -o dist/linux-arm64/hmux-control ./cmd/hmux-control

clean:
	rm -rf dist
