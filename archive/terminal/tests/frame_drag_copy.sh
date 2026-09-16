#!/bin/sh
set -eu

if [ "${1:-}" = "--mouse-helper" ]; then
	[ "$#" -eq 3 ] || exit 2
	raw_mouse=$2
	ready=$3
	stty raw -echo
	printf '\033[?1000h\033[?1006h\033[2J\033[HCOPYME-DRAG'
	: >"$ready"
	dd bs=1 count=18 of="$raw_mouse" 2>/dev/null
	printf '\033[?1000l\033[?1006l'
	exec sleep 30
fi

command -v tmux >/dev/null 2>&1 || exit 0
command -v expect >/dev/null 2>&1 || exit 0

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/../../.." && pwd)
FRAME_CONFIG=${HMUX_E2E_FRAME_CONFIG:-"$ROOT/archive/terminal/config/frame.tmux.conf"}
[ ! -L "$FRAME_CONFIG" ] && [ -f "$FRAME_CONFIG" ] || {
	echo "HMUX_E2E_FRAME_CONFIG must name a regular non-symlink file" >&2
	exit 2
}
TEST_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/hmux-e2e-frame-drag.XXXXXX")
SOCKET="$TEST_ROOT/frame.sock"
RAW_MOUSE="$TEST_ROOT/raw-mouse"
READY="$TEST_ROOT/ready"
REAL_TMUX=$(command -v tmux)
DONE="$TEST_ROOT/done"
EXPECT_PID=""

cleanup() {
	if [ -n "$EXPECT_PID" ]; then
		kill "$EXPECT_PID" >/dev/null 2>&1 || :
		wait "$EXPECT_PID" >/dev/null 2>&1 || :
	fi
	"$REAL_TMUX" -S "$SOCKET" kill-server >/dev/null 2>&1 || :
	find "$TEST_ROOT" -type s -delete 2>/dev/null || :
	chmod -R u+w "$TEST_ROOT" 2>/dev/null || :
	rm -rf "$TEST_ROOT"
}
trap cleanup EXIT HUP INT TERM

HMUX_TEST_SOCKET="$SOCKET" \
	HMUX_TEST_RAW_MOUSE="$RAW_MOUSE" \
	HMUX_TEST_READY="$READY" \
	HMUX_TEST_DONE="$DONE" \
	HMUX_TEST_MOUSE_APP="$ROOT/archive/terminal/tests/frame_drag_copy.sh" \
	HMUX_TEST_TMUX="$REAL_TMUX" \
	HMUX_TEST_FRAME_CONFIG="$FRAME_CONFIG" \
	TERM=xterm-ghostty \
	expect <<'EOF' &
set timeout 10
log_user 0

spawn -noecho $env(HMUX_TEST_TMUX) -S $env(HMUX_TEST_SOCKET) \
  -f $env(HMUX_TEST_FRAME_CONFIG) new-session -s hmux-e2e-frame-drag \
  $env(HMUX_TEST_MOUSE_APP) --mouse-helper $env(HMUX_TEST_RAW_MOUSE) \
  $env(HMUX_TEST_READY)

set ready 0
for {set attempt 0} {$attempt < 200} {incr attempt} {
  if {[file exists $env(HMUX_TEST_READY)]} {
    set ready 1
    break
  }
  after 25
}
if {!$ready} {
  exit 120
}

# The outer status occupies physical row 1; pane text starts on row 2.
send -- "\033\[<0;1;2M"
send -- "\033\[<32;8;2M"
send -- "\033\[<0;8;2m"

set captured 0
for {set attempt 0} {$attempt < 200} {incr attempt} {
  if {[file exists $env(HMUX_TEST_RAW_MOUSE)] &&
      [file size $env(HMUX_TEST_RAW_MOUSE)] == 18} {
    set captured 1
    break
  }
  after 25
}
if {!$captured} {
  exit 121
}
for {set attempt 0} {$attempt < 400} {incr attempt} {
  if {[file exists $env(HMUX_TEST_DONE)]} {
    close
    catch wait
    exit 0
  }
  after 25
}
exit 126
EOF
EXPECT_PID=$!

attempt=0
while [ "$attempt" -lt 200 ]; do
	if [ -f "$RAW_MOUSE" ] && [ "$(wc -c <"$RAW_MOUSE" | tr -d ' ')" -eq 18 ]; then
		break
	fi
	attempt=$((attempt + 1))
	sleep 0.025
done
if [ "$attempt" -eq 200 ]; then
	echo "outer frame did not forward a complete mouse down/up pair" >&2
	exit 1
fi
sleep 0.1

copied=$("$REAL_TMUX" -S "$SOCKET" show-buffer)
if [ "$copied" != "COPYME-" ]; then
	echo "outer frame copied an unexpected selection: $copied" >&2
	exit 1
fi

pane_mode=$("$REAL_TMUX" -S "$SOCKET" display-message -p \
	-t hmux-e2e-frame-drag '#{pane_in_mode}')
if [ "$pane_mode" != "0" ]; then
	echo "outer frame remained in copy mode after drag release" >&2
	exit 1
fi

raw_hex=$(od -An -tx1 "$RAW_MOUSE" | tr -d '[:space:]')
if [ "$raw_hex" != "1b5b3c303b313b314d1b5b3c303b383b316d" ]; then
	echo "nested mouse app did not receive the expected down/up pair" >&2
	exit 1
fi

features=$("$REAL_TMUX" -S "$SOCKET" list-clients -F '#{client_termfeatures}')
case "$features" in
*clipboard*) ;;
*)
	echo "outer frame client does not advertise OSC 52 clipboard support" >&2
	exit 1
	;;
esac

: >"$DONE"
wait "$EXPECT_PID"
EXPECT_PID=""

cleanup
trap - EXIT HUP INT TERM
