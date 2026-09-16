#!/bin/sh
set -eu

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
TMUX_BIN="$(command -v tmux)"
AGENT="${HMUX_AGENT_BIN:-$HOME/.local/bin/hmux-agent}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)-$$"
BACKUP_DIR="$HOME/.config/hmux/backups/$STAMP/target-tmux-cleanup"
REFERENCE="hmux-e2e-restore-$$"
RESTORE_FILE="$BACKUP_DIR/native-options.tmux"

[ -x "$AGENT" ] || {
	echo "hmux: installed hmux-agent is required for metadata migration" >&2
	exit 1
}

mkdir -p "$BACKUP_DIR"
chmod 700 "$BACKUP_DIR"
if [ -f "$HOME/.tmux.conf" ]; then
	cp -p "$HOME/.tmux.conf" "$BACKUP_DIR/.tmux.conf"
fi

snapshot() {
	prefix="$1"
	"$TMUX_BIN" list-sessions -F '#{session_id}' | sort >"$BACKUP_DIR/$prefix.sessions"
	"$TMUX_BIN" list-windows -a -F '#{window_id}' | sort >"$BACKUP_DIR/$prefix.windows"
	"$TMUX_BIN" list-panes -a -F '#{pane_id}' | sort >"$BACKUP_DIR/$prefix.panes"
	"$TMUX_BIN" list-clients -F '#{client_name}' | sort >"$BACKUP_DIR/$prefix.clients"
}

cleanup() {
	"$TMUX_BIN" -L "$REFERENCE" kill-server >/dev/null 2>&1 || :
}
trap cleanup EXIT HUP INT TERM

if ! "$TMUX_BIN" list-sessions >/dev/null 2>&1; then
	HMUX_UI_SKIP_GHOSTTY=1 "$ROOT/archive/terminal/scripts/sync-ui-config-macos.sh"
	echo "hmux: no live target tmux server; managed include retired"
	echo "hmux: backup: $BACKUP_DIR"
	exit 0
fi

snapshot before

# Persist legacy aliases/profile labels outside tmux before unsetting the four
# old @hmux_* session options. No session/window/pane/client operation occurs.
"$AGENT" metadata-migrate --clear >"$BACKUP_DIR/metadata-migration.txt"

# This creates a reference server on its own socket only. It is never attached
# and its disposable session name follows the required hmux-e2e- prefix.
HMUX_UI_SKIP_GHOSTTY=1 "$ROOT/archive/terminal/scripts/sync-ui-config-macos.sh"
"$TMUX_BIN" -L "$REFERENCE" -f "$HOME/.tmux.conf" \
	new-session -d -s "$REFERENCE"

: >"$RESTORE_FILE"
chmod 600 "$RESTORE_FILE"
for option in \
	default-terminal terminal-features \
	status-position status status-interval status-style status-left status-format; do
	"$TMUX_BIN" -L "$REFERENCE" show-options -g "$option" |
		sed 's/^/set-option -g /' >>"$RESTORE_FILE"
done
for option in \
	window-style window-active-style fill-character \
	pane-border-lines pane-border-status pane-border-style \
	pane-active-border-style pane-border-format; do
	"$TMUX_BIN" -L "$REFERENCE" show-window-options -g "$option" |
		sed 's/^/set-option -gw /' >>"$RESTORE_FILE"
done

for spec in \
	"prefix L" "prefix l" "prefix W" "prefix w" \
	"prefix Q" "prefix q" "prefix s" \
	"prefix 1" "prefix 2" "prefix 3" "prefix 4" "prefix 5" \
	"prefix 6" "prefix 7" "prefix 8" "prefix 9" \
	"root C-q" "root C-r"; do
	table=${spec% *}
	key=${spec#* }
	printf 'unbind-key -T %s %s\n' "$table" "$key" >>"$RESTORE_FILE"
	"$TMUX_BIN" -L "$REFERENCE" list-keys -T "$table" "$key" \
		>>"$RESTORE_FILE" 2>/dev/null || :
done

"$TMUX_BIN" source-file "$RESTORE_FILE"
snapshot after

for kind in sessions windows panes clients; do
	if ! cmp -s "$BACKUP_DIR/before.$kind" "$BACKUP_DIR/after.$kind"; then
		echo "hmux: target tmux $kind changed during cleanup; inspect $BACKUP_DIR" >&2
		exit 1
	fi
done
if "$TMUX_BIN" list-keys | grep -Fq 'hmux'; then
	echo "hmux: an hmux binding remains on the target server" >&2
	exit 1
fi
for id in $("$TMUX_BIN" list-sessions -F '#{session_id}'); do
	for option in @hmux_profile @hmux_tags @hmux_label @hmux_alias; do
		if [ -n "$("$TMUX_BIN" show-options -qv -t "$id" "$option")" ]; then
			echo "hmux: legacy option $option remains on $id" >&2
			exit 1
		fi
	done
done

echo "hmux: target tmux restored to its native configuration"
echo "hmux: session/window/pane/client identity sets preserved"
echo "hmux: backup: $BACKUP_DIR"
