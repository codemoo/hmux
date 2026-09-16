#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
APP_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd -P)
REPO_ROOT=$(CDPATH='' cd -- "$APP_ROOT/../.." && pwd -P)
LOCK_FILE="$APP_ROOT/Dependencies.lock.json"
BUILD_ROOT="$APP_ROOT/.build"
DOWNLOAD_ROOT="$BUILD_ROOT/downloads"
TOOLS_ROOT="$BUILD_ROOT/tools"
RAW_GHOSTTY="$BUILD_ROOT/ghostty-source"
WORK_GHOSTTY="$BUILD_ROOT/ghostty-work"

command -v jq >/dev/null 2>&1 || {
	echo "jq is required" >&2
	exit 1
}
command -v curl >/dev/null 2>&1 || {
	echo "curl is required" >&2
	exit 1
}

ARCH=$(uname -m)
if [ "$ARCH" != "arm64" ]; then
	echo "The native build requires the pinned arm64 macOS Zig toolchain; got $ARCH" >&2
	exit 1
fi

GHOSTTY_COMMIT=$(jq -er '.ghostty.commit' "$LOCK_FILE")
GHOSTTY_URL=$(jq -er '.ghostty.archive_url' "$LOCK_FILE")
GHOSTTY_SHA=$(jq -er '.ghostty.archive_sha256' "$LOCK_FILE")
ZIG_VERSION=$(jq -er '.zig.version' "$LOCK_FILE")
ZIG_URL=$(jq -er '.zig.aarch64_macos_url' "$LOCK_FILE")
ZIG_SHA=$(jq -er '.zig.aarch64_macos_sha256' "$LOCK_FILE")
SPARKLE_VERSION=$(jq -er '.swiftpm.sparkle_version' "$LOCK_FILE")
SPARKLE_URL=$(jq -er '.swiftpm.sparkle_artifact_url' "$LOCK_FILE")
SPARKLE_SHA=$(jq -er '.swiftpm.sparkle_artifact_sha256' "$LOCK_FILE")

mkdir -p "$DOWNLOAD_ROOT" "$TOOLS_ROOT"

verify_sha() {
	expected=$1
	file=$2
	actual=$(shasum -a 256 "$file" | awk '{print $1}')
	[ "$actual" = "$expected" ] || {
		echo "checksum mismatch for $file" >&2
		exit 1
	}
}

GHOSTTY_ARCHIVE="$DOWNLOAD_ROOT/ghostty-$GHOSTTY_COMMIT.tar.gz"
if [ ! -f "$GHOSTTY_ARCHIVE" ]; then
	temp_archive="$GHOSTTY_ARCHIVE.partial"
	curl -fL --retry 3 --max-time 300 "$GHOSTTY_URL" -o "$temp_archive"
	verify_sha "$GHOSTTY_SHA" "$temp_archive"
	mv "$temp_archive" "$GHOSTTY_ARCHIVE"
fi
verify_sha "$GHOSTTY_SHA" "$GHOSTTY_ARCHIVE"

ZIG_ARCHIVE="$DOWNLOAD_ROOT/zig-aarch64-macos-$ZIG_VERSION.tar.xz"
if [ ! -f "$ZIG_ARCHIVE" ]; then
	temp_archive="$ZIG_ARCHIVE.partial"
	curl -fL --retry 3 --max-time 300 "$ZIG_URL" -o "$temp_archive"
	verify_sha "$ZIG_SHA" "$temp_archive"
	mv "$temp_archive" "$ZIG_ARCHIVE"
fi
verify_sha "$ZIG_SHA" "$ZIG_ARCHIVE"

SPARKLE_ARCHIVE="$DOWNLOAD_ROOT/Sparkle-for-Swift-Package-Manager-$SPARKLE_VERSION.zip"
if [ ! -f "$SPARKLE_ARCHIVE" ]; then
	temp_archive="$SPARKLE_ARCHIVE.partial"
	curl -fL --retry 3 --max-time 300 "$SPARKLE_URL" -o "$temp_archive"
	verify_sha "$SPARKLE_SHA" "$temp_archive"
	mv "$temp_archive" "$SPARKLE_ARCHIVE"
fi
verify_sha "$SPARKLE_SHA" "$SPARKLE_ARCHIVE"

SPARKLE_ROOT="$BUILD_ROOT/swift-packages/artifacts/sparkle/Sparkle"
if [ ! -f "$SPARKLE_ROOT/Sparkle.xcframework/Info.plist" ]; then
	stage=$(mktemp -d "$BUILD_ROOT/sparkle-stage.XXXXXX")
	trap 'rm -rf "$stage"' EXIT HUP INT TERM
	mkdir -m 700 "$stage/unpacked"
	ditto -x -k "$SPARKLE_ARCHIVE" "$stage/unpacked"
	[ -f "$stage/unpacked/Sparkle.xcframework/Info.plist" ] || {
		echo "invalid Sparkle artifact" >&2
		exit 1
	}
	case "$SPARKLE_ROOT" in "$BUILD_ROOT"/swift-packages/artifacts/sparkle/Sparkle) ;; *)
		echo "unsafe Sparkle artifact path" >&2
		exit 1
		;;
	esac
	rm -rf "$SPARKLE_ROOT"
	mkdir -p "$(dirname "$SPARKLE_ROOT")"
	mv "$stage/unpacked" "$SPARKLE_ROOT"
	rm -rf "$stage"
	trap - EXIT HUP INT TERM
fi

ZIG_DIR="$TOOLS_ROOT/zig-aarch64-macos-$ZIG_VERSION"
if [ ! -x "$ZIG_DIR/zig" ]; then
	stage=$(mktemp -d "$BUILD_ROOT/zig-stage.XXXXXX")
	trap 'rm -rf "$stage"' EXIT HUP INT TERM
	tar -xJf "$ZIG_ARCHIVE" -C "$stage"
	extracted="$stage/zig-aarch64-macos-$ZIG_VERSION"
	[ -x "$extracted/zig" ] || {
		echo "invalid Zig archive" >&2
		exit 1
	}
	mv "$extracted" "$ZIG_DIR"
	rm -rf "$stage"
	trap - EXIT HUP INT TERM
fi

if [ ! -f "$RAW_GHOSTTY/.hmux-commit" ] || [ "$(cat "$RAW_GHOSTTY/.hmux-commit" 2>/dev/null || true)" != "$GHOSTTY_COMMIT" ]; then
	case "$RAW_GHOSTTY" in "$BUILD_ROOT"/*) ;; *)
		echo "unsafe Ghostty source path" >&2
		exit 1
		;;
	esac
	rm -rf "$RAW_GHOSTTY"
	stage=$(mktemp -d "$BUILD_ROOT/ghostty-stage.XXXXXX")
	trap 'rm -rf "$stage"' EXIT HUP INT TERM
	tar -xzf "$GHOSTTY_ARCHIVE" -C "$stage"
	extracted=$(find "$stage" -mindepth 1 -maxdepth 1 -type d -name 'ghostty-*' | head -n 1)
	[ -n "$extracted" ] || {
		echo "invalid Ghostty archive" >&2
		exit 1
	}
	mv "$extracted" "$RAW_GHOSTTY"
	printf '%s\n' "$GHOSTTY_COMMIT" >"$RAW_GHOSTTY/.hmux-commit"
	rm -rf "$stage"
	trap - EXIT HUP INT TERM
fi

OVERLAY_DIGEST=$(
	find "$APP_ROOT/Overlay" -type f -print0 |
		sort -z |
		xargs -0 shasum -a 256 |
		shasum -a 256 |
		awk '{print $1}'
)
if [ ! -f "$WORK_GHOSTTY/.hmux-overlay" ] || [ "$(cat "$WORK_GHOSTTY/.hmux-overlay" 2>/dev/null || true)" != "$OVERLAY_DIGEST" ]; then
	case "$WORK_GHOSTTY" in "$BUILD_ROOT"/*) ;; *)
		echo "unsafe Ghostty work path" >&2
		exit 1
		;;
	esac
	rm -rf "$WORK_GHOSTTY"
	cp -R "$RAW_GHOSTTY" "$WORK_GHOSTTY"
	mkdir -p "$WORK_GHOSTTY/macos/Sources/HMux"
	cp "$APP_ROOT"/Overlay/Sources/HMux/* "$WORK_GHOSTTY/macos/Sources/HMux/"
	mkdir -p "$WORK_GHOSTTY/macos/Assets.xcassets"
	cp -R "$APP_ROOT/Overlay/Assets.xcassets/Ghostty.appiconset" \
		"$WORK_GHOSTTY/macos/Assets.xcassets/"
	for overlay_patch in \
		"$APP_ROOT/Overlay/ghostty-hmux.patch" \
		"$APP_ROOT/Overlay/ghostty-hmux-safety.patch" \
		"$APP_ROOT/Overlay/ghostty-hmux-links.patch" \
		"$APP_ROOT/Overlay/ghostty-hmux-links-ownership.patch"; do
		(cd /private/tmp && git apply --check --recount --unsafe-paths --directory="$WORK_GHOSTTY" "$overlay_patch")
		(cd /private/tmp && git apply --recount --unsafe-paths --directory="$WORK_GHOSTTY" "$overlay_patch")
		(cd /private/tmp && git apply --reverse --check --recount --unsafe-paths --directory="$WORK_GHOSTTY" "$overlay_patch")
	done
	printf '%s\n' "$OVERLAY_DIGEST" >"$WORK_GHOSTTY/.hmux-overlay"
fi

printf '%s\n' "$WORK_GHOSTTY"
printf '%s\n' "$ZIG_DIR/zig"
printf '%s\n' "$REPO_ROOT"
