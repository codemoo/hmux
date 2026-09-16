#!/bin/sh
set -eu

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/../../.." && pwd)"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/hmux-ui-sync-test.XXXXXX")"
trap 'chmod -R u+w "$TEST_ROOT" 2>/dev/null || true; rm -rf "$TEST_ROOT"' EXIT HUP INT TERM
HOME_DIR="$TEST_ROOT/home"
# shellcheck disable=SC2016 # This is the literal shared-zshrc include.
zsh_autostart_include='source "$HOME/Dropbox/dev/hmux/scripts/hmux-zsh-autostart.zsh"'

mkdir -p "$HOME_DIR/.config/hmux" "$HOME_DIR/Dropbox/dev"
printf 'keep-tmux\nsource-file ~/.config/hmux/tmux.conf\n' >"$HOME_DIR/.tmux.conf"
printf 'keep-shared-zsh\n%s\n' "$zsh_autostart_include" >"$HOME_DIR/Dropbox/dev/.zshrc"
printf 'old-managed-tmux\n' >"$HOME_DIR/.config/hmux/tmux.conf"
printf 'old-managed-frame\n' >"$HOME_DIR/.config/hmux/frame.tmux.conf"
printf 'old-managed-frame-ui\n' >"$HOME_DIR/.config/hmux/frame-ui.tmux.conf"
printf 'old-managed-ghostty\n' >"$HOME_DIR/.config/hmux/ghostty.ghostty"

HOME="$HOME_DIR" HMUX_UI_SKIP_GHOSTTY=1 "$ROOT/archive/terminal/scripts/sync-ui-config-macos.sh"

test ! -e "$HOME_DIR/.config/hmux/tmux.conf"
cmp -s "$ROOT/archive/terminal/config/frame.tmux.conf" "$HOME_DIR/.config/hmux/frame.tmux.conf"
cmp -s "$ROOT/archive/terminal/config/frame-ui.tmux.conf" "$HOME_DIR/.config/hmux/frame-ui.tmux.conf"
cmp -s "$ROOT/archive/terminal/config/ghostty.ghostty" "$HOME_DIR/.config/hmux/ghostty.ghostty"
grep -Fqx 'keep-tmux' "$HOME_DIR/.tmux.conf"
grep -Fqx 'keep-shared-zsh' "$HOME_DIR/Dropbox/dev/.zshrc"
test "$(grep -Fxc 'source-file ~/.config/hmux/tmux.conf' "$HOME_DIR/.tmux.conf")" -eq 0
test "$(grep -Fxc "$zsh_autostart_include" "$HOME_DIR/Dropbox/dev/.zshrc")" -eq 0
test "$(find "$HOME_DIR/.config/hmux/backups" -type f -name tmux.conf | wc -l | tr -d ' ')" -eq 1
test "$(find "$HOME_DIR/.config/hmux/backups" -type f -name .tmux.conf | wc -l | tr -d ' ')" -eq 1
test "$(find "$HOME_DIR/.config/hmux/backups" -type f -name frame.tmux.conf | wc -l | tr -d ' ')" -eq 1
test "$(find "$HOME_DIR/.config/hmux/backups" -type f -name frame-ui.tmux.conf | wc -l | tr -d ' ')" -eq 1
test "$(find "$HOME_DIR/.config/hmux/backups" -type f -name ghostty.ghostty | wc -l | tr -d ' ')" -eq 1
test "$(find "$HOME_DIR/.config/hmux/backups" -type f -name shared-zshrc | wc -l | tr -d ' ')" -eq 1

HOME="$HOME_DIR" HMUX_UI_SKIP_GHOSTTY=1 "$ROOT/archive/terminal/scripts/sync-ui-config-macos.sh"
test "$(grep -Fxc 'source-file ~/.config/hmux/tmux.conf' "$HOME_DIR/.tmux.conf")" -eq 0
test "$(grep -Fxc "$zsh_autostart_include" "$HOME_DIR/Dropbox/dev/.zshrc")" -eq 0
test "$(find "$HOME_DIR/.config/hmux/backups" -type f | wc -l | tr -d ' ')" -eq 6

mkdir -p "$HOME_DIR/.config/ghostty" "$TEST_ROOT/bin"
printf '%s\n' \
	'keep-ghostty' \
	"config-file = \"$HOME_DIR/.config/hmux/ghostty.ghostty\"" \
	'config-file = ~/.config/hmux/ghostty.ghostty' \
	>"$HOME_DIR/.config/ghostty/config"
# shellcheck disable=SC2016 # Literal fake Ghostty script.
printf '%s\n' \
	'#!/bin/sh' \
	'case "$1" in' \
	'  +list-fonts) printf "%s\n" "Monatendard Nerd Font Mono" ;;' \
	'  +show-config) exit 0 ;;' \
	'  *) exit 1 ;;' \
	'esac' \
	>"$TEST_ROOT/bin/ghostty"
chmod 700 "$TEST_ROOT/bin/ghostty"
HOME="$HOME_DIR" PATH="$TEST_ROOT/bin:$PATH" "$ROOT/archive/terminal/scripts/sync-ui-config-macos.sh"
grep -Fqx 'keep-ghostty' "$HOME_DIR/.config/ghostty/config"
test "$(grep -Fxc 'config-file = ~/.config/hmux/ghostty.ghostty' "$HOME_DIR/.config/ghostty/config")" -eq 1
test "$(find "$HOME_DIR/.config/hmux/backups" -type f -name ghostty-config | wc -l | tr -d ' ')" -eq 1

FAIL_HOME="$TEST_ROOT/fail-home"
mkdir -p "$FAIL_HOME/.config/hmux" "$FAIL_HOME/.config/ghostty"
cp "$ROOT/archive/terminal/config/ghostty.ghostty" "$FAIL_HOME/.config/hmux/ghostty.ghostty"
printf '%s\n' \
	'keep-invalid-config' \
	'config-file = ~/.config/hmux/ghostty.ghostty' \
	>"$FAIL_HOME/.config/ghostty/config"
cp "$FAIL_HOME/.config/ghostty/config" "$TEST_ROOT/ghostty-config.before"
# shellcheck disable=SC2016 # Literal fake Ghostty script.
printf '%s\n' \
	'#!/bin/sh' \
	'case "$1" in' \
	'  +list-fonts) printf "%s\n" "Monatendard Nerd Font Mono" ;;' \
	'  +show-config) exit 1 ;;' \
	'  *) exit 1 ;;' \
	'esac' \
	>"$TEST_ROOT/bin/ghostty"
if HOME="$FAIL_HOME" PATH="$TEST_ROOT/bin:$PATH" \
	"$ROOT/archive/terminal/scripts/sync-ui-config-macos.sh" >/dev/null 2>&1; then
	echo "UI sync accepted a Ghostty config rejected by Ghostty" >&2
	exit 1
fi
cmp -s "$TEST_ROOT/ghostty-config.before" "$FAIL_HOME/.config/ghostty/config"
cmp -s "$ROOT/archive/terminal/config/ghostty.ghostty" "$FAIL_HOME/.config/hmux/ghostty.ghostty"

ln -s "$ROOT/archive/terminal/config/tmux.conf" "$HOME_DIR/.config/hmux/tmux.conf"
if HOME="$HOME_DIR" HMUX_UI_SKIP_GHOSTTY=1 \
	"$ROOT/archive/terminal/scripts/sync-ui-config-macos.sh" >/dev/null 2>&1; then
	echo "UI sync accepted a symlinked managed target" >&2
	exit 1
fi
