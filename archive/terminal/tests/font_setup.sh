#!/bin/sh
set -eu

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/../../.." && pwd)"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/hmux-font-test.XXXXXX")"
trap 'rm -rf "$TEST_ROOT"' EXIT HUP INT TERM
HOME_DIR="$TEST_ROOT/home"
BIN_DIR="$TEST_ROOT/bin"
mkdir -p "$HOME_DIR/Library/Fonts" "$BIN_DIR"

# shellcheck disable=SC2016 # Literal fake Ghostty script.
printf '%s\n' \
	'#!/bin/sh' \
	'[ "${1:-}" = +list-fonts ] || exit 1' \
	'printf "%s\n" "Monatendard Nerd Font Mono"' \
	>"$BIN_DIR/ghostty"
chmod 700 "$BIN_DIR/ghostty"

FONT="$HOME_DIR/Library/Fonts/MonatendardNFM-Regular.ttf"
: >"$FONT"
HOME="$HOME_DIR" PATH="$BIN_DIR:/usr/bin:/bin" \
	"$ROOT/scripts/install-monatendard.sh" --check

# A ready installation is a network-free no-op even in normal install mode.
printf '%s\n' '#!/bin/sh' 'exit 97' >"$BIN_DIR/curl"
chmod 700 "$BIN_DIR/curl"
HOME="$HOME_DIR" PATH="$BIN_DIR:/usr/bin:/bin" \
	"$ROOT/scripts/install-monatendard.sh" >/dev/null

# Exercise the complete verified install path with a local curl fixture.
INSTALL_HOME="$TEST_ROOT/install-home"
INSTALL_BIN="$TEST_ROOT/install-bin"
ARCHIVE_ROOT="$TEST_ROOT/archive-root"
mkdir -p "$INSTALL_HOME" "$INSTALL_BIN" "$ARCHIVE_ROOT/Monatendard"
printf '%s\n' 'fixture-regular-font' \
	>"$ARCHIVE_ROOT/Monatendard/MonatendardNFM-Regular.ttf"
printf '%s\n' 'fixture-bold-font' \
	>"$ARCHIVE_ROOT/Monatendard/MonatendardNFM-Bold.ttf"
(cd "$ARCHIVE_ROOT" && zip -qr "$TEST_ROOT/font.zip" Monatendard)
FONT_SHA="$(shasum -a 256 "$TEST_ROOT/font.zip" | awk '{print $1}')"
printf '%s  %s\n' "$FONT_SHA" 'Monatendard-Desktop-Nerd.zip' \
	>"$TEST_ROOT/checksums.txt"
printf '%s\n' \
	'{' \
	'  "prerelease": false,' \
	'  "draft": false,' \
	'  "name": "stable",' \
	'  "body": "verified fixture",' \
	'  "assets": [' \
	'    {"name":"Monatendard-Desktop-Nerd.zip","browser_download_url":"https://fixture.invalid/Monatendard-Desktop-Nerd.zip"},' \
	'    {"name":"checksums.txt","browser_download_url":"https://fixture.invalid/checksums.txt"}' \
	'  ]' \
	'}' \
	>"$TEST_ROOT/release.json"
# shellcheck disable=SC2016 # Literal fake curl script reads its own environment.
printf '%s\n' \
	'#!/bin/sh' \
	'set -eu' \
	'output=""' \
	'url=""' \
	'while [ "$#" -gt 0 ]; do' \
	'  case "$1" in' \
	'    -o) output="$2"; shift 2 ;;' \
	'    -*) shift ;;' \
	'    *) url="$1"; shift ;;' \
	'  esac' \
	'done' \
	'case "$url" in' \
	'  https://api.github.com/repos/younjungpark/Monatendard/releases/latest) source="$HMUX_TEST_RELEASE" ;;' \
	'  https://fixture.invalid/Monatendard-Desktop-Nerd.zip) source="$HMUX_TEST_FONT_ZIP" ;;' \
	'  https://fixture.invalid/checksums.txt) source="$HMUX_TEST_CHECKSUMS" ;;' \
	'  *) exit 95 ;;' \
	'esac' \
	'cp "$source" "$output"' \
	>"$INSTALL_BIN/curl"
cp "$BIN_DIR/ghostty" "$INSTALL_BIN/ghostty"
chmod 700 "$INSTALL_BIN/curl" "$INSTALL_BIN/ghostty"
HOME="$INSTALL_HOME" PATH="$INSTALL_BIN:/usr/bin:/bin" \
	HMUX_TEST_RELEASE="$TEST_ROOT/release.json" \
	HMUX_TEST_FONT_ZIP="$TEST_ROOT/font.zip" \
	HMUX_TEST_CHECKSUMS="$TEST_ROOT/checksums.txt" \
	"$ROOT/scripts/install-monatendard.sh" >/dev/null
cmp -s "$ARCHIVE_ROOT/Monatendard/MonatendardNFM-Regular.ttf" \
	"$INSTALL_HOME/Library/Fonts/MonatendardNFM-Regular.ttf"
test ! -L "$INSTALL_HOME/Library/Fonts"
HOME="$INSTALL_HOME" PATH="$INSTALL_BIN:/usr/bin:/bin" \
	"$ROOT/scripts/install-monatendard.sh" --check

rm "$FONT"
if HOME="$HOME_DIR" PATH="$BIN_DIR:/usr/bin:/bin" \
	"$ROOT/scripts/install-monatendard.sh" --check; then
	echo "font check accepted a missing regular font" >&2
	exit 1
fi

REAL_FONT="$TEST_ROOT/real-font.ttf"
: >"$REAL_FONT"
ln -s "$REAL_FONT" "$FONT"
if HOME="$HOME_DIR" PATH="$BIN_DIR:/usr/bin:/bin" \
	"$ROOT/scripts/install-monatendard.sh" --check; then
	echo "font check accepted a symlinked regular font" >&2
	exit 1
fi
rm "$FONT"

rm -rf "$HOME_DIR/Library/Fonts"
mkdir "$TEST_ROOT/font-directory"
ln -s "$TEST_ROOT/font-directory" "$HOME_DIR/Library/Fonts"
: >"$TEST_ROOT/font-directory/MonatendardNFM-Regular.ttf"
if HOME="$HOME_DIR" PATH="$BIN_DIR:/usr/bin:/bin" \
	"$ROOT/scripts/install-monatendard.sh" --check; then
	echo "font check accepted a symlinked font directory" >&2
	exit 1
fi
rm "$HOME_DIR/Library/Fonts"
mkdir "$HOME_DIR/Library/Fonts"
: >"$FONT"

# Ghostty must report the configured family when Ghostty is installed.
printf '%s\n' '#!/bin/sh' 'exit 0' >"$BIN_DIR/ghostty"
chmod 700 "$BIN_DIR/ghostty"
if HOME="$HOME_DIR" PATH="$BIN_DIR:/usr/bin:/bin" \
	"$ROOT/scripts/install-monatendard.sh" --check; then
	echo "font check accepted a family not reported by Ghostty" >&2
	exit 1
fi

grep -Fq '/Applications/Ghostty.app/Contents/MacOS/ghostty' \
	"$ROOT/scripts/install-monatendard.sh"

# The ordinary entrypoint attempts verified font installation before UI sync.
ENTRY_HOME="$TEST_ROOT/entry-home"
ENTRY_BIN="$TEST_ROOT/entry-bin"
mkdir -p "$ENTRY_HOME" "$ENTRY_BIN"
for tool in jq fzf; do
	printf '%s\n' '#!/bin/sh' 'exit 0' >"$ENTRY_BIN/$tool"
	chmod 700 "$ENTRY_BIN/$tool"
done
# shellcheck disable=SC2016 # Literal fake curl script reads its own environment.
printf '%s\n' \
	'#!/bin/sh' \
	'printf "%s\n" invoked >"$HMUX_TEST_CURL_MARKER"' \
	'exit 96' \
	>"$ENTRY_BIN/curl"
chmod 700 "$ENTRY_BIN/curl"
if HOME="$ENTRY_HOME" PATH="$ENTRY_BIN:/usr/bin:/bin" \
	HMUX_TEST_CURL_MARKER="$TEST_ROOT/curl-invoked" \
	"$ROOT/archive/terminal/scripts/hmux-entrypoint.sh" >"$TEST_ROOT/entry.out" 2>"$TEST_ROOT/entry.err"; then
	echo "entrypoint unexpectedly survived a failed font download" >&2
	exit 1
fi
test -f "$TEST_ROOT/curl-invoked"
grep -Fq 'hmux: installing verified Monatendard Nerd Font Mono' \
	"$TEST_ROOT/entry.err"

font_line="$(grep -nF 'install-monatendard.sh" --check' \
	"$ROOT/archive/terminal/scripts/hmux-entrypoint.sh" | cut -d: -f1 | head -1)"
ui_line="$(grep -nF 'sync-ui-config-macos.sh' \
	"$ROOT/archive/terminal/scripts/hmux-entrypoint.sh" | cut -d: -f1 | head -1)"
test "$font_line" -lt "$ui_line"
