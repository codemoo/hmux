#!/bin/sh
set -eu

command -v tmux >/dev/null 2>&1 || exit 0

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/../../.." && pwd)"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/hmux-e2e-launcher-transition.XXXXXX")"
HOME_DIR="$TEST_ROOT/home"
BIN_DIR="$TEST_ROOT/bin"
CLIENT="$TEST_ROOT/hmux"
SOCKET="hmux-e2e-launcher-transition-$$"
SESSION="hmux-e2e-launcher-transition"

cleanup() {
	tmux -L "$SOCKET" kill-server >/dev/null 2>&1 || :
	chmod -R u+w "$TEST_ROOT" 2>/dev/null || :
	rm -rf "$TEST_ROOT"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$HOME_DIR/.config/hmux" "$HOME_DIR/.local/state/hmux" "$BIN_DIR"
if [ -n "${HMUX_E2E_CLIENT_BIN:-}" ]; then
	[ -x "$HMUX_E2E_CLIENT_BIN" ] || {
		echo "HMUX_E2E_CLIENT_BIN must name an executable" >&2
		exit 1
	}
	install -m 700 "$HMUX_E2E_CLIENT_BIN" "$CLIENT"
	unset HMUX_E2E_CLIENT_BIN
else
	GOCACHE="${GOCACHE:-${TMPDIR:-/tmp}/hmux-go-cache}" \
		GOPATH="${GOPATH:-${TMPDIR:-/tmp}/hmux-go}" \
		go build -trimpath -o "$CLIENT" "$ROOT/cmd/hmux"
fi

cat >"$HOME_DIR/.config/hmux/client.toml" <<EOF
schema_version = 1
client_id = "office-mac"
role = "remote"
home_alias = "hmux-e2e-home"
agent_path = "/usr/local/bin/hmux-agent"
state_dir = "$HOME_DIR/.local/state/hmux"
timeout_seconds = 2
update_check = false
EOF
chmod 600 "$HOME_DIR/.config/hmux/client.toml"

cat >"$BIN_DIR/ssh" <<'EOF'
#!/bin/sh
set -eu
case "$*" in
*" catalog" | *" catalog --launcher "*)
	printf 'catalog\n' >>"$HMUX_TEST_SSH_LOG"
	if grep -Fq 'attach' "$HMUX_TEST_SSH_LOG"; then
		printf '%s\n' '{"protocol_version":1,"generated_at":"2026-07-30T00:00:00Z","sessions":[{"id":"$1","name":"main"}],"open_tabs":["$1"],"current_tab_id":"$1"}'
	else
		printf '%s\n' '{"protocol_version":1,"generated_at":"2026-07-30T00:00:00Z","sessions":[{"id":"$1","name":"main"}]}'
	fi
	;;
*" attach --launcher "*)
	printf 'attach\n' >>"$HMUX_TEST_SSH_LOG"
	;;
*)
	exit 91
	;;
esac
EOF
chmod 700 "$BIN_DIR/ssh"

cat >"$BIN_DIR/fzf" <<'EOF'
#!/bin/sh
set -eu
trap 'exit 0' HUP INT TERM
count=0
if [ -f "$HMUX_TEST_FZF_COUNT" ]; then
	IFS= read -r count <"$HMUX_TEST_FZF_COUNT"
fi
count=$((count + 1))
printf '%s\n' "$count" >"$HMUX_TEST_FZF_COUNT"
printf '%s\n' "$PPID" >>"$HMUX_TEST_FZF_PARENTS"
printf '%s\n' "$@" >"$HMUX_TEST_FZF_ARGS.$count"
if [ "$count" -eq 1 ]; then
	printf '\033[5;1HBACKING_SCREEN_WAS_PRESERVED' >&2
	printf '$1\tselected\n'
	exit 0
fi
sleep 30
EOF
chmod 700 "$BIN_DIR/fzf"

tmux -L "$SOCKET" -f /dev/null new-session -d -x 120 -y 40 -s "$SESSION" \
	"env HOME='$HOME_DIR' PATH='$BIN_DIR:$PATH' NO_COLOR=1 HMUX_LAUNCHER=1 HMUX_LAUNCHER_ID=0123456789abcdef0123456789abcdef HMUX_TEST_SSH_LOG='$TEST_ROOT/ssh.log' HMUX_TEST_FZF_COUNT='$TEST_ROOT/fzf.count' HMUX_TEST_FZF_PARENTS='$TEST_ROOT/fzf.parents' HMUX_TEST_FZF_ARGS='$TEST_ROOT/fzf.args' '$CLIENT' --config '$HOME_DIR/.config/hmux/client.toml' --no-update-check"

attempt=0
while [ "$attempt" -lt 100 ]; do
	if [ -f "$TEST_ROOT/fzf.count" ] &&
		[ "$(cat "$TEST_ROOT/fzf.count")" -eq 2 ]; then
		break
	fi
	attempt=$((attempt + 1))
	sleep 0.02
done
if [ ! -f "$TEST_ROOT/fzf.count" ] ||
	[ "$(cat "$TEST_ROOT/fzf.count")" -ne 2 ]; then
	echo "launcher did not resume its selector in the same client" >&2
	tmux -L "$SOCKET" capture-pane -p -t "$SESSION" >&2 || :
	exit 1
fi

grep -Fqx -- '--no-clear' "$TEST_ROOT/fzf.args.1"
grep -Fqx -- '--no-input' "$TEST_ROOT/fzf.args.1"
grep -Fq '/:bg-cancel+transform(' "$TEST_ROOT/fzf.args.1"
grep -Fq 'begin-search' "$TEST_ROOT/fzf.args.1"
grep -Fq 'TABS' "$TEST_ROOT/fzf.args.2"
grep -Fq '1 main' "$TEST_ROOT/fzf.args.2"
grep -Fqx -- '--no-input' "$TEST_ROOT/fzf.args.2"
test "$(wc -l <"$TEST_ROOT/fzf.parents" | tr -d ' ')" -eq 2
test "$(sort -u "$TEST_ROOT/fzf.parents" | wc -l | tr -d ' ')" -eq 1
test "$(cat "$TEST_ROOT/ssh.log")" = "catalog
attach
catalog"

# The second selector has started but deliberately drawn nothing. Its backing
# list must therefore still be visible. A clear between frame return and fzf
# restart would erase this marker.
tmux -L "$SOCKET" capture-pane -p -t "$SESSION" |
	grep -Fq 'BACKING_SCREEN_WAS_PRESERVED'

cleanup
trap - EXIT HUP INT TERM
