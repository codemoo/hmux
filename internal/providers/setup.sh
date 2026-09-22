#!/usr/bin/env bash
# HMux provider setup. Runs inside a visible tmux tab as the Home user.
# Usage: setup.sh connect|update codex|claude|gemini
#   connect: install when missing, then run the CLI's own login flow
#   update:  install or update to the latest release
# Runs in a private tmux server read by HMux, which reads progress from the
# $HMUX_JOB_STATE file. Installs only into ~/.local and never uses sudo.
set -uo pipefail

action=${1:-}
provider=${2:-}
bin="$HOME/.local/bin"
export PATH="$bin:$PATH"

say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
# Progress goes to $HMUX_JOB_STATE because CLIs such as Gemini clear the screen.
last_phase=none
phase() {
	last_phase=$1
	printf '%s\n' "$1" >"${HMUX_JOB_STATE:-/dev/null}"
}
done_with() {
	printf 'done:%s:%s\n' "$1" "$last_phase" >"${HMUX_JOB_STATE:-/dev/null}"
	# Keep the pane readable until HMux collects the result and closes it.
	exec sleep 600
}
need() {
	for tool in "$@"; do
		if ! command -v "$tool" >/dev/null 2>&1; then
			echo "필요한 명령이 없습니다: $tool (관리자에게 설치를 요청하세요)"
			return 1
		fi
	done
}
sha256_check() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum -c -
	else
		shasum -a 256 -c -
	fi
}

codex_target() {
	case "$(uname -s)-$(uname -m)" in
	Linux-x86_64) echo x86_64-unknown-linux-musl ;;
	Linux-aarch64 | Linux-arm64) echo aarch64-unknown-linux-musl ;;
	Darwin-arm64) echo aarch64-apple-darwin ;;
	Darwin-x86_64) echo x86_64-apple-darwin ;;
	*) return 1 ;;
	esac
}

node_platform() {
	case "$(uname -s)-$(uname -m)" in
	Linux-x86_64) echo linux-x64 ;;
	Linux-aarch64 | Linux-arm64) echo linux-arm64 ;;
	Darwin-arm64) echo darwin-arm64 ;;
	Darwin-x86_64) echo darwin-x64 ;;
	*) return 1 ;;
	esac
}

node_ok() {
	command -v node >/dev/null 2>&1 || return 1
	major=$(node -p 'process.versions.node.split(".")[0]' 2>/dev/null) || return 1
	[ "${major:-0}" -ge 20 ]
}

install_node() (
	need curl tar || return 1
	platform=$(node_platform) || {
		echo "지원하지 않는 플랫폼입니다: $(uname -sm)"
		return 1
	}
	base=https://nodejs.org/dist/latest-v22.x
	work=$(mktemp -d) || return 1
	trap 'rm -rf "$work"' EXIT
	say "Node.js 22 다운로드 ($platform)"
	curl -fsSL "$base/SHASUMS256.txt" -o "$work/SHASUMS256.txt" || return 1
	file=$(grep -o "node-v22[0-9.]*-$platform\.tar\.gz" "$work/SHASUMS256.txt" | head -n 1)
	[ -n "$file" ] || {
		echo "Node.js 배포 파일을 찾지 못했습니다."
		return 1
	}
	curl -fSL --progress-bar "$base/$file" -o "$work/$file" || return 1
	(cd "$work" && grep " $file\$" SHASUMS256.txt | sha256_check) || return 1
	dest="$HOME/.local/share/hmux/node"
	rm -rf "$dest.new"
	mkdir -p "$dest.new" "$bin" || return 1
	tar -xzf "$work/$file" -C "$dest.new" --strip-components=1 || return 1
	rm -rf "$dest"
	mv "$dest.new" "$dest" || return 1
	for tool in node npm npx; do
		ln -sfn "$dest/bin/$tool" "$bin/$tool" || return 1
	done
	node -v
)

install_codex() (
	need curl tar || return 1
	target=$(codex_target) || {
		echo "지원하지 않는 플랫폼입니다: $(uname -sm)"
		return 1
	}
	work=$(mktemp -d) || return 1
	trap 'rm -rf "$work"' EXIT
	say "Codex 다운로드 (github.com/openai/codex, $target)"
	curl -fSL --progress-bar "https://github.com/openai/codex/releases/latest/download/codex-$target.tar.gz" -o "$work/codex.tar.gz" || return 1
	tar -xzf "$work/codex.tar.gz" -C "$work" || return 1
	mkdir -p "$bin" || return 1
	install -m 0755 "$work/codex-$target" "$bin/codex" || return 1
	"$bin/codex" --version
)

install_claude() {
	need curl bash || return 1
	say "Claude Code 설치 (claude.ai/install.sh)"
	curl -fsSL https://claude.ai/install.sh | bash || return 1
	"$bin/claude" --version
}

install_gemini() {
	if ! node_ok; then
		install_node || return 1
	fi
	say "Gemini CLI 설치 (npm @google/gemini-cli)"
	npm install -g --prefix "$HOME/.local" @google/gemini-cli || return 1
	"$bin/gemini" --version
}

login() {
	case "$provider" in
	codex)
		codex login --device-auth
		;;
	claude)
		claude auth login
		;;
	gemini)
		# HMux preselects Google login; trust only matters for this login run.
		GEMINI_CLI_TRUST_WORKSPACE=true NO_BROWSER=true gemini
		;;
	esac
}

case "$provider" in
codex | claude | gemini) ;;
*)
	echo "unknown provider"
	exit 2
	;;
esac
case "$action" in
update)
	phase install
	"install_$provider"
	done_with $?
	;;
connect)
	if ! command -v "$provider" >/dev/null 2>&1; then
		phase install
		"install_$provider" || done_with $?
	fi
	phase login
	login
	done_with $?
	;;
*)
	echo "unknown action"
	exit 2
	;;
esac
