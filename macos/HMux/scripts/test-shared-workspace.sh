#!/bin/sh
set -eu
SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
APP_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd -P)
BUILD_DIR="$APP_ROOT/.build/shared-workspace-smoke"
mkdir -p "$BUILD_DIR"
python3 - "$APP_ROOT" "$BUILD_DIR/generated.swift" <<'PY'
import sys
from pathlib import Path
root=Path(sys.argv[1])
sys.path.insert(0,str(root/'Tests'))
from extract_store_methods import extract_method
source=(root/'Overlay/Sources/HMux/HMuxStore.swift').read_text()
methods='\n'.join(extract_method(source,name) for name in ['startSharedWorkspaceSync','syncSharedWorkspace','applySharedWorkspace'])
shell=(root/'Tests/HMuxSharedWorkspaceSmoke.swift').read_text()
Path(sys.argv[2]).write_text(shell.replace('// __EXACT_PRODUCTION_METHODS__',methods))
PY
swiftc -parse-as-library -module-cache-path "$BUILD_DIR/module-cache" \
  "$APP_ROOT/Overlay/Sources/HMux/HMuxModels.swift" \
  "$APP_ROOT/Overlay/Sources/HMux/HMuxWorkspaceState.swift" \
  "$BUILD_DIR/generated.swift" -o "$BUILD_DIR/smoke"
"$BUILD_DIR/smoke"
