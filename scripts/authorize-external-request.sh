#!/bin/sh
set -eu

CLIENT_ID="${1:-}"
SOURCE_ALIAS="${2:-${HMUX_DMZ_BOOTSTRAP_ALIAS:-hmux-dmz}}"
case "$CLIENT_ID" in office-mac | macbook) ;; *)
	echo "usage: $0 <office-mac|macbook> [existing-trusted-dmz-alias]" >&2
	exit 1
	;;
esac
case "$SOURCE_ALIAS" in '' | -* | *[!A-Za-z0-9._-]*)
	echo "invalid trusted DMZ alias" >&2
	exit 1
	;;
esac

HMUX_KEYS_ROOT="${HMUX_KEYS_ROOT:-$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)}"
case "$HMUX_KEYS_ROOT" in /*) ;; *)
	echo "HMUX_KEYS_ROOT must be an absolute path" >&2
	exit 1
	;;
esac
[ ! -L "$HMUX_KEYS_ROOT" ] && [ -d "$HMUX_KEYS_ROOT" ] || {
	echo "unsafe hmux shared provisioning directory" >&2
	exit 1
}
REQUEST_DIR="$HMUX_KEYS_ROOT/requests/$CLIENT_ID"
DMZ_PUBLIC="$REQUEST_DIR/dmz.pub"
HOME_PUBLIC="$REQUEST_DIR/home.pub"
for public_key in "$DMZ_PUBLIC" "$HOME_PUBLIC"; do
	[ ! -L "$public_key" ] && [ -f "$public_key" ] || {
		echo "missing or unsafe request public key" >&2
		exit 1
	}
	[ "$(wc -l <"$public_key" | tr -d ' ')" = "1" ] || exit 1
	awk '$1=="ssh-ed25519" && $2 ~ /^[A-Za-z0-9+\/=]+$/ {ok=1} END{exit !ok}' "$public_key"
done

echo "Review these client key fingerprints:"
printf 'DMZ:  '
ssh-keygen -lf "$DMZ_PUBLIC" -E sha256
printf 'HOME: '
ssh-keygen -lf "$HOME_PUBLIC" -E sha256
printf "Type the exact client id '%s' to authorize: " "$CLIENT_ID"
IFS= read -r confirmation
[ "$confirmation" = "$CLIENT_ID" ] || {
	echo "authorization cancelled" >&2
	exit 1
}

# Carry only the reviewed public key through the Home Mac's trusted alias.
scp -q "$DMZ_PUBLIC" \
	"$SOURCE_ALIAS":.local/share/hmux-control/state/client-key.pub.incoming
ssh -o BatchMode=yes "$SOURCE_ALIAS" -- \
	'$HOME/.local/bin/hmux-control' client-key authorize

STAMP="$(date -u +%Y%m%dT%H%M%SZ)-$$"
mkdir -p "$HOME/.ssh"
chmod 700 "$HOME/.ssh"
[ ! -L "$HOME/.ssh/authorized_keys" ] || {
	echo "refusing to modify a symlinked authorized_keys file" >&2
	exit 1
}
touch "$HOME/.ssh/authorized_keys"
chmod 600 "$HOME/.ssh/authorized_keys"
cp -p "$HOME/.ssh/authorized_keys" "$HOME/.ssh/authorized_keys.hmux-backup-$STAMP"
body="$(awk 'NR==1{print $2}' "$HOME_PUBLIC")"
if ! awk -v body="$body" '
  {
    for (i=1; i<NF; i++) {
      if ($i=="ssh-ed25519" && $(i+1)==body) found=1
    }
  }
  END {exit !found}
' "$HOME/.ssh/authorized_keys"; then
	tmp="$(mktemp "$HOME/.ssh/.authorized_keys.hmux.XXXXXX")"
	trap 'rm -f "$tmp"' EXIT HUP INT TERM
	{
		awk '1' "$HOME/.ssh/authorized_keys"
		awk 'NR==1{print}' "$HOME_PUBLIC"
	} >"$tmp"
	chmod 600 "$tmp"
	mv "$tmp" "$HOME/.ssh/authorized_keys"
	trap - EXIT HUP INT TERM
fi

echo "$CLIENT_ID public keys authorized on DMZ_HOST and HOME_MAC"
