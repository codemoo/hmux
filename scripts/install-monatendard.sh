#!/bin/sh
set -eu

API="https://api.github.com/repos/younjungpark/Monatendard/releases/latest"
MAX_RELEASE_JSON_BYTES=4194304
MAX_CHECKSUM_BYTES=2097152
MAX_FONT_ARCHIVE_BYTES=536870912
MAX_ARCHIVE_ENTRIES=20000
MAX_UNCOMPRESSED_BYTES=1073741824

download_https() {
	maximum="$1"
	url="$2"
	target="$3"
	case "$maximum" in '' | *[!0-9]*) return 1 ;; esac
	case "$url" in https://*) ;; *)
		echo "font download URL must use HTTPS" >&2
		return 1
		;;
	esac
	blocks=$(((maximum + 511) / 512))
	if ! (
		ulimit -f "$blocks"
		exec curl --fail --location --silent --show-error \
			--proto '=https' --tlsv1.2 \
			--connect-timeout 10 --max-time 180 \
			--retry 2 --retry-delay 1 --retry-all-errors \
			--max-filesize "$maximum" \
			"$url" -o "$target"
	); then
		rm -f "$target"
		echo "bounded font download failed" >&2
		return 1
	fi
	actual="$(wc -c <"$target" | tr -d ' ')"
	case "$actual" in '' | *[!0-9]*) return 1 ;; esac
	[ "$actual" -ge 1 ] && [ "$actual" -le "$maximum" ] || {
		rm -f "$target"
		echo "font download exceeded its size limit" >&2
		return 1
	}
}

ghostty_path() {
	if command -v ghostty >/dev/null 2>&1; then
		command -v ghostty
	elif [ -x /Applications/Ghostty.app/Contents/MacOS/ghostty ]; then
		printf '%s\n' /Applications/Ghostty.app/Contents/MacOS/ghostty
	else
		return 1
	fi
}

font_files_ready() {
	font_dir="$HOME/Library/Fonts"
	regular_font="$font_dir/MonatendardNFM-Regular.ttf"
	[ -d "$font_dir" ] && [ ! -L "$font_dir" ] &&
		[ -f "$regular_font" ] && [ ! -L "$regular_font" ]
}

font_ready() {
	font_files_ready || return 1
	if detected_ghostty="$(ghostty_path)"; then
		"$detected_ghostty" +list-fonts 2>/dev/null |
			grep -Fq 'Monatendard Nerd Font Mono'
	fi
}

case "$#:${1:-}" in
0:) ;;
1:--check)
	font_ready
	exit
	;;
*)
	echo "usage: $0 [--check]" >&2
	exit 2
	;;
esac

if font_ready; then
	echo "Monatendard Nerd Font Mono is already installed"
	exit 0
fi

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/hmux-font.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT HUP INT TERM
download_https "$MAX_RELEASE_JSON_BYTES" "$API" "$TMP_DIR/release.json"
PRERELEASE="$(jq -r '.prerelease' "$TMP_DIR/release.json")"
DRAFT="$(jq -r '.draft' "$TMP_DIR/release.json")"
[ "$PRERELEASE" = "false" ] && [ "$DRAFT" = "false" ] || {
	echo "latest release is a prerelease; refusing" >&2
	exit 1
}
if jq -er '[(.name // ""), (.body // "")] | join(" ")' "$TMP_DIR/release.json" |
	grep -Eqi '(do not use|사용[[:space:]]*금지|broken release)'; then
	echo "latest release is marked as unsafe to use; refusing" >&2
	exit 1
fi
URL="$(jq -er '.assets[] | select(.name | test("Desktop-Nerd.*\\.zip$"; "i")) | .browser_download_url' "$TMP_DIR/release.json" | head -1)"
[ -n "$URL" ] || {
	echo "official Nerd Font Mono zip was not found" >&2
	exit 1
}
download_https "$MAX_FONT_ARCHIVE_BYTES" "$URL" "$TMP_DIR/font.zip"
CHECKSUM_URL="$(jq -er '.assets[] | select(.name | test("sha256|checksum"; "i")) | .browser_download_url' "$TMP_DIR/release.json" | head -1 || true)"
[ -n "$CHECKSUM_URL" ] || {
	echo "official checksum asset is unavailable; refusing unverified install" >&2
	exit 1
}
download_https "$MAX_CHECKSUM_BYTES" "$CHECKSUM_URL" "$TMP_DIR/checksums.txt"
EXPECTED="$(grep -F "$(basename "$URL")" "$TMP_DIR/checksums.txt" | awk '{print $1}' | head -1)"
case "$EXPECTED" in '' | *[!0-9A-Fa-f]*) EXPECTED= ;; esac
[ "${#EXPECTED}" -eq 64 ] || EXPECTED=
EXPECTED="$(printf '%s' "$EXPECTED" | tr 'A-F' 'a-f')"
[ -n "$EXPECTED" ] && [ "$(shasum -a 256 "$TMP_DIR/font.zip" | awk '{print $1}')" = "$EXPECTED" ] || {
	echo "font checksum verification failed" >&2
	exit 1
}
if LC_ALL=C unzip -Z1 "$TMP_DIR/font.zip" | awk '
  /^\// || /(^|\/)\.\.(\/|$)/ || index($0, "\\") {unsafe=1}
  END {exit !unsafe}
'; then
	echo "font archive contains an unsafe path" >&2
	exit 1
fi
ENTRY_COUNT="$(LC_ALL=C unzip -Z1 "$TMP_DIR/font.zip" | awk 'END {print NR}')"
UNCOMPRESSED_BYTES="$(LC_ALL=C unzip -Z -t "$TMP_DIR/font.zip" | awk 'END {print $3}')"
case "$ENTRY_COUNT:$UNCOMPRESSED_BYTES" in *[!0-9:]*)
	echo "font archive metadata is invalid" >&2
	exit 1
	;;
esac
[ "$ENTRY_COUNT" -ge 1 ] && [ "$ENTRY_COUNT" -le "$MAX_ARCHIVE_ENTRIES" ] &&
	[ "$UNCOMPRESSED_BYTES" -ge 1 ] && [ "$UNCOMPRESSED_BYTES" -le "$MAX_UNCOMPRESSED_BYTES" ] || {
	echo "font archive expands beyond its safety limits" >&2
	exit 1
}
if LC_ALL=C unzip -Z -l "$TMP_DIR/font.zip" | awk '$1 ~ /^l/ {found=1} END {exit !found}'; then
	echo "font archive contains a symbolic link" >&2
	exit 1
fi
mkdir -m 700 "$TMP_DIR/font"
LC_ALL=C unzip -q "$TMP_DIR/font.zip" -d "$TMP_DIR/font"
if find "$TMP_DIR/font" -type l -print -quit | grep -q .; then
	echo "font archive contains a symbolic link" >&2
	exit 1
fi
[ ! -L "$HOME/Library/Fonts" ] || {
	echo "refusing symlinked user font directory" >&2
	exit 1
}
mkdir -p "$HOME/Library/Fonts"
export HMUX_FONT_DEST="$HOME/Library/Fonts"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)-$$"
export HMUX_FONT_BACKUP="$HOME/.config/hmux/backups/$STAMP/fonts"
find "$TMP_DIR/font" -type f -name '*.ttf' -exec sh -c '
  for source do
    destination="$HMUX_FONT_DEST/$(basename "$source")"
    if [ -L "$destination" ] || { [ -e "$destination" ] && [ ! -f "$destination" ]; }; then
      echo "refusing unsafe font destination: $destination" >&2
      exit 1
    fi
    if [ -f "$destination" ] && cmp -s "$source" "$destination"; then
      continue
    fi
    if [ -f "$destination" ]; then
      mkdir -p "$HMUX_FONT_BACKUP"
      chmod 700 "$(dirname "$HMUX_FONT_BACKUP")" "$HMUX_FONT_BACKUP"
      cp -p "$destination" "$HMUX_FONT_BACKUP/$(basename "$destination")"
    fi
    install -m 644 "$source" "$destination"
  done
' sh {} +
if ! font_files_ready; then
	echo "verified archive did not install MonatendardNFM-Regular.ttf" >&2
	exit 1
fi
if detected_ghostty="$(ghostty_path)" &&
	! "$detected_ghostty" +list-fonts 2>/dev/null | grep -Fq 'Monatendard Nerd Font Mono'; then
	echo "font files installed, but Ghostty does not report Monatendard Nerd Font Mono" >&2
	echo "restart Ghostty/font services and rerun verification before enabling the managed config" >&2
	exit 1
fi
echo "Monatendard fonts installed from verified official release"
