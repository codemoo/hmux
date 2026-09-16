#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
APP_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd -P)
REPO_ROOT=$(CDPATH='' cd -- "$APP_ROOT/../.." && pwd -P)
BUILD_ROOT="$APP_ROOT/.build"
GHOSTTY_ROOT="$BUILD_ROOT/ghostty-work"
ZIG="$BUILD_ROOT/tools/zig-aarch64-macos-0.15.2/zig"
DERIVED_DATA="$APP_ROOT/DerivedData"
PACKAGES="$BUILD_ROOT/swift-packages"
OUTPUT_ROOT="$APP_ROOT/build"

case "${HMUX_FRESH_BUILD:-0}" in
0) ;;
1)
	case "$DERIVED_DATA:$OUTPUT_ROOT" in
	"$APP_ROOT"/*:"$APP_ROOT"/*) ;;
	*) echo "unsafe fresh build paths" >&2; exit 1 ;;
	esac
	rm -rf "$DERIVED_DATA" "$OUTPUT_ROOT"
	;;
*) echo "HMUX_FRESH_BUILD must be 0 or 1" >&2; exit 2 ;;
esac

"$SCRIPT_DIR/prepare-dependencies.sh" >/dev/null

mkdir -p \
	"$BUILD_ROOT/cache/zig-global" \
	"$BUILD_ROOT/cache/zig-local" \
	"$BUILD_ROOT/cache/xcode-module" \
	"$BUILD_ROOT/cache/swiftpm-module" \
	"$BUILD_ROOT/cache/swift-package" \
	"$BUILD_ROOT/cache/fixed-user-home/Library/Caches" \
	"$PACKAGES" \
	"$OUTPUT_ROOT"

(
	cd "$GHOSTTY_ROOT"
	env \
		ZIG_GLOBAL_CACHE_DIR="$BUILD_ROOT/cache/zig-global" \
		ZIG_LOCAL_CACHE_DIR="$BUILD_ROOT/cache/zig-local" \
		"$ZIG" build \
		-Demit-xcframework=true \
		-Dxcframework-target=native \
		-Demit-macos-app=false \
		-Doptimize=ReleaseFast
)

VERSION=$(tr -d '[:space:]' <"$REPO_ROOT/VERSION")
env \
	CFFIXED_USER_HOME="$BUILD_ROOT/cache/fixed-user-home" \
	CLANG_MODULE_CACHE_PATH="$BUILD_ROOT/cache/xcode-module" \
	SWIFTPM_MODULECACHE_OVERRIDE="$BUILD_ROOT/cache/swiftpm-module" \
	xcodebuild -quiet \
	-project "$GHOSTTY_ROOT/macos/Ghostty.xcodeproj" \
	-scheme Ghostty \
	-configuration Release \
	-derivedDataPath "$DERIVED_DATA" \
	-clonedSourcePackagesDirPath "$PACKAGES" \
	-packageCachePath "$BUILD_ROOT/cache/swift-package" \
	-disableAutomaticPackageResolution \
	-onlyUsePackageVersionsFromResolvedFile \
	-skipPackageUpdates \
	CODE_SIGNING_ALLOWED=NO \
	CODE_SIGNING_REQUIRED=NO \
	ARCHS=arm64 \
	ONLY_ACTIVE_ARCH=YES \
	SWIFT_VERSION=5.0 \
	INFOPLIST_KEY_CFBundleDisplayName=HMux \
	MARKETING_VERSION="$VERSION" \
	CURRENT_PROJECT_VERSION=1 \
	build

APP="$DERIVED_DATA/Build/Products/Release/Ghostty.app"
[ -d "$APP" ] || {
	echo "HMux.app was not produced" >&2
	exit 1
}
mkdir -p "$APP/Contents/Helpers" "$APP/Contents/Resources"
rm -f "$APP/Contents/Helpers/hmux-usage-helper"
env GOCACHE="$BUILD_ROOT/cache/go-build" GOMODCACHE="$BUILD_ROOT/cache/go-mod" \
	go build -trimpath -ldflags "-X main.version=$VERSION" -o "$APP/Contents/Helpers/hmux" "$REPO_ROOT/cmd/hmux"
cp "$APP_ROOT/ThirdPartyNotices.md" "$APP/Contents/Resources/ThirdPartyNotices.md"
cp "$APP_ROOT/HMuxGhostty.config" "$APP/Contents/Resources/HMuxGhostty.config"
mkdir -p "$APP/Contents/Resources/BedlFrames"
cp "$APP_ROOT"/Overlay/Resources/BedlFrames/*.png "$APP/Contents/Resources/BedlFrames/"
"$SCRIPT_DIR/source-digest.sh" >"$APP/Contents/Resources/HMuxSourceDigest.txt"

case "$OUTPUT_ROOT/HMux.app" in "$OUTPUT_ROOT"/*) ;; *)
	echo "unsafe output path" >&2
	exit 1
	;;
esac
rm -rf "$OUTPUT_ROOT/HMux.app"
cp -R "$APP" "$OUTPUT_ROOT/HMux.app"

PLIST="$OUTPUT_ROOT/HMux.app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier dev.hmux.app" "$PLIST"
/usr/libexec/PlistBuddy -c "Set :CFBundleDisplayName HMux" "$PLIST"
/usr/libexec/PlistBuddy -c "Set :CFBundleName HMux" "$PLIST"

delete_plist_key() {
	/usr/libexec/PlistBuddy -c "Delete :$1" "$PLIST" >/dev/null 2>&1 || return 0
}
for key in \
	CFBundleDocumentTypes NSAppleScriptEnabled NSDockTilePlugIn NSServices \
	OSAScriptingDefinition SUEnableAutomaticChecks SUPublicEDKey \
	UTExportedTypeDeclarations; do
	delete_plist_key "$key"
done

APP_INTENTS="$OUTPUT_ROOT/HMux.app/Contents/Resources/Metadata.appintents"
case "$APP_INTENTS" in "$OUTPUT_ROOT"/*) ;; *)
	echo "unsafe app intents path" >&2
	exit 1
	;;
esac
rm -rf "$APP_INTENTS"

# Xcode's linker-generated ad-hoc signature no longer covers the bundle after
# the HMux metadata and resources above are installed. Re-seal the finished
# local-development bundle so Finder and Launch Services see a consistent app.
codesign --force --deep --sign - "$OUTPUT_ROOT/HMux.app"
codesign --verify --deep --strict --verbose=2 "$OUTPUT_ROOT/HMux.app"

echo "$OUTPUT_ROOT/HMux.app"
