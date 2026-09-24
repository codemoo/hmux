#!/bin/sh
# Pinned upstream release assets; no Go toolchain or system installation required.
set -eu

hmux_shfmt_dir=${1:-.tools}
hmux_shfmt_version=3.13.1
case "$(uname -s):$(uname -m)" in
Darwin:arm64)
	hmux_shfmt_platform=darwin_arm64
	hmux_shfmt_hash=9680526be4a66ea1ffe988ed08af58e1400fe1e4f4aef5bd88b20bb9b3da33f8
	;;
Darwin:x86_64)
	hmux_shfmt_platform=darwin_amd64
	hmux_shfmt_hash=6feedafc72915794163114f512348e2437d080d0047ef8b8fa2ec63b575f12af
	;;
Linux:x86_64)
	hmux_shfmt_platform=linux_amd64
	hmux_shfmt_hash=fb096c5d1ac6beabbdbaa2874d025badb03ee07929f0c9ff67563ce8c75398b1
	;;
Linux:aarch64 | Linux:arm64)
	hmux_shfmt_platform=linux_arm64
	hmux_shfmt_hash=32d92acaa5cd8abb29fc49dac123dc412442d5713967819d8af2c29f1b3857c7
	;;
*)
	echo 'shfmt bootstrap supports macOS/Linux on arm64/amd64' >&2
	exit 1
	;;
esac

hmux_shfmt_digest() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum <"$1" | cut -d ' ' -f 1
	else
		shasum -a 256 <"$1" | cut -d ' ' -f 1
	fi
}

mkdir -p "$hmux_shfmt_dir"
hmux_shfmt_bin="$hmux_shfmt_dir/shfmt-v$hmux_shfmt_version"
if [ -L "$hmux_shfmt_bin" ] || { [ -e "$hmux_shfmt_bin" ] && [ ! -f "$hmux_shfmt_bin" ]; }; then
	echo 'shfmt destination must be a regular file' >&2
	exit 1
fi
if [ -f "$hmux_shfmt_bin" ] && [ "$(hmux_shfmt_digest "$hmux_shfmt_bin")" = "$hmux_shfmt_hash" ]; then
	chmod 755 "$hmux_shfmt_bin"
	exit 0
fi
hmux_shfmt_tmp=$(mktemp "$hmux_shfmt_dir/.shfmt-XXXXXX")
trap 'rm -f "$hmux_shfmt_tmp"' EXIT HUP INT TERM
curl --silent --show-error --fail --location --proto '=https' --proto-redir '=https' \
	--tlsv1.2 --max-time 60 --retry 2 \
	"https://github.com/mvdan/sh/releases/download/v$hmux_shfmt_version/shfmt_v${hmux_shfmt_version}_$hmux_shfmt_platform" \
	--output "$hmux_shfmt_tmp"
if [ "$(hmux_shfmt_digest "$hmux_shfmt_tmp")" != "$hmux_shfmt_hash" ]; then
	echo 'shfmt release checksum mismatch' >&2
	exit 1
fi
chmod 755 "$hmux_shfmt_tmp"
mv -f "$hmux_shfmt_tmp" "$hmux_shfmt_bin"
