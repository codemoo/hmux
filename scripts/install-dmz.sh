#!/bin/sh
set -eu

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
PREFIX="${HMUX_CONTROL_ROOT:-$HOME/.local/share/hmux-control}"
BIN_DIR="$HOME/.local/bin"
UNIT_DIR="$HOME/.config/systemd/user"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)-$$"
for dir in "$PREFIX" "$PREFIX/inventory" "$PREFIX/releases" \
	"$PREFIX/rendered" "$PREFIX/state" "$PREFIX/history"; do
	if [ -L "$dir" ] || { [ -e "$dir" ] && [ ! -d "$dir" ]; }; then
		echo "unsafe DMZ control directory: $dir" >&2
		exit 1
	fi
done
mkdir -p "$BIN_DIR" "$PREFIX/inventory" "$PREFIX/releases" "$PREFIX/rendered" "$PREFIX/state" "$PREFIX/history" "$UNIT_DIR"
for dir in "$PREFIX" "$PREFIX/inventory" "$PREFIX/releases" \
	"$PREFIX/rendered" "$PREFIX/state" "$PREFIX/history"; do
	[ "$(stat -c '%u' "$dir")" = "$(id -u)" ] || {
		echo "DMZ control directory is not owned by the current user: $dir" >&2
		exit 1
	}
	chmod 700 "$dir"
done
for dir in "$PREFIX/rendered/ssh" "$PREFIX/rendered/termius"; do
	[ -e "$dir" ] || continue
	[ ! -L "$dir" ] && [ -d "$dir" ] &&
		[ "$(stat -c '%u' "$dir")" = "$(id -u)" ] || {
		echo "unsafe rendered directory: $dir" >&2
		exit 1
	}
	chmod 700 "$dir"
done

case "$(uname -m)" in
x86_64) PLATFORM="linux-amd64" ;;
aarch64 | arm64) PLATFORM="linux-arm64" ;;
*)
	echo "unsupported DMZ architecture" >&2
	exit 1
	;;
esac
CONTROL_BINARY="$ROOT/dist/$PLATFORM/hmux-control"
[ -x "$CONTROL_BINARY" ] || {
	echo "missing control binary: dist/$PLATFORM/hmux-control" >&2
	exit 1
}
if [ -e "$BIN_DIR/hmux-control" ]; then
	cp -p "$BIN_DIR/hmux-control" "$BIN_DIR/hmux-control.hmux-backup-$STAMP"
fi
install -m 700 "$CONTROL_BINARY" "$BIN_DIR/hmux-control"
NEW_INVENTORY=false
if [ ! -e "$PREFIX/inventory/inventory.toml" ]; then
	install -m 600 "$ROOT/config/inventory.example.toml" "$PREFIX/inventory/inventory.toml"
	NEW_INVENTORY=true
	echo "edit $PREFIX/inventory/inventory.toml before reconcile" >&2
fi
for unit in "$ROOT"/systemd/*; do
	target="$UNIT_DIR/$(basename "$unit")"
	if [ -e "$target" ]; then
		cp -p "$target" "$target.hmux-backup-$STAMP"
	fi
	install -m 600 "$unit" "$UNIT_DIR/$(basename "$unit")"
done
systemctl --user daemon-reload
if [ "$NEW_INVENTORY" = true ]; then
	echo "systemd timers were not enabled because the new inventory still contains placeholders" >&2
	exit 2
fi
"$BIN_DIR/hmux-control" validate
"$BIN_DIR/hmux-control" reconcile
systemctl --user enable --now hmux-reconcile.timer hmux-health.timer
echo "hmux DMZ control plane installed at $PREFIX"
