#!/bin/sh
set -eu

STAMP="$(date -u +%Y%m%dT%H%M%SZ)-$$"
TRASH="$HOME/.Trash/hmux-uninstall-$STAMP"
mkdir -p "$TRASH"

remove_managed_line() {
	path="$1"
	label="$2"
	line="$3"
	[ -e "$path" ] || [ -L "$path" ] || return 0
	if [ -L "$path" ] || [ ! -f "$path" ]; then
		echo "hmux: refusing non-regular managed config: $path" >&2
		exit 1
	fi
	grep -Fqx "$line" "$path" || return 0
	cp -p "$path" "$TRASH/$label.before"
	tmp="$(mktemp "$(dirname "$path")/.hmux-uninstall.XXXXXX")"
	trap 'rm -f "$tmp"' EXIT HUP INT TERM
	awk -v managed="$line" '$0 != managed' "$path" >"$tmp"
	chmod "$(stat -f '%Lp' "$path")" "$tmp"
	mv "$tmp" "$path"
	trap - EXIT HUP INT TERM
}

remove_managed_line "$HOME/.tmux.conf" "tmux.conf" 'source-file ~/.config/hmux/tmux.conf'
remove_managed_line "$HOME/.config/ghostty/config" "ghostty-xdg-config" 'config-file = ~/.config/hmux/ghostty.ghostty'
remove_managed_line "$HOME/Library/Application Support/com.mitchellh.ghostty/config" "ghostty-app-support-config" 'config-file = ~/.config/hmux/ghostty.ghostty'
# shellcheck disable=SC2016 # Keep $HOME literal in the shared cross-Mac zsh file.
remove_managed_line "$HOME/Dropbox/dev/.zshrc" "shared-zshrc" \
	'source "$HOME/Dropbox/dev/hmux/scripts/hmux-zsh-autostart.zsh"'

move_to_trash() {
	path="$1"
	label="$2"
	[ -e "$path" ] || [ -L "$path" ] || return 0
	mv "$path" "$TRASH/$label"
}

move_to_trash "$HOME/.local/bin/hmux" "bin-hmux"
move_to_trash "$HOME/.local/bin/hmux-agent" "bin-hmux-agent"
move_to_trash "$HOME/.local/bin/hmux-control" "bin-hmux-control"
move_to_trash "$HOME/.cache/hmux" "cache"
move_to_trash "$HOME/.config/hmux" "config"
move_to_trash "$HOME/.ssh/config.d/50-hmux.generated.conf" "ssh-fragment.conf"
echo "hmux files moved to recoverable location: $TRASH"
echo "Managed tmux/Ghostty include lines were removed without reloading tmux."
echo "The generic SSH config.d Include was preserved; only the hmux fragment was moved."
