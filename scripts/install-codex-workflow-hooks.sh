#!/bin/sh
set -eu

MODE="install"
case "${1:-}" in
"") ;;
--remove) MODE="remove" ;;
*)
	echo "usage: install-codex-workflow-hooks.sh [--remove]" >&2
	exit 2
	;;
esac

command -v jq >/dev/null 2>&1 || {
	echo "jq is required to merge Codex hooks safely" >&2
	exit 1
}

umask 077
CURRENT_UID="$(id -u)"
stat_uid() {
	stat -f '%u' "$1" 2>/dev/null || stat -c '%u' "$1"
}
stat_mode() {
	stat -f '%Lp' "$1" 2>/dev/null || stat -c '%a' "$1"
}
validate_owned_directory() {
	HMUX_DIRECTORY_CHECK="$1"
	[ -d "$HMUX_DIRECTORY_CHECK" ] && [ ! -L "$HMUX_DIRECTORY_CHECK" ] || {
		echo "unsafe directory: $HMUX_DIRECTORY_CHECK" >&2
		exit 1
	}
	[ "$(stat_uid "$HMUX_DIRECTORY_CHECK")" = "$CURRENT_UID" ] || {
		echo "directory is not owned by the current user: $HMUX_DIRECTORY_CHECK" >&2
		exit 1
	}
	HMUX_DIRECTORY_MODE="$(stat_mode "$HMUX_DIRECTORY_CHECK")"
	case "$HMUX_DIRECTORY_MODE" in '' | *[!0-7]*)
		echo "invalid directory mode: $HMUX_DIRECTORY_CHECK" >&2
		exit 1
		;;
	esac
	[ $((HMUX_DIRECTORY_MODE / 10 % 10 & 2)) -eq 0 ] && [ $((HMUX_DIRECTORY_MODE % 10 & 2)) -eq 0 ] || {
		echo "directory is group/other writable: $HMUX_DIRECTORY_CHECK" >&2
		exit 1
	}
}
ensure_owned_directory() {
	HMUX_DIRECTORY_TARGET="$1"
	if [ ! -e "$HMUX_DIRECTORY_TARGET" ]; then
		HMUX_DIRECTORY_PARENT="${HMUX_DIRECTORY_TARGET%/*}"
		[ -n "$HMUX_DIRECTORY_PARENT" ] || HMUX_DIRECTORY_PARENT="/"
		validate_owned_directory "$HMUX_DIRECTORY_PARENT"
		mkdir "$HMUX_DIRECTORY_TARGET"
	fi
	validate_owned_directory "$HMUX_DIRECTORY_TARGET"
}

CODEX_DIR="${CODEX_HOME:-$HOME/.codex}"
HOOKS_FILE="${HMUX_CODEX_HOOKS_FILE:-$CODEX_DIR/hooks.json}"
case "$HOOKS_FILE" in
/*) ;;
*)
	echo "Codex hooks path must be absolute" >&2
	exit 1
	;;
esac

ensure_owned_directory "$CODEX_DIR"
HOOKS_DIR="${HOOKS_FILE%/*}"
[ -n "$HOOKS_DIR" ] || HOOKS_DIR="/"
validate_owned_directory "$HOOKS_DIR"
if [ -L "$HOOKS_FILE" ]; then
	echo "refusing to replace a symlinked Codex hooks file" >&2
	exit 1
fi
HOOKS_EXISTED=""
if [ -e "$HOOKS_FILE" ]; then
	HOOKS_EXISTED="1"
	[ -f "$HOOKS_FILE" ] || {
		echo "Codex hooks path is not a regular file" >&2
		exit 1
	}
	[ "$(stat_uid "$HOOKS_FILE")" = "$CURRENT_UID" ] || {
		echo "Codex hooks file is not owned by the current user" >&2
		exit 1
	}
	file_mode="$(stat_mode "$HOOKS_FILE")"
	case "$file_mode" in '' | *[!0-7]*)
		echo "invalid Codex hooks file mode" >&2
		exit 1
		;;
	esac
	[ $((file_mode / 10 % 10 & 2)) -eq 0 ] && [ $((file_mode % 10 & 2)) -eq 0 ] || {
		echo "Codex hooks file is group/other writable" >&2
		exit 1
	}
	[ "$(wc -c <"$HOOKS_FILE")" -le 1048576 ] || {
		echo "Codex hooks file exceeds 1 MiB" >&2
		exit 1
	}
	jq -e 'type == "object" and ((.hooks // {}) | type == "object")' "$HOOKS_FILE" >/dev/null
fi
if [ "$MODE" = "remove" ] && [ -z "$HOOKS_EXISTED" ]; then
	echo "Codex workflow hooks already removed; no change"
	exit 0
fi

STAMP="$(date -u +%Y%m%dT%H%M%SZ)-$$"
BACKUP_DIR="$HOME/.config/hmux/backups/$STAMP"
TEMP=""
SOURCE_TEMP=""
trap '[ -z "$TEMP" ] || rm -f "$TEMP"; [ -z "$SOURCE_TEMP" ] || rm -f "$SOURCE_TEMP"' EXIT HUP INT TERM
TEMP="$(mktemp "$HOOKS_DIR/.hooks.hmux.XXXXXX")"
SOURCE_TEMP="$(mktemp "$HOOKS_DIR/.hooks.hmux.source.XXXXXX")"
SOURCE="$SOURCE_TEMP"
if [ -n "$HOOKS_EXISTED" ]; then
	cp "$HOOKS_FILE" "$SOURCE_TEMP"
else
	printf '%s\n' '{"hooks":{}}' >"$SOURCE_TEMP"
fi
[ "$(wc -c <"$SOURCE_TEMP")" -le 1048576 ] || {
	echo "Codex hooks snapshot exceeds 1 MiB" >&2
	exit 1
}

MARKER="HMUX_WORKFLOW_HOOK=1"
# shellcheck disable=SC2016 # $HOME expands when Codex runs the installed hook.
COMMAND='HMUX_WORKFLOW_HOOK=1; agent="$HOME/.local/bin/hmux-agent"; if [ -x "$agent" ]; then { "$agent" workflow-hook; } || { /bin/cat >/dev/null 2>/dev/null || true; echo "{}"; }; else { /bin/cat >/dev/null 2>/dev/null || true; echo "{}"; }; fi'
EVENTS='["UserPromptSubmit","SubagentStart","SubagentStop","PermissionRequest","PreToolUse","PostToolUse","Stop","SessionEnd"]'

jq --arg mode "$MODE" --arg marker "$MARKER" --arg command "$COMMAND" --argjson events "$EVENTS" '
  def managed:
    ((.command? | strings | contains($marker)) // false);
  def scrub_group:
    if ((.hooks? | type) == "array") and any(.hooks[]; managed) then
      .hooks |= map(select(managed | not)) |
      select((.hooks | length) > 0)
    else . end;
  .hooks = (.hooks // {}) |
  reduce $events[] as $event (.;
    .hooks[$event] = ((.hooks[$event] // []) | map(scrub_group)) |
    if $mode == "install" then
      .hooks[$event] += [{"hooks":[{"type":"command","command":$command,"timeout":3}]}]
    else . end
  ) |
  .hooks |= with_entries(select((.value | length) > 0))
' "$SOURCE" >"$TEMP"

jq -e 'type == "object" and (.hooks | type == "object")' "$TEMP" >/dev/null
chmod 600 "$TEMP"
if [ -n "$HOOKS_EXISTED" ] && cmp -s "$SOURCE_TEMP" "$TEMP"; then
	cmp -s "$HOOKS_FILE" "$SOURCE_TEMP" || {
		echo "Codex hooks changed concurrently; refusing stale no-op" >&2
		exit 1
	}
	echo "Codex workflow hooks already $MODE; no change"
	exit 0
fi

ensure_owned_directory "$HOME/.config"
ensure_owned_directory "$HOME/.config/hmux"
ensure_owned_directory "$HOME/.config/hmux/backups"
ensure_owned_directory "$BACKUP_DIR"
if [ -n "$HOOKS_EXISTED" ]; then
	cmp -s "$HOOKS_FILE" "$SOURCE_TEMP" || {
		echo "Codex hooks changed concurrently; refusing stale replacement" >&2
		exit 1
	}
	cp "$SOURCE_TEMP" "$BACKUP_DIR/codex-hooks.json"
	chmod "$file_mode" "$BACKUP_DIR/codex-hooks.json"
	cmp -s "$SOURCE_TEMP" "$BACKUP_DIR/codex-hooks.json" || {
		echo "Codex hooks backup verification failed" >&2
		exit 1
	}
else
	[ ! -e "$HOOKS_FILE" ] || {
		echo "Codex hooks appeared concurrently; refusing stale replacement" >&2
		exit 1
	}
fi
if [ -n "$HOOKS_EXISTED" ]; then
	cmp -s "$HOOKS_FILE" "$SOURCE_TEMP" || {
		echo "Codex hooks changed concurrently; refusing stale replacement" >&2
		exit 1
	}
else
	[ ! -e "$HOOKS_FILE" ] || {
		echo "Codex hooks appeared concurrently; refusing stale replacement" >&2
		exit 1
	}
fi
mv "$TEMP" "$HOOKS_FILE"
chmod 600 "$HOOKS_FILE"
if [ -n "$SOURCE_TEMP" ]; then
	rm -f "$SOURCE_TEMP"
	SOURCE_TEMP=""
fi
trap - EXIT HUP INT TERM
if [ -n "$HOOKS_EXISTED" ]; then
	echo "Codex workflow hooks $MODE complete; backup: $BACKUP_DIR/codex-hooks.json"
else
	echo "Codex workflow hooks $MODE complete; created a new managed file"
fi
