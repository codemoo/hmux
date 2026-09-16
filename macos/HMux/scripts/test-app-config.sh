#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
APP_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd -P)
CONFIG="$APP_ROOT/HMuxGhostty.config"

[ -f "$CONFIG" ]
[ "$(grep -Fxc 'keybind = clear' "$CONFIG")" -eq 1 ]
[ "$(grep -Fxc 'keybind = cmd+q=quit' "$CONFIG")" -eq 1 ]

clear_line=$(grep -Fn 'keybind = clear' "$CONFIG" | cut -d: -f1)
quit_line=$(grep -Fn 'keybind = cmd+q=quit' "$CONFIG" | cut -d: -f1)
[ "$quit_line" -gt "$clear_line" ] || {
	echo "HMux Cmd-Q binding must be restored after keybind = clear" >&2
	exit 1
}

printf 'app-config-ok\n'
