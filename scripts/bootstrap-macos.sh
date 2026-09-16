#!/bin/sh
set -eu

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
SOURCE_VERSION_FILE="$ROOT/VERSION"
SOURCE_VERSION="$(sed -n '1p' "$SOURCE_VERSION_FILE")"
case "$SOURCE_VERSION" in
[0-9]*.[0-9]*.[0-9]*) ;;
*)
	echo "invalid source VERSION" >&2
	exit 1
	;;
esac
case "$SOURCE_VERSION" in *[!0-9.]* | *.*.*.*)
	echo "invalid source VERSION" >&2
	exit 1
	;;
esac
STAMP="$(date -u +%Y%m%dT%H%M%SZ)-$$"
BIN_DIR="$HOME/.local/bin"
CACHE_DIR="$HOME/.cache/hmux"
CONFIG_DIR="$HOME/.config/hmux"
BACKUP_DIR="$CONFIG_DIR/backups/$STAMP"
mkdir -p "$BIN_DIR" "$CACHE_DIR/releases/local" "$CONFIG_DIR" "$BACKUP_DIR"
chmod 700 "$BIN_DIR" "$CACHE_DIR" "$CONFIG_DIR" "$BACKUP_DIR"

for path in \
	"$BIN_DIR/hmux" \
	"$BIN_DIR/hmux-agent" \
	"$BIN_DIR/hmux-control" \
	"$HOME/.ssh/config"; do
	[ -e "$path" ] || continue
	cp -p "$path" "$BACKUP_DIR/$(basename "$path")"
done
if [ -e "$CACHE_DIR/releases/local/hmux" ]; then
	cp -p "$CACHE_DIR/releases/local/hmux" "$BACKUP_DIR/hmux-local-runtime"
fi

GOCACHE="${GOCACHE:-${TMPDIR:-/tmp}/hmux-go-cache}"
GOPATH="${GOPATH:-${TMPDIR:-/tmp}/hmux-go}"
export GOCACHE GOPATH
BUILD_DIR="$(mktemp -d "$CACHE_DIR/.source-build.XXXXXX")"
trap 'rm -rf "$BUILD_DIR"' EXIT HUP INT TERM
go build -trimpath -ldflags "-s -w -X main.version=$SOURCE_VERSION" -o "$BUILD_DIR/hmux" "$ROOT/cmd/hmux"
go build -trimpath -ldflags "-s -w -X main.version=$SOURCE_VERSION" -o "$BUILD_DIR/hmux-agent" "$ROOT/cmd/hmux-agent"
go build -trimpath -ldflags "-s -w -X main.version=$SOURCE_VERSION" -o "$BUILD_DIR/hmux-control" "$ROOT/cmd/hmux-control"
"$BUILD_DIR/hmux-agent" version | grep -Fqx "hmux-agent $SOURCE_VERSION protocol=1"
"$BUILD_DIR/hmux-control" version | grep -Fqx "hmux-control $SOURCE_VERSION schema=1"
install -m 700 "$BUILD_DIR/hmux" "$CACHE_DIR/releases/local/.hmux.next"
mv "$CACHE_DIR/releases/local/.hmux.next" "$CACHE_DIR/releases/local/hmux"
install -m 700 "$BUILD_DIR/hmux-agent" "$BIN_DIR/.hmux-agent.next"
mv "$BIN_DIR/.hmux-agent.next" "$BIN_DIR/hmux-agent"
install -m 700 "$BUILD_DIR/hmux-control" "$BIN_DIR/.hmux-control.next"
mv "$BIN_DIR/.hmux-control.next" "$BIN_DIR/hmux-control"
install -m 700 "$ROOT/scripts/hmux-bootstrap" "$BIN_DIR/hmux"
ln -sfn "releases/local/hmux" "$CACHE_DIR/current.next"
mv -f "$CACHE_DIR/current.next" "$CACHE_DIR/current"
rm -rf "$BUILD_DIR"
trap - EXIT HUP INT TERM

mkdir -p "$HOME/.ssh/config.d"
chmod 700 "$HOME/.ssh" "$HOME/.ssh/config.d"
touch "$HOME/.ssh/config"
chmod 600 "$HOME/.ssh/config"
if ! grep -Eq '^[[:space:]]*Include[[:space:]].*config\\.d' "$HOME/.ssh/config"; then
	tmp="$(mktemp "$HOME/.ssh/.config.hmux.XXXXXX")"
	trap 'rm -f "$tmp"' EXIT HUP INT TERM
	{
		echo 'Include ~/.ssh/config.d/*.conf'
		cat "$HOME/.ssh/config"
	} >"$tmp"
	chmod 600 "$tmp"
	ssh -G -F "$tmp" localhost >/dev/null
	mv "$tmp" "$HOME/.ssh/config"
	trap - EXIT HUP INT TERM
fi

# The host/runtime install does not opt users into the archived terminal UI.
if [ "${HMUX_INSTALL_LEGACY_UI:-0}" = 1 ]; then
	"$ROOT/archive/terminal/scripts/sync-ui-config-macos.sh"
fi

echo "hmux installed; backups: $BACKUP_DIR"
echo "host runtime installed; web connector setup is documented in docs/WEB.md"
