#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
APP_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd -P)
REPO_ROOT=$(CDPATH='' cd -- "$APP_ROOT/../.." && pwd -P)
BUILD_ROOT="$APP_ROOT/build"
APP="$BUILD_ROOT/HMux.app"
VERSION=$(tr -d '[:space:]' <"$REPO_ROOT/VERSION")
ARCHIVE="$BUILD_ROOT/HMux-$VERSION-macOS-arm64.zip"
SOURCE_DIGEST=$("$SCRIPT_DIR/source-digest.sh")

verify_macho_arches() {
	bundle=$1
	find "$bundle" -type f -exec sh -c '
      for candidate do
        description=$(file -b "$candidate")
        case "$description" in
          *Mach-O*) printf "%s\n" "$description" | grep -Fq arm64 || exit 1 ;;
        esac
      done
    ' sh {} +
}

verify_bundle() {
	bundle=$1
	resources="$bundle/Contents/Resources"
	frames="$resources/BedlFrames"
	[ -d "$bundle" ]
	[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$bundle/Contents/Info.plist")" = "dev.hmux.app" ]
	[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$bundle/Contents/Info.plist")" = "$VERSION" ]
	[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$bundle/Contents/Info.plist")" = "ghostty" ]
	for executable in \
		"$bundle/Contents/MacOS/ghostty" \
		"$bundle/Contents/Helpers/hmux"; do
		[ -x "$executable" ]
		file "$executable" | grep -Fq 'arm64'
	done
	verify_macho_arches "$bundle"
	[ ! -e "$bundle/Contents/Helpers/hmux-usage-helper" ]
	[ -x "$bundle/Contents/Frameworks/Sparkle.framework/Versions/B/Sparkle" ]
	[ -x "$bundle/Contents/PlugIns/DockTilePlugin.plugin/Contents/MacOS/DockTilePlugin" ]
	[ -f "$resources/terminfo/67/ghostty" ]
	[ -f "$resources/terminfo/78/xterm-ghostty" ]
	otool -L "$bundle/Contents/MacOS/ghostty" | grep -Fq '@rpath/Sparkle.framework/Versions/B/Sparkle'
	otool -l "$bundle/Contents/MacOS/ghostty" | grep -Fq 'path @executable_path/../Frameworks'
	capabilities=$(
		"$bundle/Contents/Helpers/hmux" --no-update-check app capabilities
	)
	printf '%s\n' "$capabilities" | jq -e --arg version "$VERSION" '
      .app_protocol_version == 1 and
      .ok == true and
      .error == null and
      .data.backend_protocol_version == 1 and
      .data.version == $version and
      (([
        "catalog", "catalog-stream", "usage-stream", "workspace", "conversation", "profiles", "create",
        "alias-set", "hidden-set", "terminate", "file-stage", "terminal",
        "signed-native-update"
      ] - .data.features) | length == 0)
    ' >/dev/null
	for resource in \
		"$resources/Ghostty.icns" \
		"$resources/HMuxGhostty.config" \
		"$resources/ThirdPartyNotices.md" \
		"$resources/HMuxSourceDigest.txt"; do
		[ -f "$resource" ]
	done
	[ "$(grep -Fxc 'keybind = clear' "$resources/HMuxGhostty.config")" -eq 1 ]
	[ "$(grep -Fxc 'keybind = cmd+q=quit' "$resources/HMuxGhostty.config")" -eq 1 ]
	[ "$(grep -Fn 'keybind = cmd+q=quit' "$resources/HMuxGhostty.config" | cut -d: -f1)" -gt \
		"$(grep -Fn 'keybind = clear' "$resources/HMuxGhostty.config" | cut -d: -f1)" ]
	[ "$(tr -d '[:space:]' <"$resources/HMuxSourceDigest.txt")" = "$SOURCE_DIGEST" ]
	[ "$(find "$frames" -type f -name 'bedl-*.png' | wc -l | tr -d '[:space:]')" = "8" ]
	frame=1
	while [ "$frame" -le 8 ]; do
		[ -f "$frames/bedl-$frame.png" ]
		frame=$((frame + 1))
	done
	codesign --verify --deep --strict --verbose=2 "$bundle"
}

[ -d "$APP" ] || {
	echo "HMux.app is missing; run scripts/build.sh first" >&2
	exit 1
}
verify_bundle "$APP"

stage=$(mktemp -d "$BUILD_ROOT/.package-stage.XXXXXX")
trap 'case "$stage" in "$BUILD_ROOT"/.package-stage.*) rm -rf "$stage" ;; esac' EXIT HUP INT TERM
ditto -c -k --sequesterRsrc --keepParent "$APP" "$stage/HMux.zip"
mkdir -m 700 "$stage/roundtrip"
ditto -x -k "$stage/HMux.zip" "$stage/roundtrip"
verify_bundle "$stage/roundtrip/HMux.app"
shasum -a 256 "$stage/HMux.zip" | awk '{print $1}' >"$stage/HMux.zip.sha256"
mv "$stage/HMux.zip" "$ARCHIVE"
mv "$stage/HMux.zip.sha256" "$ARCHIVE.sha256"
shasum -a 256 "$ARCHIVE"
