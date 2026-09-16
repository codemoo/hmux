#!/bin/sh
set -eu
SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
APP_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd -P)
mkdir -p "$APP_ROOT/.build/swift-module-cache"
swiftc -parse-as-library -module-cache-path "$APP_ROOT/.build/swift-module-cache" \
    "$APP_ROOT/Overlay/Sources/HMux/HMuxModels.swift" \
    "$APP_ROOT/Overlay/Sources/HMux/HMuxDesign.swift" \
    "$APP_ROOT/Tests/HMuxSearchFieldSmoke.swift" \
    -o "$APP_ROOT/.build/hmux-search-field-smoke"
"$APP_ROOT/.build/hmux-search-field-smoke"
