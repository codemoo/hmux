#!/bin/sh
set -eu

command -v tmux >/dev/null 2>&1 || exit 0
command -v expect >/dev/null 2>&1 || exit 0

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/../../.." && pwd)
TEST_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/hmux-e2e-frame-pty.XXXXXX")
HOME_DIR="$TEST_ROOT/home"
TARGET_SOCKET="$TEST_ROOT/target.sock"
BIN_DIR="$TEST_ROOT/bin"
TMUX_LOG="$TEST_ROOT/tmux.log"
REAL_TMUX=$(command -v tmux)
LAUNCHER=0123456789abcdef0123456789abcdef
OBSERVER_PID=""

cleanup() {
	if [ -n "$OBSERVER_PID" ]; then
		kill "$OBSERVER_PID" >/dev/null 2>&1 || :
		wait "$OBSERVER_PID" >/dev/null 2>&1 || :
	fi
	"$REAL_TMUX" -S "$TARGET_SOCKET" kill-server >/dev/null 2>&1 || :
	find "$TEST_ROOT" -type s -delete 2>/dev/null || :
	chmod -R u+w "$TEST_ROOT" 2>/dev/null || :
	rm -rf "$TEST_ROOT"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$HOME_DIR/.config/hmux" "$BIN_DIR"
if [ -n "${HMUX_E2E_AGENT_BIN:-}" ]; then
	[ -x "$HMUX_E2E_AGENT_BIN" ] || {
		echo "HMUX_E2E_AGENT_BIN must name an executable" >&2
		exit 1
	}
	# Run the exact installed bytes from the isolated test root. Keeping every
	# executable beneath that root also avoids host-specific application
	# execution policies from changing this disposable PTY test.
	AGENT_BIN="$TEST_ROOT/hmux-agent"
	install -m 700 "$HMUX_E2E_AGENT_BIN" "$AGENT_BIN"
	unset HMUX_E2E_AGENT_BIN
else
	AGENT_BIN="$TEST_ROOT/hmux-agent"
	GOCACHE="${GOCACHE:-${TMPDIR:-/tmp}/hmux-go-cache}" \
		GOPATH="${GOPATH:-${TMPDIR:-/tmp}/hmux-go}" \
		go build -trimpath -o "$AGENT_BIN" "$ROOT/cmd/hmux-agent"
fi
FRAME_HELPER="$TEST_ROOT/frame-helper"
cat >"$FRAME_HELPER" <<EOF
#!/bin/sh
printf '%s\n' "\$*" >>'$TEST_ROOT/frame-helper.log'
exec '$AGENT_BIN' "\$@"
EOF
chmod 700 "$FRAME_HELPER"

"$REAL_TMUX" -S "$TARGET_SOCKET" -f /dev/null \
	new-session -d -s hmux-e2e-frame-one
"$REAL_TMUX" -S "$TARGET_SOCKET" \
	new-session -d -s hmux-e2e-frame-two
SESSION_ONE=$("$REAL_TMUX" -S "$TARGET_SOCKET" display-message -p \
	-t hmux-e2e-frame-one '#{session_id}')
SESSION_TWO=$("$REAL_TMUX" -S "$TARGET_SOCKET" display-message -p \
	-t hmux-e2e-frame-two '#{session_id}')

cat >"$BIN_DIR/tmux" <<EOF
#!/bin/sh
printf '%s\n' "\$*" >>'$TMUX_LOG'
case "\${TMUX:-}:\$1" in
  *hmux-frame-*:refresh-client)
    if '$REAL_TMUX' "\$@"; then
      printf 'HMUX_REFRESH_OK %s\n' "\$*" >>'$TMUX_LOG'
      exit 0
    fi
    printf 'HMUX_REFRESH_FAILED %s\n' "\$*" >>'$TMUX_LOG'
    exit 1
    ;;
  *hmux-frame-*:*|*:-L) exec '$REAL_TMUX' "\$@" ;;
  *) exec '$REAL_TMUX' -S '$TARGET_SOCKET' "\$@" ;;
esac
EOF
chmod 700 "$BIN_DIR/tmux"
sed "s#~/.local/bin/hmux-agent#$FRAME_HELPER#g" \
	"$ROOT/archive/terminal/config/frame.tmux.conf" >"$HOME_DIR/.config/hmux/frame.tmux.conf"
sed "s#~/.local/bin/hmux-agent#$FRAME_HELPER#g" \
	"$ROOT/archive/terminal/config/frame-ui.tmux.conf" >"$HOME_DIR/.config/hmux/frame-ui.tmux.conf"
chmod 600 \
	"$HOME_DIR/.config/hmux/frame.tmux.conf" \
	"$HOME_DIR/.config/hmux/frame-ui.tmux.conf"
cat >"$HOME_DIR/.config/hmux/client.toml" <<EOF
schema_version = 1
client_id = "home-mac"
role = "home"
state_dir = "$HOME_DIR/.local/state/hmux"
timeout_seconds = 10
EOF
chmod 600 "$HOME_DIR/.config/hmux/client.toml"

before_sessions=$("$REAL_TMUX" -S "$TARGET_SOCKET" list-sessions -F '#{session_id}' | sort)
before_windows=$("$REAL_TMUX" -S "$TARGET_SOCKET" list-windows -a -F '#{window_id}' | sort)
before_panes=$("$REAL_TMUX" -S "$TARGET_SOCKET" list-panes -a -F '#{pane_id}' | sort)
before_options=$("$REAL_TMUX" -S "$TARGET_SOCKET" show-options -g)
before_window_options=$("$REAL_TMUX" -S "$TARGET_SOCKET" show-window-options -g)
before_keys=$("$REAL_TMUX" -S "$TARGET_SOCKET" list-keys)

run_frame() {
	mode="$1"
	session_id="$2"
	tmux_log_size=0
	if [ -f "$TMUX_LOG" ]; then
		tmux_log_size=$(wc -c <"$TMUX_LOG" | tr -d ' ')
	fi
	if HOME="$HOME_DIR" \
		PATH="$BIN_DIR:$PATH" \
		TERM=xterm-256color \
		HMUX_TEST_AGENT="$AGENT_BIN" \
		HMUX_TEST_LAUNCHER="$LAUNCHER" \
		HMUX_TEST_SESSION="$session_id" \
		HMUX_TEST_STATE_FILE="$HOME_DIR/.local/state/hmux/tabs/launchers/$LAUNCHER.json" \
		HMUX_TEST_TMUX_LOG="$TMUX_LOG" \
		HMUX_TEST_TMUX_LOG_SIZE="$tmux_log_size" \
		HMUX_TEST_MODE="$mode" \
		expect <<'EOF'; then
set timeout 10
if {[info exists env(HMUX_E2E_VERBOSE)]} {
  log_user 1
} else {
  log_user 0
}
log_file -a -noappend "$env(HOME)/frame-$env(HMUX_TEST_MODE).log"
spawn -noecho $env(HMUX_TEST_AGENT) attach --launcher $env(HMUX_TEST_LAUNCHER) $env(HMUX_TEST_SESSION)
set ready 0
for {set attempt 0} {$attempt < 200} {incr attempt} {
  if {[file exists $env(HMUX_TEST_STATE_FILE)]} {
    set input [open $env(HMUX_TEST_STATE_FILE) r]
    set state [read $input]
    close $input
    if {[string first "\"current_id\":\"$env(HMUX_TEST_SESSION)\"" $state] >= 0} {
      set ready 1
      break
    }
  }
  after 50
}
if {!$ready} {
  exit 123
}
# The state file is committed before the outer header is synchronously
# replaced. Wait for that second observable boundary so click assertions
# never race the status-range installation.
set header_ready 0
for {set attempt 0} {$attempt < 200} {incr attempt} {
  if {[file exists $env(HMUX_TEST_TMUX_LOG)]} {
    set input [open $env(HMUX_TEST_TMUX_LOG) r]
    set tmux_log [read $input]
    close $input
    set current_log [string range $tmux_log $env(HMUX_TEST_TMUX_LOG_SIZE) end]
    if {[string first "set-option -g @hmux_frame_tab1" $current_log] >= 0 &&
        [string first "refresh-client" $current_log] >= 0} {
      set header_ready 1
      break
    }
  }
  after 25
}
if {!$header_ready} {
  exit 122
}
# OpenFrame is immediately followed by raw-mode/input-proxy startup. Give that
# final local transition a small deterministic margin instead of assuming the
# user's login shell always starts within a fixed 1.2 seconds.
after 100
switch -- $env(HMUX_TEST_MODE) {
  list {
    send -- "\033\[5;9010~"
  }
  click-tabs-list {
    # The first visual tab begins after the fixed outer HMUX/TABS header. It is
    # on physical row 1, above the nested LIVE SESSION body frame.
    send -- "\033\[<0;35;1M"
    send -- "\033\[<0;35;1m"
    after 500
    send -- "\033\[<0;5;1M"
    send -- "\033\[<0;5;1m"
  }
  switch-close {
    send -- "\033\[5;9021~"
    after 400
    send -- "\033\[5;9011~"
    after 400
    send -- "\033\[5;9010~"
  }
  alias {
    send -- "\033\[5;9022~"
    after 250
    send -- "\033\[5;9013~"
    after 250
    send -- "friendly alias\r"
    after 500
    send -- "\033\[5;9010~"
  }
  close-last {
    send -- "\033\[5;9011~"
  }
  quit {
    send -- "\033\[5;9012~"
  }
  default {
    exit 125
  }
}
expect {
  eof {
    catch wait result
    exit [lindex $result 3]
  }
  timeout { exit 124 }
}
EOF
		status=0
	else
		status=$?
	fi
	if [ "$status" -ne 0 ]; then
		if [ "$mode" != quit ] || [ "$status" -ne 130 ]; then
			echo "frame PTY mode $mode failed with status $status" >&2
			tail -n 80 "$HOME_DIR/frame-$mode.log" >&2 || :
		fi
		return "$status"
	fi
}

start_observer() {
	session_id="$1"
	HMUX_REAL_TMUX="$REAL_TMUX" \
		HMUX_TARGET_SOCKET="$TARGET_SOCKET" \
		HMUX_OBSERVER_SESSION="$session_id" \
		expect <<'EOF' >/dev/null 2>&1 &
set timeout 60
log_user 0
spawn -noecho $env(HMUX_REAL_TMUX) -S $env(HMUX_TARGET_SOCKET) attach-session -t $env(HMUX_OBSERVER_SESSION)
expect {
  eof { exit 90 }
  timeout { exit 0 }
}
EOF
	OBSERVER_PID=$!
	attempt=0
	while [ "$attempt" -lt 50 ]; do
		if [ "$("$REAL_TMUX" -S "$TARGET_SOCKET" list-clients 2>/dev/null |
			wc -l | tr -d ' ')" -eq 1 ]; then
			return
		fi
		attempt=$((attempt + 1))
		sleep 0.02
	done
	echo "disposable observer tmux client did not attach" >&2
	exit 1
}

stop_observer() {
	kill "$OBSERVER_PID" >/dev/null 2>&1 || :
	wait "$OBSERVER_PID" >/dev/null 2>&1 || :
	OBSERVER_PID=""
	attempt=0
	while [ "$attempt" -lt 50 ]; do
		if test -z "$("$REAL_TMUX" -S "$TARGET_SOCKET" list-clients 2>/dev/null)"; then
			return
		fi
		attempt=$((attempt + 1))
		sleep 0.02
	done
	echo "disposable observer tmux client did not exit" >&2
	exit 1
}

run_frame list "$SESSION_ONE"
state_file="$HOME_DIR/.local/state/hmux/tabs/launchers/$LAUNCHER.json"
grep -Fq "\"sessions\":[\"$SESSION_ONE\"]" "$state_file"
grep -Fq 'HMUX' "$HOME_DIR/frame-list.log"
grep -Fq 'LIVE SESSION' "$HOME_DIR/frame-list.log"
grep -Fq '⌘L sessions' "$HOME_DIR/frame-list.log"
test -z "$("$REAL_TMUX" -S "$TARGET_SOCKET" list-clients 2>/dev/null)"
if grep -Fq 'HMUX_REFRESH_FAILED ' "$TMUX_LOG"; then
	echo "visible tab header refresh targeted the wrong outer client" >&2
	exit 1
fi
grep -Fq 'set-option -g @hmux_frame_tab1' "$TMUX_LOG"
grep -Fq 'refresh-client -t ' "$TMUX_LOG"
if grep -Fq '[exited]' "$HOME_DIR/frame-list.log"; then
	echo "frame leave exposed a transient [exited] screen" >&2
	exit 1
fi

run_frame click-tabs-list "$SESSION_TWO"
grep -Fq "\"sessions\":[\"$SESSION_ONE\",\"$SESSION_TWO\"]" "$state_file"
if ! grep -Fq "\"current_id\":\"$SESSION_ONE\"" "$state_file"; then
	echo "tab click did not activate the first launcher tab" >&2
	cat "$TEST_ROOT/frame-helper.log" >&2
	exit 1
fi
if grep -Fq '[exited]' "$HOME_DIR/frame-click-tabs-list.log"; then
	echo "status header click exposed a transient [exited] screen" >&2
	exit 1
fi

run_frame switch-close "$SESSION_TWO"
grep -Fq "\"sessions\":[\"$SESSION_TWO\"]" "$state_file"
grep -Fq "\"current_id\":\"$SESSION_TWO\"" "$state_file"

run_frame alias "$SESSION_TWO"
grep -Fq '"alias":"friendly alias"' \
	"$HOME_DIR/.local/state/hmux/sessions/sessions.json"
if "$REAL_TMUX" -S "$TARGET_SOCKET" capture-pane -p -t "$SESSION_TWO" |
	grep -Fq 'friendly alias'; then
	echo "alias editor input leaked into the target tmux pane" >&2
	exit 1
fi
for option in @hmux_profile @hmux_tags @hmux_label @hmux_alias; do
	test -z "$("$REAL_TMUX" -S "$TARGET_SOCKET" show-options -qv \
		-t "$SESSION_TWO" "$option")"
done

"$REAL_TMUX" -S "$TARGET_SOCKET" send-keys -l -t "$SESSION_TWO" \
	'while :; do printf "HMUX_STALE_FRAME_SENTINEL\n"; sleep 0.02; done'
"$REAL_TMUX" -S "$TARGET_SOCKET" send-keys -t "$SESSION_TWO" Enter
start_observer "$SESSION_TWO"
run_frame close-last "$SESSION_TWO"
if ! grep -Fq '"sessions":[]' "$state_file"; then
	echo "last-tab close did not clear launcher state: $(cat "$state_file")" >&2
	exit 1
fi
if ! kill -0 "$OBSERVER_PID" 2>/dev/null; then
	echo "Cmd-W detached a pre-existing tmux client" >&2
	exit 1
fi
test "$("$REAL_TMUX" -S "$TARGET_SOCKET" list-clients | wc -l | tr -d ' ')" -eq 1
if grep -Fq '[exited]' "$HOME_DIR/frame-close-last.log"; then
	echo "last-tab close exposed a transient [exited] screen" >&2
	exit 1
fi
stop_observer

set +e
run_frame quit "$SESSION_ONE"
quit_status=$?
set -e
test "$quit_status" -eq 130
"$REAL_TMUX" -S "$TARGET_SOCKET" has-session -t "$SESSION_ONE"
test -z "$("$REAL_TMUX" -S "$TARGET_SOCKET" list-clients 2>/dev/null)"
if grep -Fq '[exited]' "$HOME_DIR/frame-quit.log"; then
	echo "frame quit exposed a transient [exited] screen" >&2
	exit 1
fi

test "$("$REAL_TMUX" -S "$TARGET_SOCKET" list-sessions -F '#{session_id}' | sort)" = \
	"$before_sessions"
test "$("$REAL_TMUX" -S "$TARGET_SOCKET" list-windows -a -F '#{window_id}' | sort)" = \
	"$before_windows"
test "$("$REAL_TMUX" -S "$TARGET_SOCKET" list-panes -a -F '#{pane_id}' | sort)" = \
	"$before_panes"
test "$("$REAL_TMUX" -S "$TARGET_SOCKET" show-options -g)" = "$before_options"
test "$("$REAL_TMUX" -S "$TARGET_SOCKET" show-window-options -g)" = \
	"$before_window_options"
test "$("$REAL_TMUX" -S "$TARGET_SOCKET" list-keys)" = "$before_keys"
test "$("$REAL_TMUX" -S "$TARGET_SOCKET" list-sessions -F '#{session_name}' |
	grep -Fxc 'hmux-e2e-frame-one')" -eq 1
test "$("$REAL_TMUX" -S "$TARGET_SOCKET" list-sessions -F '#{session_name}' |
	grep -Fxc 'hmux-e2e-frame-two')" -eq 1
if grep -Eq \
	'(^|[[:space:]])(detach-client|kill-(client|session|window|pane|server))([[:space:]]|$)' \
	"$TMUX_LOG"; then
	echo "framed tab control issued a destructive tmux client/session command" >&2
	exit 1
fi
if grep -Eq \
	'^attach-session([[:space:]].*)?[[:space:]]-d([[:space:]]|$)' \
	"$TMUX_LOG"; then
	echo "framed attach used tmux handoff mode and detached another client" >&2
	exit 1
fi
grep -Fqx "attach-session -t $SESSION_ONE" "$TMUX_LOG"
grep -Fqx "attach-session -t $SESSION_TWO" "$TMUX_LOG"

cleanup
trap - EXIT HUP INT TERM
