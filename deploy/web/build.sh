#!/bin/sh
# Native release bundles; building never installs binaries or changes services.
set -eu
ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd -P)
cd "$ROOT"

if [ "${HMUX_SKIP_WEB_BUILD:-0}" != 1 ]; then
	npm ci --prefix web
	npm run build --prefix web
fi
test -f web/dist/index.html || {
	echo 'Built web assets are required.' >&2
	exit 1
}
HMUX_VERSION=$(tr -d '[:space:]' <VERSION)
export HMUX_VERSION
hmux_host=$(rustc -vV | sed -n 's/^host: //p')
hmux_targets=${HMUX_RUST_TARGETS:-$hmux_host}
hmux_target_dir=${CARGO_TARGET_DIR:-target}
if [ -z "$hmux_host" ] || [ -z "$hmux_targets" ]; then
	echo 'No native Rust host/target selected.' >&2
	exit 1
fi

for hmux_target in $hmux_targets; do
	case "$hmux_target" in
	aarch64-apple-darwin) hmux_platform=darwin-arm64 ;;
	x86_64-apple-darwin) hmux_platform=darwin-amd64 ;;
	x86_64-unknown-linux-gnu | x86_64-unknown-linux-musl) hmux_platform=linux-amd64 ;;
	aarch64-unknown-linux-gnu | aarch64-unknown-linux-musl) hmux_platform=linux-arm64 ;;
	*)
		echo "Unsupported native release target: $hmux_target" >&2
		exit 1
		;;
	esac
	# Offline metadata resolves the lock graph, including dev-only packages that
	# a release build may not cache. Cross-build notices also include host tools.
	cargo fetch --locked --target "$hmux_target"
	if [ "$hmux_target" != "$hmux_host" ]; then
		cargo fetch --locked --target "$hmux_host"
	fi
	cargo build --locked --release --target "$hmux_target" -p hmux-web -p hmux-agent
	mkdir -p dist
	hmux_final="dist/web-$hmux_platform"
	hmux_bundle=$(mktemp -d "dist/.web-$hmux_platform.XXXXXX")
	trap 'rm -rf "$hmux_bundle"' EXIT
	trap 'exit 130' HUP INT TERM
	mkdir -p "$hmux_bundle/web" "$hmux_bundle/licenses"
	cp "$hmux_target_dir/$hmux_target/release/hmux-web" "$hmux_target_dir/$hmux_target/release/hmux-agent" "$hmux_bundle/"
	cp -R web/dist/. "$hmux_bundle/web/"
	cp THIRD_PARTY_NOTICES.md "$hmux_bundle/"
	if [ -f LICENSE ]; then cp LICENSE "$hmux_bundle/"; fi
	cp third_party/licenses/* "$hmux_bundle/licenses/"
	cp third_party/token-terrier-server/LICENSE "$hmux_bundle/licenses/token-terrier-LICENSE.txt"
	cp third_party/token-terrier-server/NOTICE "$hmux_bundle/licenses/token-terrier-NOTICE.txt"
	python3 scripts/rust_notices.py --target "$hmux_target" --output "$hmux_bundle/licenses/rust"
	cp deploy/web/hmux-web.service deploy/web/nginx.conf.example "$hmux_bundle/"
	cp deploy/web/install-home.py "$hmux_bundle/install-home.py"
	printf 'version=%s\ntarget=%s\nplatform=%s\nruntime=rust\n' "$HMUX_VERSION" "$hmux_target" "$hmux_platform" >"$hmux_bundle/RELEASE"
	(
		cd "$hmux_bundle"
		# The manifest is explicitly excluded from its own input set.
		if command -v sha256sum >/dev/null 2>&1; then
			# shellcheck disable=SC2094
			find . -type f ! -name SHA256SUMS -exec sha256sum '{}' \; | LC_ALL=C sort >SHA256SUMS
			sha256sum -c SHA256SUMS >/dev/null
		else
			# macOS provides shasum; Linux installations normally provide sha256sum.
			# shellcheck disable=SC2094
			find . -type f ! -name SHA256SUMS -exec shasum -a 256 '{}' \; | LC_ALL=C sort >SHA256SUMS
			shasum -a 256 -c SHA256SUMS >/dev/null
		fi
	)
	COPYFILE_DISABLE=1 tar -czf "dist/hmux-web-$hmux_platform.tar.gz" -C "$hmux_bundle" .
	rm -rf "$hmux_final"
	mv "$hmux_bundle" "$hmux_final"
	trap - EXIT HUP INT TERM
done
