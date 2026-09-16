#!/bin/sh
set -eu

case "$(uname -s)" in Darwin) ;; *) exit 0 ;; esac

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/hmux-source-runtime.XXXXXX")"
trap 'chmod -R u+w "$TEST_ROOT" 2>/dev/null || true; rm -rf "$TEST_ROOT"' EXIT HUP INT TERM
HOME_DIR="$TEST_ROOT/home"
VERSION="$(sed -n '1p' "$ROOT/VERSION")"
mkdir -p "$HOME_DIR/.config/hmux"
cat >"$HOME_DIR/.config/hmux/client.toml" <<EOF
schema_version = 1
client_id = "home-mac"
role = "home"
cache_dir = "$HOME_DIR/.cache/hmux"
state_dir = "$HOME_DIR/.local/state/hmux"
timeout_seconds = 5
EOF
chmod 600 "$HOME_DIR/.config/hmux/client.toml"

HOME="$HOME_DIR" "$ROOT/scripts/bootstrap-macos.sh" >/dev/null

test "$(readlink "$HOME_DIR/.cache/hmux/current")" = "releases/local/hmux"
test "$("$HOME_DIR/.cache/hmux/current" --no-update-check version 2>/dev/null |
	sed -n '1p')" = "hmux $VERSION protocol=1 platform=darwin-$(uname -m)"
test "$("$HOME_DIR/.local/bin/hmux-agent" version)" = \
	"hmux-agent $VERSION protocol=1"
test "$("$HOME_DIR/.local/bin/hmux-control" version)" = \
	"hmux-control $VERSION schema=1"
test -x "$HOME_DIR/.local/bin/hmux"
test ! -e "$HOME_DIR/.config/hmux/frame-ui.tmux.conf"
test ! -e "$HOME_DIR/.config/hmux/ghostty.ghostty"
