#!/bin/sh
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/../../.." && pwd)
TARGET_CONFIG="$ROOT/archive/terminal/config/tmux.conf"
FRAME_CONFIG="$ROOT/archive/terminal/config/frame.tmux.conf"
FRAME_UI_CONFIG="$ROOT/archive/terminal/config/frame-ui.tmux.conf"
GHOSTTY_CONFIG="$ROOT/archive/terminal/config/ghostty.ghostty"

command -v tmux >/dev/null 2>&1 || exit 0

test_dir=$(mktemp -d "${TMPDIR:-/tmp}/hmux-e2e-frame.XXXXXX")
target_socket="$test_dir/target.sock"
frame_socket="$test_dir/frame.sock"
frame_ui_socket="$test_dir/frame-ui.sock"
cleanup() {
	tmux -S "$target_socket" kill-server >/dev/null 2>&1 || :
	tmux -S "$frame_socket" kill-server >/dev/null 2>&1 || :
	tmux -S "$frame_ui_socket" kill-server >/dev/null 2>&1 || :
	test ! -S "$target_socket" || unlink "$target_socket"
	test ! -S "$frame_socket" || unlink "$frame_socket"
	test ! -S "$frame_ui_socket" || unlink "$frame_ui_socket"
	rmdir "$test_dir"
}
trap cleanup EXIT HUP INT TERM

# Sourcing the retired compatibility file is a strict no-op on an isolated
# disposable target server.
tmux -S "$target_socket" -f /dev/null new-session -d -s hmux-e2e-frame-render
before_options=$(tmux -S "$target_socket" show-options -g)
before_window_options=$(tmux -S "$target_socket" show-window-options -g)
before_keys=$(tmux -S "$target_socket" list-keys)
before_ids=$(tmux -S "$target_socket" list-sessions -F '#{session_id}')
before_panes=$(tmux -S "$target_socket" list-panes -a -F '#{pane_id}')

tmux -S "$target_socket" source-file "$TARGET_CONFIG"

test "$(tmux -S "$target_socket" show-options -g)" = "$before_options"
test "$(tmux -S "$target_socket" show-window-options -g)" = "$before_window_options"
test "$(tmux -S "$target_socket" list-keys)" = "$before_keys"
test "$(tmux -S "$target_socket" list-sessions -F '#{session_id}')" = "$before_ids"
test "$(tmux -S "$target_socket" list-panes -a -F '#{pane_id}')" = "$before_panes"

grep -Fqx 'window-padding-x = 10' "$GHOSTTY_CONFIG"
grep -Fqx 'window-padding-y = 8' "$GHOSTTY_CONFIG"
grep -Fqx 'window-padding-balance = true' "$GHOSTTY_CONFIG"
grep -Fqx 'window-padding-color = background' "$GHOSTTY_CONFIG"
grep -Fqx 'link-url = true' "$GHOSTTY_CONFIG"
grep -Fqx 'link-previews = true' "$GHOSTTY_CONFIG"

launcher=0123456789abcdef0123456789abcdef
frame_options="$(
	HMUX_FRAME_LAUNCHER="$launcher" tmux -S "$frame_socket" -f "$FRAME_CONFIG" \
		start-server \; \
		show-options -gv prefix \; \
		show-options -gv prefix2 \; \
		show-options -gv status \; \
		show-options -gv mouse
)"
test "$frame_options" = "None
None
on
on"

header=$(
	HMUX_FRAME_LAUNCHER="$launcher" tmux -S "$frame_socket" -f "$FRAME_CONFIG" \
		start-server \; show-options -gv 'status-format[0]'
)
case "$header" in
*'range=user|list'*'HMUX'*'TABS'*'%b %d %I:%M %p'*) ;;
*)
	echo "outer frame header or top tabs are incomplete" >&2
	exit 1
	;;
esac

test "$(
	HMUX_FRAME_LAUNCHER="$launcher" tmux -S "$frame_ui_socket" -f "$FRAME_UI_CONFIG" \
		start-server \; show-options -gv status
)" = "off"

if grep -Eq '(^|[[:space:]])(split-window|new-window|join-pane|move-pane|break-pane|kill-(pane|window|session|server)|detach-client)([[:space:]]|$)' \
	"$TARGET_CONFIG" "$FRAME_CONFIG" "$FRAME_UI_CONFIG"; then
	echo "hmux frame config contains a forbidden target mutation" >&2
	exit 1
fi

cleanup
trap - EXIT HUP INT TERM
