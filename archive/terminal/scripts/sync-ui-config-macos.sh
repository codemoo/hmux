#!/bin/sh
set -eu

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/../../.." && pwd)"
CONFIG_DIR="$HOME/.config/hmux"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)-$$"
BACKUP_DIR="$CONFIG_DIR/backups/$STAMP"
backup_ready=0
managed_ghostty_changed=0
ghostty_config_changed=0

ensure_backup_dir() {
	if [ "$backup_ready" -eq 0 ]; then
		mkdir -p "$BACKUP_DIR"
		chmod 700 "$BACKUP_DIR"
		backup_ready=1
	fi
}

check_target() {
	path="$1"
	if [ -L "$path" ]; then
		echo "hmux: refusing symlinked UI config target: $path" >&2
		exit 1
	fi
	if [ -e "$path" ] && [ ! -f "$path" ]; then
		echo "hmux: UI config target is not a regular file: $path" >&2
		exit 1
	fi
}

backup_file() {
	path="$1"
	label="$2"
	[ -e "$path" ] || return 0
	ensure_backup_dir
	cp -p "$path" "$BACKUP_DIR/$label"
}

install_managed() {
	source="$1"
	target="$2"
	label="$3"
	check_target "$target"
	if [ -f "$target" ] && cmp -s "$source" "$target"; then
		return
	fi
	backup_file "$target" "$label"
	mkdir -p "$(dirname "$target")"
	tmp="$(mktemp "$(dirname "$target")/.hmux-ui.XXXXXX")"
	trap 'rm -f "$tmp"' EXIT HUP INT TERM
	install -m 600 "$source" "$tmp"
	mv "$tmp" "$target"
	if [ "$label" = "ghostty.ghostty" ]; then
		managed_ghostty_changed=1
	fi
	trap - EXIT HUP INT TERM
}

ensure_include() {
	target="$1"
	line="$2"
	label="$3"
	check_target "$target"
	mkdir -p "$(dirname "$target")"
	if [ ! -e "$target" ]; then
		touch "$target"
		chmod 600 "$target"
	fi
	if grep -Fqx "$line" "$target"; then
		return
	fi
	backup_file "$target" "$label"
	tmp="$(mktemp "$(dirname "$target")/.hmux-include.XXXXXX")"
	trap 'rm -f "$tmp"' EXIT HUP INT TERM
	{
		cat "$target"
		printf '\n%s\n' "$line"
	} >"$tmp"
	chmod 600 "$tmp"
	mv "$tmp" "$target"
	if [ "$label" = "ghostty-config" ]; then
		ghostty_config_changed=1
	fi
	trap - EXIT HUP INT TERM
}

remove_include() {
	target="$1"
	line="$2"
	label="$3"
	check_target "$target"
	[ -e "$target" ] || return 0
	grep -Fqx "$line" "$target" || return 0
	backup_file "$target" "$label"
	tmp="$(mktemp "$(dirname "$target")/.hmux-remove-include.XXXXXX")"
	trap 'rm -f "$tmp"' EXIT HUP INT TERM
	awk -v managed="$line" '$0 != managed' "$target" >"$tmp"
	chmod 600 "$tmp"
	mv "$tmp" "$target"
	if [ "$label" = "ghostty-config" ]; then
		ghostty_config_changed=1
	fi
	trap - EXIT HUP INT TERM
}

retire_managed_file() {
	target="$1"
	label="$2"
	check_target "$target"
	[ -e "$target" ] || return 0
	backup_file "$target" "$label"
	rm -f "$target"
}

normalize_ghostty_include() {
	target="$1"
	line='config-file = ~/.config/hmux/ghostty.ghostty'
	absolute="$CONFIG_DIR/ghostty.ghostty"
	check_target "$target"
	[ -e "$target" ] || return 0
	tmp="$(mktemp "$(dirname "$target")/.hmux-ghostty-normalize.XXXXXX")"
	trap 'rm -f "$tmp"' EXIT HUP INT TERM
	awk -v canonical="$line" -v absolute="$absolute" '
		function trim(value) {
			gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
			return value
		}
		{
			value = $0
			if (value ~ /^[[:space:]]*config-file[[:space:]]*=/) {
				sub(/^[^=]*=/, "", value)
				value = trim(value)
				if (value ~ /^".*"$/) {
					value = substr(value, 2, length(value) - 2)
				}
				if (value == "~/.config/hmux/ghostty.ghostty" || value == absolute) {
					if (!seen) {
						print canonical
						seen = 1
					}
					next
				}
			}
			print
		}
	' "$target" >"$tmp"
	if cmp -s "$target" "$tmp"; then
		rm -f "$tmp"
		trap - EXIT HUP INT TERM
		return 0
	fi
	backup_file "$target" "ghostty-config"
	chmod 600 "$tmp"
	mv "$tmp" "$target"
	trap - EXIT HUP INT TERM
}

mkdir -p "$CONFIG_DIR"
chmod 700 "$CONFIG_DIR"
install_managed "$ROOT/archive/terminal/config/frame.tmux.conf" "$CONFIG_DIR/frame.tmux.conf" "frame.tmux.conf"
install_managed "$ROOT/archive/terminal/config/frame-ui.tmux.conf" "$CONFIG_DIR/frame-ui.tmux.conf" "frame-ui.tmux.conf"
install_managed "$ROOT/archive/terminal/config/ghostty.ghostty" "$CONFIG_DIR/ghostty.ghostty" "ghostty.ghostty"
remove_include "$HOME/.tmux.conf" 'source-file ~/.config/hmux/tmux.conf' ".tmux.conf"
retire_managed_file "$CONFIG_DIR/tmux.conf" "tmux.conf"
if [ -f "$HOME/Dropbox/dev/.zshrc" ]; then
	# shellcheck disable=SC2016 # Keep $HOME for expansion by each Mac's zsh.
	zsh_autostart_include='source "$HOME/Dropbox/dev/hmux/scripts/hmux-zsh-autostart.zsh"'
	remove_include "$HOME/Dropbox/dev/.zshrc" \
		"$zsh_autostart_include" \
		"shared-zshrc"
fi

if [ "${HMUX_UI_SKIP_GHOSTTY:-0}" = "1" ]; then
	exit 0
fi

GHOSTTY=""
if command -v ghostty >/dev/null 2>&1; then
	GHOSTTY="$(command -v ghostty)"
elif [ -x /Applications/Ghostty.app/Contents/MacOS/ghostty ]; then
	GHOSTTY="/Applications/Ghostty.app/Contents/MacOS/ghostty"
fi
[ -n "$GHOSTTY" ] || exit 0

if ! "$GHOSTTY" +list-fonts 2>/dev/null | grep -Fq 'Monatendard Nerd Font Mono'; then
	exit 0
fi

GHOSTTY_CONFIG="$HOME/.config/ghostty/config"
if [ -e "$HOME/Library/Application Support/com.mitchellh.ghostty/config" ]; then
	GHOSTTY_CONFIG="$HOME/Library/Application Support/com.mitchellh.ghostty/config"
fi
ghostty_config_existed=0
if [ -e "$GHOSTTY_CONFIG" ]; then
	ghostty_config_existed=1
fi
normalize_ghostty_include "$GHOSTTY_CONFIG"
ensure_include "$GHOSTTY_CONFIG" 'config-file = ~/.config/hmux/ghostty.ghostty' "ghostty-config"
if ! "$GHOSTTY" +show-config >/dev/null 2>&1; then
	if [ "$managed_ghostty_changed" -eq 1 ]; then
		if [ -e "$BACKUP_DIR/ghostty.ghostty" ]; then
			cp -p "$BACKUP_DIR/ghostty.ghostty" "$CONFIG_DIR/ghostty.ghostty"
		else
			rm -f "$CONFIG_DIR/ghostty.ghostty"
		fi
	fi
	if [ "$ghostty_config_changed" -eq 1 ]; then
		if [ "$ghostty_config_existed" -eq 1 ] &&
			[ -e "$BACKUP_DIR/ghostty-config" ]; then
			cp -p "$BACKUP_DIR/ghostty-config" "$GHOSTTY_CONFIG"
		else
			rm -f "$GHOSTTY_CONFIG"
		fi
	fi
	echo "hmux: Ghostty rejected the managed UI include; previous config restored" >&2
	exit 1
fi
