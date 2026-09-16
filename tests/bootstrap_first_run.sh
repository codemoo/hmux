#!/bin/sh
set -eu

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/hmux-bootstrap-test.XXXXXX")"
trap 'chmod -R u+w "$TEST_ROOT" 2>/dev/null || true; rm -rf "$TEST_ROOT"' EXIT HUP INT TERM

PLATFORM="$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)"
[ "$PLATFORM" = "darwin-x86_64" ] && PLATFORM="darwin-amd64"
case "$PLATFORM" in darwin-arm64 | darwin-amd64) ;; *) exit 0 ;; esac

mkdir -p "$TEST_ROOT/home/.config/hmux" "$TEST_ROOT/store" "$TEST_ROOT/bin"
GOPATH="${GOPATH:-$TEST_ROOT/go}" GOCACHE="${GOCACHE:-$TEST_ROOT/go-cache}" \
	go build -trimpath -o "$TEST_ROOT/hmux-control" "$ROOT/cmd/hmux-control"
"$TEST_ROOT/hmux-control" --root "$TEST_ROOT/store" keygen \
	--private "$TEST_ROOT/private.pem" \
	--public "$TEST_ROOT/home/.config/hmux/release-public-key.pem"

printf '%s\n' '#!/bin/sh' 'printf "bootstrap-first-run-ok\\n"' >"$TEST_ROOT/artifact"
chmod 700 "$TEST_ROOT/artifact"
"$TEST_ROOT/hmux-control" --root "$TEST_ROOT/store" publish \
	--version 0.1.3 \
	--signing-key "$TEST_ROOT/private.pem" \
	--artifact "$PLATFORM=$TEST_ROOT/artifact"

# shellcheck disable=SC2016 # These are literal lines of the fake ssh fixture.
printf '%s\n' \
	'#!/bin/sh' \
	'set -eu' \
	'while [ "$1" != "--" ]; do shift; done' \
	'shift' \
	'shift' \
	'if [ "${HMUX_TEST_OVERSIZE_MANIFEST:-0}" = 1 ] && [ "${1:-}" = manifest ]; then' \
	'  yes x | head -c 1048577' \
	'  exit 0' \
	'fi' \
	'if [ "${HMUX_TEST_OVERSIZE_ARTIFACT:-0}" = 1 ] && [ "${1:-}" = artifact ]; then' \
	'  yes x | head -c 1048577' \
	'  exit 0' \
	'fi' \
	'if [ "${HMUX_TEST_STALLED_MANIFEST:-0}" = 1 ] && [ "${1:-}" = manifest ]; then' \
	'  exec sleep 60' \
	'fi' \
	'if [ "${HMUX_TEST_UNKNOWN_MANIFEST_FIELD:-0}" = 1 ] && [ "${1:-}" = manifest ]; then' \
	'  "$HMUX_TEST_CONTROL" --root "$HMUX_TEST_STORE" "$@" | jq ". + {unexpected:true}"' \
	'  exit 0' \
	'fi' \
	'exec "$HMUX_TEST_CONTROL" --root "$HMUX_TEST_STORE" "$@"' \
	>"$TEST_ROOT/bin/ssh"
chmod 700 "$TEST_ROOT/bin/ssh"

OUTPUT="$(
	HOME="$TEST_ROOT/home" \
		PATH="$TEST_ROOT/bin:$PATH" \
		HMUX_TEST_CONTROL="$TEST_ROOT/hmux-control" \
		HMUX_TEST_STORE="$TEST_ROOT/store" \
		HMUX_CACHE_DIR="$TEST_ROOT/home/.cache/hmux" \
		HMUX_DMZ_ALIAS="hmux-test-dmz" \
		HMUX_CONTROL_PATH="hmux-control" \
		"$ROOT/scripts/hmux-bootstrap"
)"
[ "$OUTPUT" = "bootstrap-first-run-ok" ]
[ -x "$TEST_ROOT/home/.cache/hmux/releases/0.1.3/hmux" ]
[ -f "$TEST_ROOT/home/.cache/hmux/releases/0.1.3/manifest.json" ]

rm "$TEST_ROOT/home/.cache/hmux/current"
OUTPUT="$(
	HOME="$TEST_ROOT/home" \
		PATH="$TEST_ROOT/bin:$PATH" \
		HMUX_TEST_CONTROL="$TEST_ROOT/hmux-control" \
		HMUX_TEST_STORE="$TEST_ROOT/store" \
		HMUX_CACHE_DIR="$TEST_ROOT/home/.cache/hmux" \
		HMUX_DMZ_ALIAS="hmux-test-dmz" \
		HMUX_CONTROL_PATH="hmux-control" \
		"$ROOT/scripts/hmux-bootstrap"
)"
[ "$OUTPUT" = "bootstrap-first-run-ok" ]

printf 'tampered\n' >"$TEST_ROOT/home/.cache/hmux/releases/0.1.3/hmux"
chmod 700 "$TEST_ROOT/home/.cache/hmux/releases/0.1.3/hmux"
rm "$TEST_ROOT/home/.cache/hmux/current"
if HOME="$TEST_ROOT/home" \
	PATH="$TEST_ROOT/bin:$PATH" \
	HMUX_TEST_CONTROL="$TEST_ROOT/hmux-control" \
	HMUX_TEST_STORE="$TEST_ROOT/store" \
	HMUX_CACHE_DIR="$TEST_ROOT/home/.cache/hmux" \
	HMUX_DMZ_ALIAS="hmux-test-dmz" \
	HMUX_CONTROL_PATH="hmux-control" \
	"$ROOT/scripts/hmux-bootstrap" >/dev/null 2>&1; then
	echo "bootstrap replaced an immutable tampered cache" >&2
	exit 1
fi
[ "$(cat "$TEST_ROOT/home/.cache/hmux/releases/0.1.3/hmux")" = "tampered" ]

rm -rf "$TEST_ROOT/home/.cache/hmux"
if HOME="$TEST_ROOT/home" \
	PATH="$TEST_ROOT/bin:$PATH" \
	HMUX_TEST_CONTROL="$TEST_ROOT/hmux-control" \
	HMUX_TEST_STORE="$TEST_ROOT/store" \
	HMUX_TEST_OVERSIZE_MANIFEST=1 \
	HMUX_CACHE_DIR="$TEST_ROOT/home/.cache/hmux" \
	HMUX_DMZ_ALIAS="hmux-test-dmz" \
	HMUX_CONTROL_PATH="hmux-control" \
	"$ROOT/scripts/hmux-bootstrap" >/dev/null 2>&1; then
	echo "bootstrap accepted an oversized manifest" >&2
	exit 1
fi
[ ! -e "$TEST_ROOT/home/.cache/hmux/current" ]

rm -rf "$TEST_ROOT/home/.cache/hmux"
if HOME="$TEST_ROOT/home" \
	PATH="$TEST_ROOT/bin:$PATH" \
	HMUX_TEST_CONTROL="$TEST_ROOT/hmux-control" \
	HMUX_TEST_STORE="$TEST_ROOT/store" \
	HMUX_TEST_STALLED_MANIFEST=1 \
	HMUX_BOOTSTRAP_DOWNLOAD_TIMEOUT_SECONDS=1 \
	HMUX_CACHE_DIR="$TEST_ROOT/home/.cache/hmux" \
	HMUX_DMZ_ALIAS="hmux-test-dmz" \
	HMUX_CONTROL_PATH="hmux-control" \
	"$ROOT/scripts/hmux-bootstrap" >/dev/null 2>&1; then
	echo "bootstrap accepted a stalled manifest command" >&2
	exit 1
fi
[ ! -e "$TEST_ROOT/home/.cache/hmux/current" ]

rm -rf "$TEST_ROOT/home/.cache/hmux"
if HOME="$TEST_ROOT/home" \
	PATH="$TEST_ROOT/bin:$PATH" \
	HMUX_TEST_CONTROL="$TEST_ROOT/hmux-control" \
	HMUX_TEST_STORE="$TEST_ROOT/store" \
	HMUX_TEST_UNKNOWN_MANIFEST_FIELD=1 \
	HMUX_CACHE_DIR="$TEST_ROOT/home/.cache/hmux" \
	HMUX_DMZ_ALIAS="hmux-test-dmz" \
	HMUX_CONTROL_PATH="hmux-control" \
	"$ROOT/scripts/hmux-bootstrap" >/dev/null 2>&1; then
	echo "bootstrap accepted an unknown manifest field" >&2
	exit 1
fi
[ ! -e "$TEST_ROOT/home/.cache/hmux/current" ]

rm -rf "$TEST_ROOT/home/.cache/hmux"
if HOME="$TEST_ROOT/home" \
	PATH="$TEST_ROOT/bin:$PATH" \
	HMUX_TEST_CONTROL="$TEST_ROOT/hmux-control" \
	HMUX_TEST_STORE="$TEST_ROOT/store" \
	HMUX_TEST_OVERSIZE_ARTIFACT=1 \
	HMUX_CACHE_DIR="$TEST_ROOT/home/.cache/hmux" \
	HMUX_DMZ_ALIAS="hmux-test-dmz" \
	HMUX_CONTROL_PATH="hmux-control" \
	"$ROOT/scripts/hmux-bootstrap" >/dev/null 2>&1; then
	echo "bootstrap accepted an oversized artifact" >&2
	exit 1
fi
[ ! -e "$TEST_ROOT/home/.cache/hmux/current" ]
