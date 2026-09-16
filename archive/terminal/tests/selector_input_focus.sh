#!/bin/sh
set -eu

command -v fzf >/dev/null 2>&1 || exit 0
command -v tmux >/dev/null 2>&1 || exit 0

TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/hmux-e2e-selector-focus.XXXXXX")"
SOCKET="hmux-e2e-selector-focus-$$"
RUNNER="$TEST_ROOT/runner"

cleanup() {
	tmux -L "$SOCKET" kill-server >/dev/null 2>&1 || :
	chmod -R u+w "$TEST_ROOT" 2>/dev/null || :
	rm -rf "$TEST_ROOT"
}
trap cleanup EXIT HUP INT TERM

cat >"$RUNNER" <<'EOF'
#!/bin/sh
set +e
printf '$1\talpha\n$2\tbeta\n' |
	fzf --no-input --disabled --print-query '--bind=/:unbind(/)+show-input' \
		>"$HMUX_FZF_OUTPUT" 2>"$HMUX_FZF_ERROR"
result=$?
printf '%s\n' "$result" >"$HMUX_FZF_DONE"
exit "$result"
EOF
chmod 700 "$RUNNER"

tmux -L "$SOCKET" -f /dev/null new-session -d \
	-s hmux-e2e-selector-focus-anchor 'sleep 30'

run_case() {
	mode="$1"
	case "$mode" in normal | search | slash) ;; *) exit 1 ;; esac
	output="$TEST_ROOT/$mode.out"
	error_file="$TEST_ROOT/$mode.err"
	done_file="$TEST_ROOT/$mode.done"
	session="hmux-e2e-selector-focus-$mode"
	tmux -L "$SOCKET" new-session -d -s "$session" \
		"env HMUX_FZF_OUTPUT='$output' HMUX_FZF_ERROR='$error_file' HMUX_FZF_DONE='$done_file' '$RUNNER'"
	attempt=0
	ready=0
	while [ "$attempt" -lt 100 ]; do
		if tmux -L "$SOCKET" capture-pane -p -t "$session" 2>/dev/null |
			grep -Fq alpha; then
			ready=1
			break
		fi
		attempt=$((attempt + 1))
		sleep 0.02
	done
	[ "$ready" -eq 1 ] || {
		if [ -f "$done_file" ] && [ "$(sed -n '1p' "$done_file")" -eq 2 ] &&
			grep -Fqx 'operation not permitted' "$error_file"; then
			echo "selector focus PTY skipped: interactive fzf is blocked by the execution sandbox" >&2
			exit 0
		fi
		echo "fzf did not become ready for $mode" >&2
		tmux -L "$SOCKET" capture-pane -p -e -t "$session" >&2 2>/dev/null || :
		if [ -f "$done_file" ]; then
			printf 'fzf status: %s\n' "$(sed -n '1p' "$done_file")" >&2
		fi
		exit 1
	}
	case "$mode" in
	normal)
		tmux -L "$SOCKET" send-keys -t "$session" -l -- x
		;;
	search)
		tmux -L "$SOCKET" send-keys -t "$session" -l -- /beta
		;;
	slash)
		tmux -L "$SOCKET" send-keys -t "$session" -l -- //path
		;;
	esac
	tmux -L "$SOCKET" send-keys -t "$session" Enter
	attempt=0
	while [ "$attempt" -lt 100 ] && [ ! -f "$done_file" ]; do
		attempt=$((attempt + 1))
		sleep 0.02
	done
	[ -f "$done_file" ] && [ "$(sed -n '1p' "$done_file")" -eq 0 ] || {
		echo "fzf focus case failed: $mode" >&2
		exit 1
	}
}

run_case normal
run_case search
run_case slash

test -z "$(sed -n '1p' "$TEST_ROOT/normal.out")"
test "$(sed -n '1p' "$TEST_ROOT/search.out")" = beta
test "$(sed -n '1p' "$TEST_ROOT/slash.out")" = /path

# Production completes the potentially slow catalog fetch before asking fzf
# to reload a fast local snapshot. Stable-ID tracking must survive a real row
# reorder, and navigation after that boundary must advance from the same ID.
REFRESH_ROWS="$TEST_ROOT/refresh-before.rows"
REFRESH_CHANGED_ROWS="$TEST_ROOT/refresh-after.rows"
REFRESH_OUTPUT="$TEST_ROOT/refresh.out"
REFRESH_DONE="$TEST_ROOT/refresh.done"
REFRESH_RUNNER="$TEST_ROOT/refresh-runner"
{
	row=1
	while [ "$row" -le 20 ]; do
		printf '$%s\trow-%02d\n' "$row" "$row"
		row=$((row + 1))
	done
} >"$REFRESH_ROWS"
{
	row=20
	while [ "$row" -ge 1 ]; do
		printf '$%s\trow-%02d-changed\n' "$row" "$row"
		row=$((row - 1))
	done
} >"$REFRESH_CHANGED_ROWS"
cat >"$REFRESH_RUNNER" <<'EOF'
#!/bin/sh
set +e
fzf --disabled --no-input --layout=reverse --delimiter='\t' --with-nth=2 \
	--id-nth=1 --track \
	--bind='every(1):reload(cat "$HMUX_REFRESH_CHANGED_ROWS")' \
	<"$HMUX_REFRESH_ROWS" >"$HMUX_REFRESH_OUTPUT" 2>"$HMUX_REFRESH_ERROR"
result=$?
printf '%s\n' "$result" >"$HMUX_REFRESH_DONE"
exit "$result"
EOF
chmod 700 "$REFRESH_RUNNER"
refresh_session=hmux-e2e-selector-focus-refresh
tmux -L "$SOCKET" new-session -d -s "$refresh_session" \
	"env HMUX_REFRESH_ROWS='$REFRESH_ROWS' HMUX_REFRESH_CHANGED_ROWS='$REFRESH_CHANGED_ROWS' HMUX_REFRESH_OUTPUT='$REFRESH_OUTPUT' HMUX_REFRESH_ERROR='$TEST_ROOT/refresh.err' HMUX_REFRESH_DONE='$REFRESH_DONE' '$REFRESH_RUNNER'"
attempt=0
ready=0
while [ "$attempt" -lt 100 ]; do
	if tmux -L "$SOCKET" capture-pane -p -t "$refresh_session" 2>/dev/null |
		grep -Fq row-01; then
		ready=1
		break
	fi
	attempt=$((attempt + 1))
	sleep 0.02
done
[ "$ready" -eq 1 ] || {
	echo "refresh navigation fzf did not become ready" >&2
	exit 1
}
movement=0
while [ "$movement" -lt 5 ]; do
	tmux -L "$SOCKET" send-keys -t "$refresh_session" Down
	movement=$((movement + 1))
	sleep 0.05
done
# The first reload reverses the rows. Tracking must keep $6 focused; two more
# Down keys in the reversed list then land on $4. Without stable-ID tracking,
# the same row-position behavior would land elsewhere.
sleep 1.05
tmux -L "$SOCKET" send-keys -t "$refresh_session" Down Down
tmux -L "$SOCKET" send-keys -t "$refresh_session" Enter
attempt=0
while [ "$attempt" -lt 100 ] && [ ! -f "$REFRESH_DONE" ]; do
	attempt=$((attempt + 1))
	sleep 0.02
done
[ -f "$REFRESH_DONE" ] && [ "$(sed -n '1p' "$REFRESH_DONE")" -eq 0 ] || {
	echo "refresh navigation case did not finish" >&2
	exit 1
}
test "$(sed -n '1p' "$REFRESH_OUTPUT" | cut -f1)" = "\$4"
