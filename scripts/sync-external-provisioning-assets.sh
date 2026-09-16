#!/bin/sh
set -eu

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
SOURCE_ALIAS="${1:-${HMUX_DMZ_BOOTSTRAP_ALIAS:-hmux-dmz}}"
KEYS_ROOT="${HMUX_KEYS_ROOT:-$HOME/.config/hmux/provisioning}"

case "$SOURCE_ALIAS" in '' | -* | *[!A-Za-z0-9._-]*)
	echo "usage: $0 [existing-trusted-dmz-alias]" >&2
	exit 2
	;;
esac
case "$KEYS_ROOT" in /*) ;; *)
	echo "HMUX_KEYS_ROOT must be an absolute path" >&2
	exit 2
	;;
esac
command -v jq >/dev/null 2>&1 || {
	echo "jq is required" >&2
	exit 1
}

mkdir -p "$KEYS_ROOT"
[ ! -L "$KEYS_ROOT" ] && [ -d "$KEYS_ROOT" ] || {
	echo "unsafe shared provisioning directory" >&2
	exit 1
}
[ "$(stat -f '%u' "$KEYS_ROOT")" = "$(id -u)" ] || {
	echo "shared provisioning directory ownership mismatch" >&2
	exit 1
}
chmod 700 "$KEYS_ROOT"

effective="$(ssh -G "$SOURCE_ALIAS" 2>/dev/null)"
address="$(printf '%s\n' "$effective" | awk '$1=="hostname"{print $2;exit}')"
user="$(printf '%s\n' "$effective" | awk '$1=="user"{print $2;exit}')"
port="$(printf '%s\n' "$effective" | awk '$1=="port"{print $2;exit}')"
case "$address" in '' | -* | *[!A-Za-z0-9.-]*)
	echo "trusted alias has an unsupported address" >&2
	exit 1
	;;
esac
case "$user" in '' | -* | *[!A-Za-z0-9._-]*)
	echo "trusted alias has an unsupported user" >&2
	exit 1
	;;
esac
case "$port" in '' | *[!0-9]*)
	echo "trusted alias has an invalid port" >&2
	exit 1
	;;
esac
[ "$port" -ge 1 ] && [ "$port" -le 65535 ] || {
	echo "trusted alias has an invalid port" >&2
	exit 1
}
ssh -o BatchMode=yes "$SOURCE_ALIAS" -- true

for required in \
	"$KEYS_ROOT/dmz-ssh-host-ed25519.pub" \
	"$KEYS_ROOT/home-ssh-host-ed25519.pub" \
	"$KEYS_ROOT/release-public-key.pem"; do
	[ ! -L "$required" ] && [ -f "$required" ] || {
		echo "missing or unsafe public trust asset" >&2
		exit 1
	}
done

stamp="$(date -u +%Y%m%dT%H%M%SZ)-external-bootstrap"
backup_dir="$KEYS_ROOT/backups/$stamp"
backup_ready=0
ensure_backup() {
	if [ "$backup_ready" -eq 0 ]; then
		mkdir -p "$backup_dir"
		chmod 700 "$KEYS_ROOT/backups" "$backup_dir"
		backup_ready=1
	fi
}

install_managed() {
	source=$1
	target=$2
	mode=$3
	label=$4
	if [ -L "$target" ] || { [ -e "$target" ] && [ ! -f "$target" ]; }; then
		echo "unsafe shared provisioning target" >&2
		exit 1
	fi
	if [ -f "$target" ] && cmp -s "$source" "$target"; then
		chmod "$mode" "$target"
		return
	fi
	if [ -f "$target" ]; then
		ensure_backup
		cp -p "$target" "$backup_dir/$label"
	fi
	tmp="$(mktemp "$KEYS_ROOT/.hmux-asset.XXXXXX")"
	trap 'rm -f "$tmp"' EXIT HUP INT TERM
	install -m "$mode" "$source" "$tmp"
	mv "$tmp" "$target"
	trap - EXIT HUP INT TERM
}

bootstrap_tmp="$(mktemp "${TMPDIR:-/tmp}/hmux-dmz-bootstrap.XXXXXX")"
trap 'rm -f "$bootstrap_tmp"' EXIT HUP INT TERM
jq -n \
	--arg address "$address" \
	--arg user "$user" \
	--argjson port "$port" \
	'{schema_version: 1, dmz: {address: $address, user: $user, port: $port}}' \
	>"$bootstrap_tmp"
chmod 600 "$bootstrap_tmp"

install_managed "$bootstrap_tmp" "$KEYS_ROOT/dmz-bootstrap.json" 600 dmz-bootstrap.json
install_managed "$ROOT/scripts/prepare-external-mac.sh" \
	"$KEYS_ROOT/prepare-external-mac.sh" 700 prepare-external-mac.sh
install_managed "$ROOT/scripts/authorize-external-request.sh" \
	"$KEYS_ROOT/authorize-external-request.sh" 700 authorize-external-request.sh
install_managed "$ROOT/scripts/hmux-bootstrap" "$KEYS_ROOT/hmux-bootstrap" 700 hmux-bootstrap
install_managed "$ROOT/scripts/EXTERNAL_PROVISIONING_README.md" \
	"$KEYS_ROOT/EXTERNAL_PROVISIONING_README.md" 600 EXTERNAL_PROVISIONING_README.md

if find "$KEYS_ROOT" -type f \
	-exec grep -E -l 'BEGIN (OPENSSH |EC |RSA )?PRIVATE KEY' {} + 2>/dev/null |
	grep -q .; then
	echo "private key material detected in shared provisioning assets" >&2
	exit 1
fi

rm -f "$bootstrap_tmp"
trap - EXIT HUP INT TERM
if [ "$backup_ready" -eq 1 ]; then
	printf 'hmux external provisioning assets synchronized; backups: %s\n' "$(basename "$backup_dir")"
else
	echo "hmux external provisioning assets already current"
fi
