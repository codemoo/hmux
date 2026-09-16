#!/bin/sh
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TEST_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/hmux-e2e-live-cleanup.XXXXXX")
HOME_DIR="$TEST_ROOT/home"
TARGET_SOCKET="$TEST_ROOT/target.sock"
AGENT_BIN="$TEST_ROOT/hmux-agent"

cleanup() {
	tmux -S "$TARGET_SOCKET" kill-server >/dev/null 2>&1 || :
	test ! -S "$TARGET_SOCKET" || unlink "$TARGET_SOCKET"
	chmod -R u+w "$TEST_ROOT" 2>/dev/null || :
	rm -rf "$TEST_ROOT"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$HOME_DIR/.config/hmux"
printf '%s\n' \
	'set -g status-position bottom' \
	'set -g status-left "native-left"' \
	'source-file ~/.config/hmux/tmux.conf' \
	>"$HOME_DIR/.tmux.conf"
printf '%s\n' \
	'set -g status-position top' \
	'set -g status 2' \
	'set -g status-left "hmux legacy status"' \
	'bind-key L run-shell "hmux legacy binding"' \
	>"$HOME_DIR/.config/hmux/tmux.conf"

GOCACHE="${GOCACHE:-${TMPDIR:-/tmp}/hmux-go-cache}" \
	GOPATH="${GOPATH:-${TMPDIR:-/tmp}/hmux-go}" \
	go build -trimpath -o "$AGENT_BIN" "$ROOT/cmd/hmux-agent"

HOME="$HOME_DIR" tmux -S "$TARGET_SOCKET" -f "$HOME_DIR/.tmux.conf" \
	new-session -d -s hmux-e2e-live-cleanup
HOME="$HOME_DIR" tmux -S "$TARGET_SOCKET" set-option -t hmux-e2e-live-cleanup \
	@hmux_alias "friendly"
before_sessions=$(HOME="$HOME_DIR" tmux -S "$TARGET_SOCKET" list-sessions -F '#{session_id}')
before_windows=$(HOME="$HOME_DIR" tmux -S "$TARGET_SOCKET" list-windows -a -F '#{window_id}')
before_panes=$(HOME="$HOME_DIR" tmux -S "$TARGET_SOCKET" list-panes -a -F '#{pane_id}')
target_pid=$(HOME="$HOME_DIR" tmux -S "$TARGET_SOCKET" display-message -p '#{pid}')

HOME="$HOME_DIR" \
	TMUX="$TARGET_SOCKET,$target_pid,0" \
	HMUX_AGENT_BIN="$AGENT_BIN" \
	HMUX_UI_SKIP_GHOSTTY=1 \
	"$ROOT/scripts/clean-live-target-tmux.sh" >"$TEST_ROOT/cleanup.log"

test "$(HOME="$HOME_DIR" tmux -S "$TARGET_SOCKET" show-options -gv status-position)" = "bottom"
test "$(HOME="$HOME_DIR" tmux -S "$TARGET_SOCKET" show-options -gv status)" = "on"
test "$(HOME="$HOME_DIR" tmux -S "$TARGET_SOCKET" show-options -gv status-left)" = "native-left"
HOME="$HOME_DIR" tmux -S "$TARGET_SOCKET" list-keys -T prefix L |
	grep -Fqx 'bind-key -T prefix L switch-client -l'
test -z "$(HOME="$HOME_DIR" tmux -S "$TARGET_SOCKET" show-options -qv -t hmux-e2e-live-cleanup @hmux_alias)"
test "$(HOME="$HOME_DIR" tmux -S "$TARGET_SOCKET" list-sessions -F '#{session_id}')" = "$before_sessions"
test "$(HOME="$HOME_DIR" tmux -S "$TARGET_SOCKET" list-windows -a -F '#{window_id}')" = "$before_windows"
test "$(HOME="$HOME_DIR" tmux -S "$TARGET_SOCKET" list-panes -a -F '#{pane_id}')" = "$before_panes"
test ! -e "$HOME_DIR/.config/hmux/tmux.conf"
if grep -Fq 'source-file ~/.config/hmux/tmux.conf' "$HOME_DIR/.tmux.conf"; then
	exit 1
fi
grep -Fq '"alias":"friendly"' "$HOME_DIR/.local/state/hmux/sessions/sessions.json"

cleanup
trap - EXIT HUP INT TERM
