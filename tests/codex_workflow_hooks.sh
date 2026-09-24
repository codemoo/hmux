#!/bin/sh
set -eu

command -v jq >/dev/null 2>&1 || exit 0
ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/hmux-codex-hooks.XXXXXX")"
trap 'rm -rf "$TEST_ROOT"' EXIT HUP INT TERM
mkdir -p "$TEST_ROOT/home/.codex" "$TEST_ROOT/home/.config"
chmod 755 "$TEST_ROOT/home/.config"
cat >"$TEST_ROOT/home/.codex/hooks.json" <<'JSON'
{
  "hooks": {
    "PreToolUse": [
      {"matcher":"mixed","hooks":[
        {"type":"command","command":"cmux preserved","timeout":5},
        {"type":"command","command":"HMUX_WORKFLOW_HOOK=1 old managed","timeout":3}
      ]}
    ]
  }
}
JSON

HOME="$TEST_ROOT/home" "$ROOT/scripts/install-codex-workflow-hooks.sh" >/dev/null
jq -e '
  (.hooks.PreToolUse | map(.hooks[].command) | any(. == "cmux preserved")) and
  ([.hooks.PreToolUse[] | select(.matcher == "mixed") | .hooks[] | .command] == ["cmux preserved"]) and
  ([.hooks[] | .[] | .hooks[] | .command | select(contains("HMUX_WORKFLOW_HOOK=1"))] | length == 8) and
  ([.hooks[] | .[] | .hooks[] | .command | select(contains("HMUX_WORKFLOW_HOOK=1")) | contains("/bin/cat")] | all)
' "$TEST_ROOT/home/.codex/hooks.json" >/dev/null

mkdir "$TEST_ROOT/real-codex"
ln -s "$TEST_ROOT/real-codex" "$TEST_ROOT/symlinked-codex"
if HOME="$TEST_ROOT/home" CODEX_HOME="$TEST_ROOT/symlinked-codex" \
	"$ROOT/scripts/install-codex-workflow-hooks.sh" >/dev/null 2>&1; then
	echo "installer accepted a symlinked Codex directory" >&2
	exit 1
fi

mkdir "$TEST_ROOT/home-empty" "$TEST_ROOT/home-empty/.codex"
HOME="$TEST_ROOT/home-empty" "$ROOT/scripts/install-codex-workflow-hooks.sh" --remove >/dev/null
[ ! -e "$TEST_ROOT/home-empty/.codex/hooks.json" ]
HOME="$TEST_ROOT/home-empty" "$ROOT/scripts/install-codex-workflow-hooks.sh" >/dev/null
jq -e '([.hooks[] | .[] | .hooks[] | .command | select(contains("HMUX_WORKFLOW_HOOK=1"))] | length == 8)' \
	"$TEST_ROOT/home-empty/.codex/hooks.json" >/dev/null
[ -z "$(find "$TEST_ROOT/home-empty/.codex" -maxdepth 1 -name '.hooks.hmux.source.*' -print -quit)" ]
case "$(uname -s)" in
Darwin)
	[ "$(stat -f '%Lp' "$TEST_ROOT/home/.codex/hooks.json")" = "600" ]
	[ "$(stat -f '%Lp' "$TEST_ROOT/home/.config")" = "755" ]
	;;
Linux)
	[ "$(stat -c '%a' "$TEST_ROOT/home/.codex/hooks.json")" = "600" ]
	[ "$(stat -c '%a' "$TEST_ROOT/home/.config")" = "755" ]
	;;
*) exit 1 ;;
esac

hash_file() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | awk '{print $1}'
	else
		shasum -a 256 "$1" | awk '{print $1}'
	fi
}
FIRST_HASH="$(hash_file "$TEST_ROOT/home/.codex/hooks.json")"
HOME="$TEST_ROOT/home" "$ROOT/scripts/install-codex-workflow-hooks.sh" >/dev/null
SECOND_HASH="$(hash_file "$TEST_ROOT/home/.codex/hooks.json")"
[ "$FIRST_HASH" = "$SECOND_HASH" ]

HOME="$TEST_ROOT/home" "$ROOT/scripts/install-codex-workflow-hooks.sh" --remove >/dev/null
jq -e '
  (.hooks.PreToolUse | length == 1) and
  (.hooks.PreToolUse[0].matcher == "mixed") and
  (.hooks.PreToolUse[0].hooks == [{"type":"command","command":"cmux preserved","timeout":5}]) and
  ([.hooks[] | .[] | .hooks[] | .command | select(contains("HMUX_WORKFLOW_HOOK=1"))] | length == 0)
' "$TEST_ROOT/home/.codex/hooks.json" >/dev/null
