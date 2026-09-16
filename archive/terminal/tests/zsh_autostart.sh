#!/bin/sh
set -eu

[ "$(uname -s)" = Darwin ] || exit 0

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/../../.." && pwd)"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/hmux-zsh-autostart-test.XXXXXX")"
trap 'rm -rf "$TEST_ROOT"' EXIT HUP INT TERM
AUTOSTART="$ROOT/archive/terminal/scripts/hmux-zsh-autostart.zsh"

run_interactive_case() {
	name="$1"
	term_program="$2"
	tmux_value="$3"
	log="$TEST_ROOT/$name.log"
	: >"$log"
	TERM_PROGRAM="$term_program" TMUX="$tmux_value" \
		HMUX_TEST_LOG="$log" HMUX_AUTOSTART_FILE="$AUTOSTART" \
		zsh -fic '
			hmux() { print -r -- launched >>"$HMUX_TEST_LOG"; }
			source "$HMUX_AUTOSTART_FILE"
			source "$HMUX_AUTOSTART_FILE"
			exit
		' >/dev/null 2>&1
	wc -l <"$log" | tr -d ' '
}

test "$(run_interactive_case ghostty ghostty '')" -eq 0
test "$(run_interactive_case tmux ghostty /tmp/hmux-e2e-tmux,1,0)" -eq 0
test "$(run_interactive_case terminal Apple_Terminal '')" -eq 0

noninteractive_log="$TEST_ROOT/noninteractive.log"
: >"$noninteractive_log"
TERM_PROGRAM=ghostty TMUX='' HMUX_TEST_LOG="$noninteractive_log" \
	HMUX_AUTOSTART_FILE="$AUTOSTART" \
	zsh -fc '
		hmux() { print -r -- launched >>"$HMUX_TEST_LOG"; }
		source "$HMUX_AUTOSTART_FILE"
	'
test ! -s "$noninteractive_log"
