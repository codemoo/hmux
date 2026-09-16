#!/bin/sh
set -eu

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/../../.." && pwd)"
CONFIG_DIR="$HOME/.config/hmux"
CONFIG="$CONFIG_DIR/client.toml"
ROLE_FILE="$CONFIG_DIR/machine-role"
HMUX_BIN="$HOME/.local/bin/hmux"
CACHE_CURRENT="$HOME/.cache/hmux/current"
SOURCE_VERSION_FILE="$ROOT/VERSION"

case "$(uname -s)" in
Darwin) ;;
*)
	echo "hmux automatic client setup currently supports macOS only" >&2
	exit 1
	;;
esac

brew_path() {
	if command -v brew >/dev/null 2>&1; then
		command -v brew
	elif [ -x /opt/homebrew/bin/brew ]; then
		printf '%s\n' /opt/homebrew/bin/brew
	elif [ -x /usr/local/bin/brew ]; then
		printf '%s\n' /usr/local/bin/brew
	else
		return 1
	fi
}

ensure_tool() {
	tool="$1"
	package="$2"
	command -v "$tool" >/dev/null 2>&1 && return
	brew="$(brew_path)" || {
		echo "Homebrew is required to install $package" >&2
		exit 1
	}
	echo "hmux: installing required package $package" >&2
	"$brew" install "$package"
	command -v "$tool" >/dev/null 2>&1 || {
		echo "hmux: $tool is still unavailable after installing $package" >&2
		exit 1
	}
}

config_value() {
	key="$1"
	awk -F= -v key="$key" '
    $1 ~ "^[[:space:]]*" key "[[:space:]]*$" {
      value=$2
      sub(/^[[:space:]]*"/, "", value)
      sub(/"[[:space:]]*$/, "", value)
      print value
      exit
    }
  ' "$CONFIG"
}

select_machine_role() {
	if [ -f "$CONFIG" ]; then
		client_id="$(config_value client_id)"
		role="$(config_value role)"
		case "$client_id:$role" in
		home-mac:home | office-mac:remote | macbook:remote)
			printf '%s\n' "$client_id"
			return
			;;
		esac
		echo "hmux: existing client.toml has an unsupported client_id/role pair" >&2
		exit 1
	fi
	if [ -f "$ROLE_FILE" ]; then
		IFS= read -r client_id <"$ROLE_FILE"
		case "$client_id" in home-mac | office-mac | macbook)
			printf '%s\n' "$client_id"
			return
			;;
		esac
		echo "hmux: invalid local machine-role file" >&2
		exit 1
	fi
	[ -r /dev/tty ] || {
		echo "hmux: first setup needs an interactive terminal to select this Mac's role" >&2
		exit 1
	}
	printf '%s\n' \
		"Select this Mac's hmux role:" \
		"  1) home-mac   (the only Mac that owns tmux sessions)" \
		"  2) office-mac (remote client)" \
		"  3) macbook    (remote client)" >&2
	printf 'Role [1-3]: ' >&2
	IFS= read -r choice </dev/tty
	case "$choice" in
	1) client_id=home-mac ;;
	2) client_id=office-mac ;;
	3) client_id=macbook ;;
	*)
		echo "hmux: invalid role selection" >&2
		exit 1
		;;
	esac
	mkdir -p "$CONFIG_DIR"
	chmod 700 "$CONFIG_DIR"
	tmp="$(mktemp "$CONFIG_DIR/.machine-role.XXXXXX")"
	trap 'rm -f "$tmp"' EXIT HUP INT TERM
	printf '%s\n' "$client_id" >"$tmp"
	chmod 600 "$tmp"
	mv "$tmp" "$ROLE_FILE"
	trap - EXIT HUP INT TERM
	printf '%s\n' "$client_id"
}

remote_cache_ready() {
	[ -x "$HMUX_BIN" ] && [ -L "$CACHE_CURRENT" ] && [ -f "$CONFIG" ] || return 1
	[ ! -L "$CONFIG_DIR/provisioned-client" ] &&
		[ -f "$CONFIG_DIR/provisioned-client" ] &&
		[ "$(stat -f '%u' "$CONFIG_DIR/provisioned-client")" = "$(id -u)" ] &&
		[ -z "$(find "$CONFIG_DIR/provisioned-client" -prune -perm -022 -print)" ] || return 1
	IFS= read -r provisioned_client <"$CONFIG_DIR/provisioned-client"
	[ "$provisioned_client" = "$client_id" ] || return 1
	target="$(readlink "$CACHE_CURRENT")"
	case "$target" in releases/*/hmux) ;; *) return 1 ;; esac
	release_dir="${target%/hmux}"
	[ -f "$HOME/.cache/hmux/$release_dir/manifest.json" ]
}

source_version() {
	[ ! -L "$SOURCE_VERSION_FILE" ] && [ -f "$SOURCE_VERSION_FILE" ] || return 1
	IFS= read -r version <"$SOURCE_VERSION_FILE"
	case "$version" in
	[0-9]*.[0-9]*.[0-9]*) ;;
	*) return 1 ;;
	esac
	case "$version" in *[!0-9.]* | *.*.*.*) return 1 ;; esac
	printf '%s\n' "$version"
}

home_runtime_ready() {
	version="$(source_version)" || return 1
	[ -x "$CACHE_CURRENT" ] && [ -x "$HOME/.local/bin/hmux-agent" ] || return 1
	client_line="$("$CACHE_CURRENT" --no-update-check version 2>/dev/null | head -1)"
	agent_line="$("$HOME/.local/bin/hmux-agent" version 2>/dev/null | head -1)"
	case "$client_line" in "hmux $version protocol=1 "*) ;; *) return 1 ;; esac
	case "$agent_line" in "hmux-agent $version protocol=1") ;; *) return 1 ;; esac
}

case "${HMUX_SKIP_PREREQUISITES:-0}" in
0)
	ensure_tool jq jq
	ensure_tool fzf fzf
	if ! "$ROOT/scripts/install-monatendard.sh" --check; then
		echo "hmux: installing verified Monatendard Nerd Font Mono" >&2
		"$ROOT/scripts/install-monatendard.sh"
	fi
	;;
1) ;;
*)
	echo "hmux: invalid HMUX_SKIP_PREREQUISITES value" >&2
	exit 1
	;;
esac

client_id="$(select_machine_role)"
case "$client_id" in
home-mac)
	if ! home_runtime_ready; then
		ensure_tool go go
		echo "hmux: installing the current Home Mac client and agent" >&2
		"$ROOT/scripts/bootstrap-macos.sh"
	fi
	;;
office-mac | macbook)
	if ! remote_cache_ready; then
		alias="${HMUX_DMZ_BOOTSTRAP_ALIAS:-}"
		if [ -n "$alias" ]; then
			case "$alias" in -* | *[!A-Za-z0-9._-]*)
				echo "hmux: invalid HMUX_DMZ_BOOTSTRAP_ALIAS" >&2
				exit 1
				;;
			esac
			echo "hmux: provisioning $client_id through a trusted local DMZ alias" >&2
			"$ROOT/scripts/prepare-external-mac.sh" "$client_id" "$alias"
		else
			echo "hmux: provisioning $client_id from shared public bootstrap assets" >&2
			"$ROOT/scripts/prepare-external-mac.sh" "$client_id"
		fi
	fi
	;;
esac

"$ROOT/archive/terminal/scripts/sync-ui-config-macos.sh"

[ -x "$HMUX_BIN" ] || {
	echo "hmux: installation did not produce $HMUX_BIN" >&2
	exit 1
}

run_client() {
	if [ "$client_id" = home-mac ] && [ -L "$CACHE_CURRENT" ] &&
		[ "$(readlink "$CACHE_CURRENT")" = "releases/local/hmux" ]; then
		# A source-built Home client may contain fixes newer than the last signed
		# central release. Do not let the older release replace it automatically.
		"$HMUX_BIN" --no-update-check "$@"
	else
		"$HMUX_BIN" "$@"
	fi
}

new_launcher_id() {
	command -v uuidgen >/dev/null 2>&1 || {
		echo "hmux: uuidgen is required to create a launcher ID" >&2
		return 1
	}
	id="$(uuidgen | tr -d '-' | tr '[:upper:]' '[:lower:]')"
	case "$id" in '' | *[!0-9a-f]*)
		echo "hmux: uuidgen returned an invalid launcher ID" >&2
		return 1
		;;
	esac
	if [ "${#id}" -ne 32 ]; then
		echo "hmux: uuidgen returned an invalid launcher ID" >&2
		return 1
	fi
	printf '%s\n' "$id"
}

launcher_id=""
cleanup_launcher() {
	[ -n "$launcher_id" ] || return 0
	run_client launcher-cleanup "$launcher_id" >/dev/null 2>&1 || :
}

# A bare interactive `hmux` owns the selector outside tmux. Current clients
# keep selector/frame/selector transitions in one process; this outer loop is a
# compatibility fallback for an older cached client that exits zero after a
# framed attach. Exit 130 is the selector's explicit cancel signal.
force_launcher="${HMUX_FORCE_LAUNCHER:-0}"
case "$force_launcher" in 0 | 1) ;; *)
	echo "hmux: invalid HMUX_FORCE_LAUNCHER value" >&2
	exit 1
	;;
esac
if [ "$#" -eq 0 ] && [ -z "${TMUX:-}" ] &&
	{ [ "$force_launcher" -eq 1 ] || { [ -t 0 ] && [ -t 1 ]; }; }; then
	launcher_id="$(new_launcher_id)"
	export HMUX_LAUNCHER=1
	export HMUX_LAUNCHER_ID="$launcher_id"
	trap cleanup_launcher EXIT
	while :; do
		if run_client "$@"; then
			continue
		else
			status=$?
		fi
		if [ "$status" -eq 130 ]; then
			exit 0
		fi
		exit "$status"
	done
fi

if [ "$client_id" = home-mac ] && [ -L "$CACHE_CURRENT" ] &&
	[ "$(readlink "$CACHE_CURRENT")" = "releases/local/hmux" ]; then
	exec "$HMUX_BIN" --no-update-check "$@"
fi
exec "$HMUX_BIN" "$@"
