#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
APP_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd -P)
REPO_ROOT=$(CDPATH='' cd -- "$APP_ROOT/../.." && pwd -P)

for forbidden in internal/source deploy heap-snapshots; do
	[ ! -e "$REPO_ROOT/third_party/token-terrier-server/$forbidden" ] || {
		echo "forbidden usage-helper vendor path is present: $forbidden" >&2
		exit 1
	}
done

cd "$REPO_ROOT"
{
	for input in \
		VERSION go.mod go.sum cmd internal archive/terminal/ui archive/terminal/frame third_party/token-terrier-server \
		macos/HMux/Dependencies.lock.json macos/HMux/HMuxGhostty.config \
		macos/HMux/Overlay macos/HMux/ThirdPartyNotices.md \
		macos/HMux/scripts/build.sh macos/HMux/scripts/prepare-dependencies.sh \
		macos/HMux/scripts/package.sh macos/HMux/scripts/source-digest.sh; do
		if [ -d "$input" ]; then
			find "$input" -type f -print
		elif [ -f "$input" ]; then
			printf '%s\n' "$input"
		else
			echo "missing build input: $input" >&2
			exit 1
		fi
	done
} | LC_ALL=C sort | while IFS= read -r input; do
	digest=$(shasum -a 256 "$input" | awk '{print $1}')
	printf '%s  %s\n' "$digest" "$input"
done | shasum -a 256 | awk '{print $1}'
