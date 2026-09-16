#!/bin/sh
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
VERSION_FILE="$ROOT/VERSION"
SOURCE_ALIAS=${HMUX_APP_SOURCE_ALIAS:-hmux-home}
INSTALL_ROOT="$HOME/Applications"
LOCAL_BUILD=0
SYSTEM_INSTALL=0
MAX_ARCHIVE_BYTES=$((128 * 1024 * 1024))
TEMP_PARENT=${TMPDIR:-/tmp}
TEMP_PARENT=${TEMP_PARENT%/}

fail() {
	printf 'hmux: %s\n' "$*" >&2
	exit 1
}

while [ "$#" -gt 0 ]; do
	case "$1" in
	--local)
		[ "$LOCAL_BUILD" -eq 0 ] || fail "duplicate --local"
		LOCAL_BUILD=1
		;;
	--system)
		[ "$SYSTEM_INSTALL" -eq 0 ] || fail "duplicate --system"
		SYSTEM_INSTALL=1
		;;
	*) fail "usage: install-hmux-app.sh [--local [--system]]" ;;
	esac
	shift
done
[ "$SYSTEM_INSTALL" -eq 0 ] || [ "$LOCAL_BUILD" -eq 1 ] || fail "--system requires --local"
if [ "$SYSTEM_INSTALL" -eq 1 ]; then INSTALL_ROOT=/Applications; fi
TARGET_APP="$INSTALL_ROOT/HMux.app"

case "$(uname -s):$(uname -m)" in
Darwin:arm64) ;;
*) fail "the native app installer requires an arm64 Mac" ;;
esac

[ -f "$VERSION_FILE" ] && [ ! -L "$VERSION_FILE" ] || fail "source VERSION is unavailable"
IFS= read -r version <"$VERSION_FILE"
case "$version" in
[0-9]*.[0-9]*.[0-9]*) ;;
*) fail "source VERSION is invalid" ;;
esac
case "$version" in *[!0-9.]* | *.*.*.*) fail "source VERSION is invalid" ;; esac

case "$SOURCE_ALIAS" in
'' | -* | *[!A-Za-z0-9._-]*) fail "HMUX_APP_SOURCE_ALIAS is invalid" ;;
esac
case "$TEMP_PARENT" in /*) ;; *) fail "TMPDIR must be absolute" ;; esac

for required in scp ditto codesign shasum uuidgen pgrep ps stat; do
	command -v "$required" >/dev/null 2>&1 || fail "$required is required"
done
[ -x /usr/libexec/PlistBuddy ] || fail "PlistBuddy is required"
[ -x /usr/bin/zipinfo ] || fail "zipinfo is required"
[ -x /usr/bin/open ] || fail "open is required"

temp_root=$(mktemp -d "$TEMP_PARENT/hmux-app-install.XXXXXX")
case "$temp_root" in
"$TEMP_PARENT"/hmux-app-install.*) ;;
*) fail "mktemp returned an unsafe path" ;;
esac
chmod 700 "$temp_root"
archive="$temp_root/HMux-$version-macOS-arm64.zip"
checksum_file="$archive.sha256"
entries_file="$temp_root/entries"
extract_root="$temp_root/extract"
ready_directory=""
stage_app=""
launched_pid=""
install_committed=0
backup_app=""
preexisting_file=""
target_binary=""

cleanup() {
	rc=$?
	trap - EXIT HUP INT TERM
	if [ "$rc" -ne 0 ] && [ -n "$launched_pid" ]; then
		case "$launched_pid" in *[!0-9]* | '') ;; *)
			if [ "$(ps -p "$launched_pid" -o comm= 2>/dev/null | sed 's/^[[:space:]]*//')" = "$target_binary" ]; then
				kill -TERM "$launched_pid" >/dev/null 2>&1 || :
			fi
			;;
		esac
	fi
	if [ "$rc" -ne 0 ] && [ -n "$preexisting_file" ] && [ -f "$preexisting_file" ] && [ -n "$target_binary" ]; then
		target_pids 2>/dev/null | while IFS= read -r candidate_pid; do
			case "$candidate_pid" in '' | *[!0-9]*) continue ;; esac
			if ! grep -Fqx "$candidate_pid" "$preexisting_file"; then
				kill -TERM "$candidate_pid" >/dev/null 2>&1 || :
			fi
		done
	fi
	if [ -n "$ready_directory" ]; then
		case "$ready_directory" in "$TEMP_PARENT"/hmux-restart-ready.*)
			rm -f "$ready_directory/ready"
			rmdir "$ready_directory" >/dev/null 2>&1 || :
			;;
		esac
	fi
	if [ "$rc" -ne 0 ] && [ "$install_committed" -eq 1 ]; then
		failed_app="$INSTALL_ROOT/HMux.app.hmux-failed-$(date -u +%Y%m%dT%H%M%SZ)"
		if [ -d "$TARGET_APP" ] && [ ! -L "$TARGET_APP" ] && [ ! -e "$failed_app" ]; then
			mv "$TARGET_APP" "$failed_app" || :
		fi
	fi
	# The first rename may succeed even when the replacement rename fails or
	# execution is interrupted between the two operations.
	if [ "$rc" -ne 0 ] && [ -n "$backup_app" ] && [ -d "$backup_app" ] && [ ! -e "$TARGET_APP" ]; then
		mv "$backup_app" "$TARGET_APP" || :
	fi
	if [ -n "$stage_app" ]; then
		case "$stage_app" in "$INSTALL_ROOT"/.HMux.app.stage.*)
			rm -rf "$stage_app"
			;;
		esac
	fi
	case "$temp_root" in "$TEMP_PARENT"/hmux-app-install.*)
		rm -rf "$temp_root"
		;;
	esac
	exit "$rc"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

if [ "$LOCAL_BUILD" -eq 1 ]; then
	local_archive="$ROOT/macos/HMux/build/HMux-$version-macOS-arm64.zip"
	for local_input in "$local_archive" "$local_archive.sha256"; do
		[ -f "$local_input" ] && [ ! -L "$local_input" ] || fail "local package is missing or unsafe; build and package first"
	done
	cp "$local_archive" "$archive"
	cp "$local_archive.sha256" "$checksum_file"
	expected_source_digest=$("$ROOT/macos/HMux/scripts/source-digest.sh")
	printf 'hmux: installing local HMux %s at %s\n' "$version" "$TARGET_APP" >&2
else
	remote_base="Dropbox/HMux-$version-macOS-arm64.zip"
	printf 'hmux: downloading HMux %s from %s\n' "$version" "$SOURCE_ALIAS" >&2
	scp -q \
		-o BatchMode=yes \
		-o ConnectTimeout=10 \
		-o ConnectionAttempts=1 \
		-o ForwardAgent=no \
		-o ClearAllForwardings=yes \
		"$SOURCE_ALIAS:$remote_base.sha256" "$checksum_file"
	scp -q \
		-o BatchMode=yes \
		-o ConnectTimeout=10 \
		-o ConnectionAttempts=1 \
		-o ForwardAgent=no \
		-o ClearAllForwardings=yes \
		"$SOURCE_ALIAS:$remote_base" "$archive"
fi

for downloaded in "$checksum_file" "$archive"; do
	[ -f "$downloaded" ] && [ ! -L "$downloaded" ] || fail "SCP returned an unsafe file"
	[ "$(stat -f '%u' "$downloaded")" = "$(id -u)" ] || fail "SCP file owner is invalid"
done
[ "$(stat -f '%z' "$checksum_file")" -le 80 ] || fail "checksum file is oversized"
[ "$(wc -l <"$checksum_file" | tr -d '[:space:]')" -eq 1 ] || fail "checksum file is malformed"
IFS= read -r expected_sha <"$checksum_file"
case "$expected_sha" in '' | *[!0-9a-f]*) fail "checksum is malformed" ;; esac
[ "${#expected_sha}" -eq 64 ] || fail "checksum is malformed"
archive_bytes=$(stat -f '%z' "$archive")
[ "$archive_bytes" -ge 1048576 ] && [ "$archive_bytes" -le "$MAX_ARCHIVE_BYTES" ] ||
	fail "archive size is outside the allowed range"
actual_sha=$(shasum -a 256 "$archive" | awk '{print $1}')
[ "$actual_sha" = "$expected_sha" ] || fail "archive checksum verification failed"

/usr/bin/zipinfo -1 "$archive" >"$entries_file"
awk '
  length($0) == 0 || length($0) > 1024 { exit 1 }
  $0 != "HMux.app/" && $0 !~ /^HMux[.]app\// &&
    $0 != "__MACOSX/" && $0 !~ /^__MACOSX\/HMux[.]app\// { exit 1 }
  $0 ~ /(^|\/)\.{1,2}(\/|$)/ || $0 ~ /\\/ { exit 1 }
  { count++ }
  count > 50000 { exit 1 }
  END { if (count == 0) exit 1 }
' "$entries_file" || fail "archive paths are unsafe"

mkdir -m 700 "$extract_root"
ditto -x -k "$archive" "$extract_root"
candidate_app="$extract_root/HMux.app"
[ -d "$candidate_app" ] && [ ! -L "$candidate_app" ] || fail "archive does not contain HMux.app"
[ "$(find "$extract_root" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d '[:space:]')" -eq 1 ] ||
	fail "archive contains unexpected top-level entries"

validate_bundle() {
	bundle=$1
	plist="$bundle/Contents/Info.plist"
	binary="$bundle/Contents/MacOS/ghostty"
	helper="$bundle/Contents/Helpers/hmux"
	config="$bundle/Contents/Resources/HMuxGhostty.config"
	digest_file="$bundle/Contents/Resources/HMuxSourceDigest.txt"
	[ -f "$plist" ] && [ ! -L "$plist" ] || return 1
	[ -x "$binary" ] && [ -x "$helper" ] || return 1
	[ -f "$config" ] && [ -f "$digest_file" ] || return 1
	[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$plist")" = "dev.hmux.app" ] || return 1
	[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$plist")" = "$version" ] || return 1
	file "$binary" | grep -Fq arm64 || return 1
	[ "$(grep -Fxc 'keybind = clear' "$config")" -eq 1 ] || return 1
	[ "$(grep -Fxc 'keybind = cmd+q=quit' "$config")" -eq 1 ] || return 1
	clear_line=$(grep -Fn 'keybind = clear' "$config" | cut -d: -f1)
	quit_line=$(grep -Fn 'keybind = cmd+q=quit' "$config" | cut -d: -f1)
	[ "$quit_line" -gt "$clear_line" ] || return 1
	digest=$(tr -d '[:space:]' <"$digest_file")
	case "$digest" in '' | *[!0-9a-f]*) return 1 ;; esac
	[ "${#digest}" -eq 64 ] || return 1
	if [ "$LOCAL_BUILD" -eq 1 ]; then
		[ "$digest" = "$expected_source_digest" ] || return 1
	fi
	"$helper" --no-update-check version | grep -Fqx "hmux $version protocol=1 platform=darwin-arm64" || return 1
	codesign --verify --deep --strict "$bundle" >/dev/null 2>&1 || return 1
}

validate_bundle "$candidate_app" || fail "downloaded HMux bundle validation failed"

if [ -e "$INSTALL_ROOT" ]; then
	[ -d "$INSTALL_ROOT" ] && [ ! -L "$INSTALL_ROOT" ] || fail "installation directory is unsafe"
else
	[ "$SYSTEM_INSTALL" -eq 0 ] || fail "/Applications is missing"
	mkdir -m 700 "$INSTALL_ROOT"
fi
if [ "$SYSTEM_INSTALL" -eq 1 ]; then
	install_owner=$(stat -f '%u' "$INSTALL_ROOT")
	[ "$install_owner" = 0 ] || [ "$install_owner" = "$(id -u)" ] || fail "/Applications has an unexpected owner"
	[ -z "$(find "$INSTALL_ROOT" -prune -perm -002 -print)" ] || fail "/Applications is world writable"
else
	[ "$(stat -f '%u' "$INSTALL_ROOT")" = "$(id -u)" ] || fail "$HOME/Applications has the wrong owner"
	[ -z "$(find "$INSTALL_ROOT" -prune -perm -022 -print)" ] || fail "$HOME/Applications is group/world writable"
fi
[ -w "$INSTALL_ROOT" ] || fail "installation directory is not writable by this user"

# Only inspect exact executable paths when retiring a prior installed instance.
# A broad regular expression must never authorize a process termination.
target_pids() {
	pgrep -f '/Contents/MacOS/ghostty' 2>/dev/null | while IFS= read -r candidate_pid; do
		case "$candidate_pid" in '' | *[!0-9]*) continue ;; esac
		candidate_path=$(ps -p "$candidate_pid" -o comm= 2>/dev/null | sed 's/^[[:space:]]*//')
		if [ "$candidate_path" = "$target_binary" ]; then printf '%s\n' "$candidate_pid"; fi
	done
}

nonce=$(uuidgen | tr '[:upper:]' '[:lower:]')
case "$nonce" in '' | *[!0-9a-f-]*) fail "uuidgen returned an invalid nonce" ;; esac
[ "${#nonce}" -eq 36 ] || fail "uuidgen returned an invalid nonce"
stage_app="$INSTALL_ROOT/.HMux.app.stage.$nonce"
[ ! -e "$stage_app" ] || fail "installation stage already exists"
ditto "$candidate_app" "$stage_app"
validate_bundle "$stage_app" || fail "staged HMux bundle validation failed"

preexisting_file="$temp_root/preexisting-pids"
target_binary="$TARGET_APP/Contents/MacOS/ghostty"
target_pids >"$preexisting_file" 2>/dev/null || :

if [ -e "$TARGET_APP" ]; then
	[ -d "$TARGET_APP" ] && [ ! -L "$TARGET_APP" ] || fail "existing HMux.app is unsafe"
	target_owner=$(stat -f '%u' "$TARGET_APP")
	if [ "$SYSTEM_INSTALL" -eq 1 ]; then
		[ "$target_owner" = 0 ] || [ "$target_owner" = "$(id -u)" ] || fail "existing HMux.app has the wrong owner"
	else
		[ "$target_owner" = "$(id -u)" ] || fail "existing HMux.app has the wrong owner"
	fi
	[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$TARGET_APP/Contents/Info.plist")" = dev.hmux.app ] || fail "existing target is not HMux"
	stamp=$(date -u +%Y%m%dT%H%M%SZ)
	backup_app="$INSTALL_ROOT/HMux.app.hmux-shell-backup-$stamp"
	[ ! -e "$backup_app" ] || fail "HMux backup path already exists"
	mv "$TARGET_APP" "$backup_app"
fi
install_committed=1
mv "$stage_app" "$TARGET_APP"
stage_app=""
validate_bundle "$TARGET_APP" || fail "installed HMux bundle validation failed"

ready_directory="$TEMP_PARENT/hmux-restart-ready.$nonce"
[ ! -e "$ready_directory" ] || fail "launch readiness path already exists"
mkdir -m 700 "$ready_directory"
ready_file="$ready_directory/ready"
/usr/bin/open -n \
	--env "HMUX_RESTART_READY_DIRECTORY=$ready_directory" \
	--env "HMUX_RESTART_READY_NONCE=$nonce" \
	"$TARGET_APP"

deadline=$(($(date +%s) + 20))
while [ ! -f "$ready_file" ] && [ "$(date +%s)" -lt "$deadline" ]; do
	sleep 0.1
done
[ -f "$ready_file" ] && [ ! -L "$ready_file" ] || fail "installed HMux did not report readiness"
[ "$(stat -f '%u' "$ready_file")" = "$(id -u)" ] || fail "readiness owner is invalid"
[ "$(stat -f '%Lp' "$ready_file")" = 600 ] || fail "readiness mode is invalid"
[ "$(stat -f '%l' "$ready_file")" = 1 ] || fail "readiness link count is invalid"
[ "$(stat -f '%z' "$ready_file")" -le 128 ] || fail "readiness file is oversized"
[ "$(sed -n '1p' "$ready_file")" = "$nonce" ] || fail "readiness nonce is invalid"
launched_pid=$(sed -n '2p' "$ready_file")
case "$launched_pid" in '' | *[!0-9]*) fail "readiness PID is invalid" ;; esac
grep -Fqx "$launched_pid" "$preexisting_file" && fail "readiness reused an old HMux process"
process_path=$(ps -p "$launched_pid" -o comm= | sed 's/^[[:space:]]*//')
[ "$process_path" = "$target_binary" ] || fail "readiness process path is invalid"
kill -0 "$launched_pid" >/dev/null 2>&1 || fail "installed HMux exited before readiness completed"

while IFS= read -r old_pid; do
	case "$old_pid" in '' | *[!0-9]*) continue ;; esac
	if [ "$old_pid" != "$launched_pid" ] &&
		[ "$(ps -p "$old_pid" -o comm= 2>/dev/null | sed 's/^[[:space:]]*//')" = "$target_binary" ]; then
		kill -TERM "$old_pid" >/dev/null 2>&1 || :
	fi
done <"$preexisting_file"

install_committed=0
printf 'hmux: installed and launched HMux %s at %s\n' "$version" "$TARGET_APP"
