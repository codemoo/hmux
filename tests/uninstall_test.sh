#!/bin/sh
set -eu

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/hmux-uninstall-test.XXXXXX")"
trap 'rm -rf "$TEST_ROOT"' EXIT HUP INT TERM

MAC_HOME="$TEST_ROOT/mac-home"
mkdir -p \
	"$MAC_HOME/.local/bin" \
	"$MAC_HOME/.cache/hmux" \
	"$MAC_HOME/.config/hmux" \
	"$MAC_HOME/.config/ghostty" \
	"$MAC_HOME/.ssh/config.d" \
	"$MAC_HOME/Dropbox/dev"
printf 'binary\n' >"$MAC_HOME/.local/bin/hmux"
printf 'agent\n' >"$MAC_HOME/.local/bin/hmux-agent"
printf 'control\n' >"$MAC_HOME/.local/bin/hmux-control"
printf 'source-file ~/.config/hmux/tmux.conf\nkeep-tmux\n' >"$MAC_HOME/.tmux.conf"
printf 'config-file = ~/.config/hmux/ghostty.ghostty\nkeep-ghostty\n' >"$MAC_HOME/.config/ghostty/config"
# shellcheck disable=SC2016 # Keep the shared cross-Mac $HOME expression literal.
printf 'source "$HOME/Dropbox/dev/hmux/scripts/hmux-zsh-autostart.zsh"\nkeep-zsh\n' >"$MAC_HOME/Dropbox/dev/.zshrc"
printf 'fragment\n' >"$MAC_HOME/.ssh/config.d/50-hmux.generated.conf"
HOME="$MAC_HOME" "$ROOT/scripts/uninstall-macos.sh" >/dev/null
TRASH="$(find "$MAC_HOME/.Trash" -mindepth 1 -maxdepth 1 -type d | head -1)"
[ -f "$TRASH/bin-hmux" ]
[ -d "$TRASH/cache" ]
[ -d "$TRASH/config" ]
[ -f "$TRASH/ssh-fragment.conf" ]
grep -Fq 'keep-tmux' "$MAC_HOME/.tmux.conf"
if grep -Fq 'source-file ~/.config/hmux/tmux.conf' "$MAC_HOME/.tmux.conf"; then exit 1; fi
grep -Fq 'keep-ghostty' "$MAC_HOME/.config/ghostty/config"
if grep -Fq 'config-file = ~/.config/hmux/ghostty.ghostty' "$MAC_HOME/.config/ghostty/config"; then exit 1; fi
grep -Fq 'keep-zsh' "$MAC_HOME/Dropbox/dev/.zshrc"
if grep -Fq 'hmux-zsh-autostart.zsh' "$MAC_HOME/Dropbox/dev/.zshrc"; then exit 1; fi

DMZ_HOME="$TEST_ROOT/dmz-home"
mkdir -p "$DMZ_HOME/.local/bin" "$DMZ_HOME/.local/share/hmux-control" "$DMZ_HOME/.config/systemd/user" "$TEST_ROOT/bin"
printf 'control\n' >"$DMZ_HOME/.local/bin/hmux-control"
printf 'state\n' >"$DMZ_HOME/.local/share/hmux-control/state"
for unit in hmux-reconcile.service hmux-reconcile.timer hmux-health.service hmux-health.timer; do
	printf 'unit\n' >"$DMZ_HOME/.config/systemd/user/$unit"
done
printf '%s\n' '#!/bin/sh' 'exit 0' >"$TEST_ROOT/bin/systemctl"
chmod 700 "$TEST_ROOT/bin/systemctl"
HOME="$DMZ_HOME" PATH="$TEST_ROOT/bin:$PATH" "$ROOT/scripts/uninstall-dmz.sh" >/dev/null
DMZ_TRASH="$(find "$DMZ_HOME/.local/share" -mindepth 1 -maxdepth 1 -type d -name 'hmux-uninstall-*' | head -1)"
[ -f "$DMZ_TRASH/bin-hmux-control" ]
[ -d "$DMZ_TRASH/control-data" ]
