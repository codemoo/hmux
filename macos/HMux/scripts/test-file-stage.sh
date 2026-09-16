#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
APP_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd -P)
OUTPUT="$APP_ROOT/.build/hmux-file-stage-smoke"
MODULE_CACHE="$APP_ROOT/.build/swift-module-cache"
mkdir -p "$APP_ROOT/.build" "$MODULE_CACHE"

swiftc -D DEBUG -parse-as-library \
	-module-cache-path "$MODULE_CACHE" \
	"$APP_ROOT/Overlay/Sources/HMux/HMuxModels.swift" \
	"$APP_ROOT/Overlay/Sources/HMux/HMuxBackend.swift" \
    "$APP_ROOT/Overlay/Sources/HMux/HMuxWorkspaceState.swift" \
    "$APP_ROOT/Overlay/Sources/HMux/HMuxConversation.swift" \
	"$APP_ROOT/Tests/HMuxFileStageSmoke.swift" \
	-o "$OUTPUT"
"$OUTPUT"
