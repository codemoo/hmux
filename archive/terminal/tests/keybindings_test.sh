#!/bin/sh
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/../../.." && pwd)
GHOSTTY_CONFIG="$ROOT/archive/terminal/config/ghostty.ghostty"
TARGET_CONFIG="$ROOT/archive/terminal/config/tmux.conf"
FRAME_CONFIG="$ROOT/archive/terminal/config/frame.tmux.conf"
FRAME_UI_CONFIG="$ROOT/archive/terminal/config/frame-ui.tmux.conf"
PROXY_SOURCE="$ROOT/archive/terminal/frame/proxy.go"

# The target server fragment is retired and must contain no tmux command,
# hmux option, status decoration or binding.
if grep -Eq '^[[:space:]]*(set|bind|unbind|source|run|display|if)-' "$TARGET_CONFIG"; then
	echo "retired target tmux config still contains executable commands" >&2
	exit 1
fi
if grep -Eq '@hmux_|status-format|pane-border-format' "$TARGET_CONFIG"; then
	echo "retired target tmux config still contains hmux server state" >&2
	exit 1
fi

test "$(grep -Fxc 'keybind = cmd+l=text:\x1b[5;9010~' "$GHOSTTY_CONFIG")" -eq 1
test "$(grep -Fxc 'keybind = cmd+grave_accent=text:\x1b[5;9010~' "$GHOSTTY_CONFIG")" -eq 1
test "$(grep -Fxc 'keybind = cmd+w=text:\x1b[5;9011~' "$GHOSTTY_CONFIG")" -eq 1
test "$(grep -Fxc 'keybind = cmd+q=text:\x1b[5;9012~' "$GHOSTTY_CONFIG")" -eq 1
test "$(grep -Fxc 'keybind = cmd+r=text:\x1b[5;9013~' "$GHOSTTY_CONFIG")" -eq 1
test "$(grep -Fxc 'theme = "Flexoki Dark"' "$GHOSTTY_CONFIG")" -eq 1
test "$(grep -Fxc 'window-theme = dark' "$GHOSTTY_CONFIG")" -eq 1
test "$(grep -Fxc 'link-url = true' "$GHOSTTY_CONFIG")" -eq 1
test "$(grep -Fxc 'link-previews = true' "$GHOSTTY_CONFIG")" -eq 1

for number in 1 2 3 4 5 6 7 8 9; do
	sequence=$((9020 + number))
	test "$(grep -Fxc "keybind = cmd+digit_$number=text:\\x1b[5;$sequence~" "$GHOSTTY_CONFIG")" -eq 1
done

grep -Fqx 'set -g prefix None' "$FRAME_CONFIG"
grep -Fqx 'set -g prefix2 None' "$FRAME_CONFIG"
grep -Fqx 'set -g mouse on' "$FRAME_CONFIG"
grep -Fqx 'set -g set-clipboard external' "$FRAME_CONFIG"
grep -Fqx 'set -as terminal-features ",xterm-ghostty:RGB:clipboard"' "$FRAME_CONFIG"
grep -Fqx 'set -g status-position top' "$FRAME_CONFIG"
grep -Fqx 'set -g status on' "$FRAME_CONFIG"
grep -Fq 'status-format[0]' "$FRAME_CONFIG"
grep -Fq '#[range=user|tab1]#{E:@hmux_frame_tab1}#[norange]' "$FRAME_CONFIG"
awk '
  /@hmux_frame_tab[1-9] / {
    value=$0
    sub(/^[^"]*"/, "", value)
    sub(/".*/, "", value)
    expected = ($3 == "@hmux_frame_tab1" ? 22 : 23)
    if (length(value) != expected) exit 1
    seen++
  }
  END { if (seen != 9) exit 1 }
' "$FRAME_CONFIG"
grep -Fq '#[range=user|list]' "$FRAME_CONFIG"
grep -Fq 'MouseDown1Status' "$FRAME_CONFIG"
grep -Fq 'frame-click --launcher' "$FRAME_CONFIG"
grep -Fqx 'set -g prefix None' "$FRAME_UI_CONFIG"
grep -Fqx 'set -g prefix2 None' "$FRAME_UI_CONFIG"
grep -Fqx 'set -g status off' "$FRAME_UI_CONFIG"
grep -Fqx 'set -g mouse on' "$FRAME_UI_CONFIG"
if grep -Eq '^[[:space:]]*set -s user-keys' "$FRAME_CONFIG" "$FRAME_UI_CONFIG" ||
	[ "$(grep -Ec '^[[:space:]]*bind-key' "$FRAME_CONFIG")" -ne 16 ] ||
	[ "$(grep -Ec '^[[:space:]]*bind-key' "$FRAME_UI_CONFIG")" -ne 14 ] ||
	[ "$(grep -Ec '^bind-key -n (Mouse(Down|Up)[123]Pane|Mouse(Drag|DragEnd)[23]Pane|Wheel(Up|Down)Pane) send-keys -M$' "$FRAME_CONFIG")" -ne 12 ] ||
	[ "$(grep -Ec '^bind-key -n (Mouse(Down|Up|Drag|DragEnd)[123]Pane|Wheel(Up|Down)Pane) send-keys -M$' "$FRAME_UI_CONFIG")" -ne 14 ]; then
	echo "outer header click and body mouse pass-through bindings are incomplete" >&2
	exit 1
fi
grep -Fqx 'bind-key -n MouseDrag1Pane copy-mode -M' "$FRAME_CONFIG"
grep -Fqx 'bind-key -T copy-mode MouseDragEnd1Pane send-keys -X copy-selection-and-cancel \; send-keys -M' "$FRAME_CONFIG"
grep -Fqx 'bind-key -T copy-mode-vi MouseDragEnd1Pane send-keys -X copy-selection-and-cancel \; send-keys -M' "$FRAME_CONFIG"
for sequence in 9010 9011 9012 9013 9021 9022 9023 9024 9025 9026 9027 9028 9029; do
	grep -Fq "\\x1b[5;$sequence~" "$PROXY_SOURCE"
done
if grep -Fq 'io.Copy(os.Stdout' "$PROXY_SOURCE"; then
	echo "target output must share the proxy event loop with final screen cleanup" >&2
	exit 1
fi

if grep -Eq 'kill-(session|window|pane|server)|detach-client|switch-client' \
	"$FRAME_CONFIG" "$FRAME_UI_CONFIG"; then
	echo "outer key config may only call the validated hmux frame helper" >&2
	exit 1
fi

if command -v tmux >/dev/null 2>&1; then
	test_dir=$(mktemp -d "${TMPDIR:-/tmp}/hmux-e2e-keybinding.XXXXXX")
	socket="$test_dir/tmux.sock"
	cleanup() {
		tmux -S "$socket" kill-server >/dev/null 2>&1 || :
		test ! -S "$socket" || unlink "$socket"
		rm -f "$test_dir/options"
		rmdir "$test_dir"
	}
	trap cleanup EXIT HUP INT TERM
	launcher=0123456789abcdef0123456789abcdef
	HMUX_FRAME_LAUNCHER="$launcher" tmux -S "$socket" -f "$FRAME_CONFIG" \
		start-server \; \
		show-options -gv status >"$test_dir/options"
	test "$(sed -n '1p' "$test_dir/options")" = "on"
	cleanup
	trap - EXIT HUP INT TERM
fi
