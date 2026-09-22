GOCACHE ?= /tmp/hmux-go-cache
GOPATH ?= /tmp/hmux-go

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
	gofmt -w cmd internal
	gofmt -w third_party/token-terrier-server/cmd third_party/token-terrier-server/internal third_party/token-terrier-server/stream

fmt-check:
	test -z "$$(gofmt -l cmd internal)"
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

shfmt-check:
	GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go run mvdan.cc/sh/v3/cmd/shfmt@v3.13.1 -d scripts tests deploy/web

integration:
	python3 tests/home_install_test.py
	tests/codex_workflow_hooks.sh
	HMUX_RUN_TMUX_CREATE_TEST=1 GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test ./internal/agent -run 'TestCreateSessionWithIsolatedTmux|TestProvidersExitToShellWithIsolatedTmux' -count=1
	HMUX_RUN_WEB_TMUX_TEST=1 GOCACHE=$(GOCACHE) GOPATH=$(GOPATH) go test ./internal/home -run TestWebTerminalViewWithIsolatedTmux -count=1

check: fmt-check test race vet shellcheck web-check

build: web-build

clean:
	rm -rf dist
