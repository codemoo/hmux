#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
APP_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd -P)
OUTPUT="$APP_ROOT/.build/hmux-surface-input-smoke"
MODULE_CACHE="$APP_ROOT/.build/swift-module-cache"
mkdir -p "$APP_ROOT/.build" "$MODULE_CACHE"

swiftc -parse-as-library \
    -module-cache-path "$MODULE_CACHE" \
    "$APP_ROOT/Overlay/Sources/HMux/HMuxSurfaceDeck.swift" \
    "$APP_ROOT/Tests/HMuxSurfaceInputSmoke.swift" \
    -o "$OUTPUT"
"$OUTPUT"
