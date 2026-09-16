#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
APP_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd -P)
STORE="$APP_ROOT/Overlay/Sources/HMux/HMuxStore.swift"
MODELS="$APP_ROOT/Overlay/Sources/HMux/HMuxModels.swift"
TEST_SOURCE="$APP_ROOT/Tests/HMuxCatalogStoreSmoke.swift"
EXTRACTOR="$APP_ROOT/Tests/extract_store_methods.py"
BUILD_DIR="$APP_ROOT/.build/catalog-store-smoke"
GENERATED="$BUILD_DIR/HMuxCatalogStoreSmoke.generated.swift"
OUTPUT="$BUILD_DIR/hmux-catalog-store-smoke"
MODULE_CACHE="$BUILD_DIR/swift-module-cache"

mkdir -p "$BUILD_DIR" "$MODULE_CACHE"
python3 "$EXTRACTOR" "$STORE" "$TEST_SOURCE" "$GENERATED"

swiftc -parse-as-library \
    -module-cache-path "$MODULE_CACHE" \
    "$MODELS" \
    "$GENERATED" \
    -o "$OUTPUT"

"$OUTPUT"
