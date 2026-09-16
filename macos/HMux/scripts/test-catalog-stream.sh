#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
APP_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd -P)
REPO_ROOT=$(CDPATH='' cd -- "$APP_ROOT/../.." && pwd -P)
OUTPUT="$APP_ROOT/.build/hmux-catalog-stream-smoke"
MODULE_CACHE="$APP_ROOT/.build/swift-module-cache"
TEST_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/hmux-e2e-catalog-stream.XXXXXX")
trap 'rm -rf "$TEST_ROOT"' EXIT HUP INT TERM
mkdir -m 700 "$TEST_ROOT/bin" "$TEST_ROOT/state" "$TEST_ROOT/cache"
# Always exercise this checkout's bridge and a fake empty tmux catalog. The
# default check must not read user configuration or an installed app helper.
GOCACHE="${GOCACHE:-/tmp/hmux-go-cache}" GOPATH="${GOPATH:-/tmp/hmux-go}" \
	go build -o "$TEST_ROOT/hmux" "$REPO_ROOT/cmd/hmux"
cat >"$TEST_ROOT/client.toml" <<EOF
schema_version = 1
role = "home"
state_dir = "$TEST_ROOT/state"
cache_dir = "$TEST_ROOT/cache"
update_check = false
EOF
chmod 600 "$TEST_ROOT/client.toml"
cat >"$TEST_ROOT/bin/tmux" <<'SH'
#!/bin/sh
case "$1" in
list-sessions|list-windows) exit 0 ;;
*) echo "unexpected tmux action in catalog smoke" >&2; exit 1 ;;
esac
SH
cat >"$TEST_ROOT/helper" <<'SH'
#!/bin/sh
HMUX_SMOKE_ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
PATH="$HMUX_SMOKE_ROOT/bin:$PATH"
export PATH
exec "$HMUX_SMOKE_ROOT/hmux" --config "$HMUX_SMOKE_ROOT/client.toml" "$@"
SH
chmod 700 "$TEST_ROOT/helper" "$TEST_ROOT/bin/tmux"
HELPER="$TEST_ROOT/helper"
mkdir -p "$APP_ROOT/.build" "$MODULE_CACHE"

swiftc -parse-as-library -D DEBUG \
	-module-cache-path "$MODULE_CACHE" \
	"$APP_ROOT/Overlay/Sources/HMux/HMuxModels.swift" \
	"$APP_ROOT/Overlay/Sources/HMux/HMuxBackend.swift" \
    "$APP_ROOT/Overlay/Sources/HMux/HMuxWorkspaceState.swift" \
    "$APP_ROOT/Overlay/Sources/HMux/HMuxConversation.swift" \
	"$APP_ROOT/Overlay/Sources/HMux/HMuxCatalogStream.swift" \
	"$APP_ROOT/Tests/HMuxCatalogStreamSmoke.swift" \
	-o "$OUTPUT"
"$OUTPUT" "$HELPER"
