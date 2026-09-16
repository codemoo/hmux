#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
APP_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd -P)
OUTPUT="$APP_ROOT/.build/hmux-backend-lifecycle-smoke"
MODULE_CACHE="$APP_ROOT/.build/swift-module-cache"
TEST_DIR=$(mktemp -d "${TMPDIR:-/tmp}/hmux-backend-lifecycle.XXXXXX")
trap 'rm -rf "$TEST_DIR"' EXIT HUP INT TERM
HELPER="$TEST_DIR/helper"
PID_FILE="$TEST_DIR/pid"

cat >"$HELPER" <<'SH'
#!/bin/sh
exec /usr/bin/perl -e '
  $SIG{TERM} = "IGNORE";
  open my $fh, ">", $ENV{HMUX_TEST_PID_FILE} or die "pid file";
  print {$fh} "$$\n";
  close $fh;
  while (1) { select undef, undef, undef, 1; }
'
SH
chmod 700 "$HELPER"
mkdir -p "$APP_ROOT/.build" "$MODULE_CACHE"

swiftc -parse-as-library -D DEBUG \
	-module-cache-path "$MODULE_CACHE" \
	"$APP_ROOT/Overlay/Sources/HMux/HMuxModels.swift" \
	"$APP_ROOT/Overlay/Sources/HMux/HMuxBackend.swift" \
    "$APP_ROOT/Overlay/Sources/HMux/HMuxWorkspaceState.swift" \
    "$APP_ROOT/Overlay/Sources/HMux/HMuxConversation.swift" \
	"$APP_ROOT/Overlay/Sources/HMux/HMuxCatalogStream.swift" \
	"$APP_ROOT/Tests/HMuxBackendLifecycleSmoke.swift" \
	-o "$OUTPUT"
"$OUTPUT" "$HELPER" "$PID_FILE"
