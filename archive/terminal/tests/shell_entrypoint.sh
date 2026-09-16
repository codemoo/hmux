#!/bin/sh
set -eu

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/../../.." && pwd)"
if grep -Fq 'linux-server' "$ROOT/archive/terminal/scripts/hmux-entrypoint.sh"; then
	echo "shared entrypoint still depends on a pre-existing linux-server alias" >&2
	exit 1
fi
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/hmux-entrypoint-test.XXXXXX")"
trap 'rm -rf "$TEST_ROOT"' EXIT HUP INT TERM
HOME_DIR="$TEST_ROOT/home"
mkdir -p "$HOME_DIR/.config/hmux" "$HOME_DIR/.local/bin" \
	"$HOME_DIR/.cache/hmux/releases/0.1.3"

cat >"$HOME_DIR/.config/hmux/client.toml" <<'EOF'
schema_version = 1
client_id = "office-mac"
role = "remote"
EOF
printf '%s\n' office-mac >"$HOME_DIR/.config/hmux/provisioned-client"
chmod 600 "$HOME_DIR/.config/hmux/provisioned-client"

cat >"$HOME_DIR/.local/bin/hmux" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "launcher-cleanup" ] || [ "${2:-}" = "launcher-cleanup" ]; then
	exit 0
fi
if [ -n "${HMUX_TEST_LAUNCHER_LOG:-}" ]; then
	printf '%s:%s\n' "${HMUX_LAUNCHER:-}" "${HMUX_LAUNCHER_ID:-}" >>"$HMUX_TEST_LAUNCHER_LOG"
	count="$(wc -l <"$HMUX_TEST_LAUNCHER_LOG" | tr -d ' ')"
	if [ "$count" -eq 1 ]; then
		exit 0
	fi
	exit 130
fi
printf '%s\n' "$@"
EOF
chmod 700 "$HOME_DIR/.local/bin/hmux"
cp "$HOME_DIR/.local/bin/hmux" "$HOME_DIR/.cache/hmux/releases/0.1.3/hmux"
printf '{}\n' >"$HOME_DIR/.cache/hmux/releases/0.1.3/manifest.json"
ln -s releases/0.1.3/hmux "$HOME_DIR/.cache/hmux/current"

if ! OUTPUT="$(
	HOME="$HOME_DIR" \
		HMUX_SKIP_PREREQUISITES=1 \
		HMUX_UI_SKIP_GHOSTTY=1 \
		"$ROOT/archive/terminal/scripts/hmux-entrypoint.sh" alpha "two words" 2>"$TEST_ROOT/entrypoint.err"
)"; then
	cat "$TEST_ROOT/entrypoint.err" >&2
	exit 1
fi
if [ "$OUTPUT" != "alpha
two words" ]; then
	cat "$TEST_ROOT/entrypoint.err" >&2
	exit 1
fi

launcher_log="$TEST_ROOT/launcher.log"
HOME="$HOME_DIR" \
	HMUX_SKIP_PREREQUISITES=1 \
	HMUX_UI_SKIP_GHOSTTY=1 \
	HMUX_FORCE_LAUNCHER=1 \
	HMUX_TEST_LAUNCHER_LOG="$launcher_log" \
	TMUX='' \
	"$ROOT/archive/terminal/scripts/hmux-entrypoint.sh"
test "$(wc -l <"$launcher_log" | tr -d ' ')" -eq 2
test "$(grep -Ec '^1:[0-9a-f]{32}$' "$launcher_log")" -eq 2

rm "$HOME_DIR/.config/hmux/provisioned-client"
if HOME="$HOME_DIR" HMUX_SKIP_PREREQUISITES=1 \
	HMUX_DMZ_BOOTSTRAP_ALIAS='invalid alias' \
	"$ROOT/archive/terminal/scripts/hmux-entrypoint.sh" >/dev/null 2>&1; then
	echo "remote cache without a completion marker was treated as provisioned" >&2
	exit 1
fi

rm "$HOME_DIR/.config/hmux/client.toml"
printf '%s\n' invalid-client >"$HOME_DIR/.config/hmux/machine-role"
if HOME="$HOME_DIR" HMUX_SKIP_PREREQUISITES=1 \
	"$ROOT/archive/terminal/scripts/hmux-entrypoint.sh" >/dev/null 2>&1; then
	echo "invalid local role was accepted" >&2
	exit 1
fi
