#!/bin/sh
set -eu

CLIENT_ID="${1:-}"
SOURCE_ALIAS="${2:-}"
case "$CLIENT_ID" in office-mac | macbook) ;; *)
	echo "usage: $0 <office-mac|macbook> [existing-trusted-dmz-alias]" >&2
	exit 1
	;;
esac
if [ -n "$SOURCE_ALIAS" ]; then
	case "$SOURCE_ALIAS" in -* | *[!A-Za-z0-9._-]*) exit 1 ;; esac
fi

SCRIPT_ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
REPO="${HMUX_REPO_ROOT:-$(dirname -- "$SCRIPT_ROOT")}"
case "$REPO" in /*) ;; *)
	echo "HMUX_REPO_ROOT must be an absolute path" >&2
	exit 1
	;;
esac
KEYS_ROOT="${HMUX_KEYS_ROOT:-$HOME/.config/hmux/provisioning}"
case "$KEYS_ROOT" in /*) ;; *)
	echo "HMUX_KEYS_ROOT must be an absolute path" >&2
	exit 1
	;;
esac
[ ! -L "$KEYS_ROOT" ] && [ -d "$KEYS_ROOT" ] || {
	echo "hmux shared provisioning directory is unavailable or unsafe" >&2
	exit 1
}
ARCH="$(uname -m)"
case "$ARCH" in
arm64) PLATFORM="darwin-arm64" ;;
x86_64) PLATFORM="darwin-amd64" ;;
*)
	echo "unsupported architecture: $ARCH" >&2
	exit 1
	;;
esac
CONTROL="${HMUX_CONTROL_BIN:-$REPO/dist/$PLATFORM/hmux-control}"
case "$CONTROL" in /*) ;; *)
	echo "HMUX_CONTROL_BIN must be an absolute path" >&2
	exit 1
	;;
esac
[ ! -L "$CONTROL" ] && [ -f "$CONTROL" ] && [ -x "$CONTROL" ] || {
	echo "build artifacts are missing under $REPO/dist" >&2
	exit 1
}

STAMP="$(date -u +%Y%m%dT%H%M%SZ)-$$"
BACKUP="$HOME/.config/hmux/backups/$STAMP"
mkdir -p "$BACKUP" "$HOME/.ssh/config.d" "$HOME/.config/hmux" "$HOME/.cache/hmux/releases" "$HOME/.local/bin"
chmod 700 "$BACKUP" "$HOME/.ssh" "$HOME/.ssh/config.d" "$HOME/.config/hmux" "$HOME/.cache/hmux"
for file in "$HOME/.ssh/config" "$HOME/.ssh/known_hosts" "$HOME/.config/hmux/client.toml"; do
	[ -e "$file" ] && cp -p "$file" "$BACKUP/$(basename "$file")"
done

ensure_client_key() {
	target="$1"
	key="$HOME/.ssh/hmux_${target}_ed25519"
	if [ -L "$key" ] || { [ -e "$key" ] && [ ! -f "$key" ]; }; then
		echo "unsafe private key path for $target" >&2
		exit 1
	fi
	if [ ! -e "$key" ]; then
		ssh-keygen -q -t ed25519 -N '' -C "hmux-$CLIENT_ID-$target" -f "$key"
	fi
	[ "$(stat -f '%u' "$key")" = "$(id -u)" ] &&
		[ -z "$(find "$key" -prune -perm -077 -print)" ] || {
		echo "private key ownership or mode is unsafe for $target" >&2
		exit 1
	}
	chmod 600 "$key"
	derived_body="$(ssh-keygen -y -f "$key" | awk '$1=="ssh-ed25519"{print $2; exit}')"
	[ -n "$derived_body" ] || {
		echo "private key is not Ed25519 for $target" >&2
		exit 1
	}
	if [ -L "$key.pub" ] || { [ -e "$key.pub" ] && [ ! -f "$key.pub" ]; }; then
		echo "unsafe public key path for $target" >&2
		exit 1
	fi
	if [ -f "$key.pub" ]; then
		stored_body="$(awk '$1=="ssh-ed25519"{print $2; exit}' "$key.pub")"
		[ "$stored_body" = "$derived_body" ] || {
			echo "public/private key mismatch for $target" >&2
			exit 1
		}
	else
		tmp_public="$(mktemp "$HOME/.ssh/.hmux-${target}-public.XXXXXX")"
		printf 'ssh-ed25519 %s hmux-%s-%s\n' "$derived_body" "$CLIENT_ID" "$target" >"$tmp_public"
		chmod 644 "$tmp_public"
		mv "$tmp_public" "$key.pub"
	fi
	chmod 644 "$key.pub"
}

for target in dmz home; do
	ensure_client_key "$target"
done

REQUEST="$KEYS_ROOT/requests/$CLIENT_ID"
mkdir -p "$REQUEST"
chmod 700 "$KEYS_ROOT/requests" "$REQUEST"
for name in dmz home; do
	request_target="$REQUEST/$name.pub"
	local_public="$HOME/.ssh/hmux_${name}_ed25519.pub"
	if [ -f "$request_target" ] && ! cmp -s "$local_public" "$request_target"; then
		cp -p "$request_target" "$BACKUP/request-$name.pub"
	fi
	install -m 644 "$local_public" "$request_target"
done

pin_known_host() {
	host_id="$1"
	address="$2"
	port="$3"
	public_key_file="$4"
	case "$port" in
	'' | *[!0-9]*)
		echo "invalid port for $host_id" >&2
		exit 1
		;;
	esac
	if [ "$port" = 22 ]; then
		known_host="$address"
	else
		known_host="[$address]:$port"
	fi
	pinned_body="$(awk '$1=="ssh-ed25519" && $2 ~ /^[A-Za-z0-9+\/=]+$/ {print $2;exit}' "$public_key_file")"
	[ -n "$pinned_body" ] || {
		echo "invalid pinned host key for $host_id" >&2
		exit 1
	}
	existing_bodies="$(ssh-keygen -F "$known_host" -f "$HOME/.ssh/known_hosts" 2>/dev/null |
		awk '$2=="ssh-ed25519"{print $3}')"
	if [ -n "$existing_bodies" ]; then
		printf '%s\n' "$existing_bodies" | awk -v body="$pinned_body" '
      $0==body {found=1}
      END {exit !found}
    ' || {
			echo "existing host key differs from the pinned key for $host_id" >&2
			exit 1
		}
		return
	fi
	printf '%s ssh-ed25519 %s\n' "$known_host" "$pinned_body" >>"$HOME/.ssh/known_hosts"
}

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/hmux-client.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
mkdir -p "$TMP_ROOT/control/inventory"

touch "$HOME/.ssh/known_hosts"
chmod 600 "$HOME/.ssh/known_hosts"
BOOTSTRAP_SSH_CONFIG=""
if [ -z "$SOURCE_ALIAS" ]; then
	BOOTSTRAP_JSON="${HMUX_DMZ_BOOTSTRAP_CONFIG:-$KEYS_ROOT/dmz-bootstrap.json}"
	[ ! -L "$BOOTSTRAP_JSON" ] && [ -f "$BOOTSTRAP_JSON" ] || {
		echo "hmux: missing shared DMZ bootstrap topology" >&2
		echo "run sync-external-provisioning-assets.sh on HOME_MAC first" >&2
		exit 2
	}
	[ "$(stat -f '%u' "$BOOTSTRAP_JSON")" = "$(id -u)" ] &&
		[ -z "$(find "$BOOTSTRAP_JSON" -prune -perm -022 -print)" ] || {
		echo "hmux: shared DMZ bootstrap topology has unsafe ownership or mode" >&2
		exit 1
	}
	if ! jq -e '
      type == "object" and keys == ["dmz", "schema_version"] and
      .schema_version == 1 and
      (.dmz | type == "object" and keys == ["address", "port", "user"]) and
      (.dmz.address | type == "string" and length >= 1 and length <= 253) and
      (.dmz.user | type == "string" and length >= 1 and length <= 64) and
      (.dmz.port | type == "number" and floor == . and . >= 1 and . <= 65535)
    ' "$BOOTSTRAP_JSON" >/dev/null; then
		echo "hmux: invalid shared DMZ bootstrap topology" >&2
		exit 1
	fi
	dmz_address="$(jq -er '.dmz.address' "$BOOTSTRAP_JSON")"
	dmz_user="$(jq -er '.dmz.user' "$BOOTSTRAP_JSON")"
	dmz_port="$(jq -er '.dmz.port' "$BOOTSTRAP_JSON")"
	case "$dmz_address" in '' | -* | *[!A-Za-z0-9.-]*)
		echo "hmux: unsupported DMZ bootstrap address" >&2
		exit 1
		;;
	esac
	case "$dmz_user" in '' | -* | *[!A-Za-z0-9._-]*)
		echo "hmux: unsupported DMZ bootstrap user" >&2
		exit 1
		;;
	esac
	pin_known_host dmz "$dmz_address" "$dmz_port" "$KEYS_ROOT/dmz-ssh-host-ed25519.pub"
	BOOTSTRAP_SSH_CONFIG="$TMP_ROOT/bootstrap.ssh.conf"
	{
		printf 'Host hmux-bootstrap-dmz\n'
		printf '    HostName %s\n' "$dmz_address"
		printf '    User %s\n' "$dmz_user"
		printf '    Port %s\n' "$dmz_port"
		printf '    IdentityFile ~/.ssh/hmux_dmz_ed25519\n'
		printf '    IdentitiesOnly yes\n'
		printf '    ForwardAgent no\n'
		printf '    BatchMode yes\n'
		printf '    StrictHostKeyChecking yes\n'
		printf '    UserKnownHostsFile ~/.ssh/known_hosts\n'
	} >"$BOOTSTRAP_SSH_CONFIG"
	chmod 600 "$BOOTSTRAP_SSH_CONFIG"
	ssh -G -F "$BOOTSTRAP_SSH_CONFIG" hmux-bootstrap-dmz >/dev/null
fi

bootstrap_ssh() {
	if [ -n "$SOURCE_ALIAS" ]; then
		ssh -o BatchMode=yes "$SOURCE_ALIAS" -- "$@"
	else
		ssh -F "$BOOTSTRAP_SSH_CONFIG" hmux-bootstrap-dmz -- "$@"
	fi
}

# Keep HOME expansion on the DMZ, not on the client Mac.
# shellcheck disable=SC2016
REMOTE_CONTROL='$HOME/.local/bin/hmux-control'
if ! CONTROL_OUTPUT="$(bootstrap_ssh "$REMOTE_CONTROL" version 2>/dev/null)"; then
	echo "hmux: public-key authorization is pending for $CLIENT_ID" >&2
	echo "on HOME_MAC run: $KEYS_ROOT/authorize-external-request.sh $CLIENT_ID" >&2
	echo "then run hmux again on this Mac" >&2
	exit 2
fi
CONTROL_VERSION="$(printf '%s\n' "$CONTROL_OUTPUT" | awk '$1=="hmux-control"{print $2; exit}')"
if ! awk -v version="$CONTROL_VERSION" 'BEGIN {
  sub(/[-+].*$/, "", version)
  count=split(version, part, ".")
  if (count < 3) exit 1
  for (i=1; i<=3; i++) if (part[i] !~ /^[0-9]+$/) exit 1
  ok=(part[1] > 0 || (part[1] == 0 && part[2] > 1) ||
      (part[1] == 0 && part[2] == 1 && part[3] >= 3))
  exit !ok
}'; then
	echo "hmux-control 0.1.3 or newer must be deployed on the DMZ before provisioning" >&2
	exit 1
fi

if [ -n "$SOURCE_ALIAS" ]; then
	# Optional compatibility path for a Mac that already has a trusted alias.
	scp -q "$HOME/.ssh/hmux_dmz_ed25519.pub" \
		"$SOURCE_ALIAS":.local/share/hmux-control/state/client-key.pub.incoming
	bootstrap_ssh "$REMOTE_CONTROL" client-key authorize
fi

bootstrap_ssh "$REMOTE_CONTROL" rendered inventory >"$TMP_ROOT/control/inventory/inventory.toml"
HMUX_CONTROL_ROOT="$TMP_ROOT/control" "$CONTROL" reconcile >/dev/null

touch "$HOME/.ssh/config"
chmod 600 "$HOME/.ssh/config" "$HOME/.ssh/known_hosts"
if ! grep -Eq '^[[:space:]]*Include[[:space:]].*config\\.d' "$HOME/.ssh/config"; then
	tmp="$(mktemp "$HOME/.ssh/.config.hmux.XXXXXX")"
	trap 'rm -rf "$TMP_ROOT"; rm -f "$tmp"' EXIT HUP INT TERM
	{
		echo 'Include ~/.ssh/config.d/*.conf'
		cat "$HOME/.ssh/config"
	} >"$tmp"
	chmod 600 "$tmp"
	ssh -G -F "$tmp" localhost >/dev/null
	mv "$tmp" "$HOME/.ssh/config"
	trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
fi
install -m 600 "$TMP_ROOT/control/rendered/ssh/50-hmux.generated.conf" "$HOME/.ssh/config.d/50-hmux.generated.conf"
install -m 600 "$TMP_ROOT/control/inventory/inventory.toml" "$HOME/.config/hmux/inventory.toml"
install -m 600 "$REPO/config/client.example.toml" "$HOME/.config/hmux/client.toml"
perl -pi -e "s/client_id = \"office-mac\"/client_id = \"$CLIENT_ID\"/" "$HOME/.config/hmux/client.toml"
install -m 644 "$KEYS_ROOT/release-public-key.pem" "$HOME/.config/hmux/release-public-key.pem"

HOSTS_JSON="$("$CONTROL" --root "$TMP_ROOT/control" host list)"
for host_id in dmz home; do
	host_address="$(printf '%s\n' "$HOSTS_JSON" | jq -er --arg id "$host_id" '.[]|select(.id==$id)|.address')"
	host_port="$(printf '%s\n' "$HOSTS_JSON" | jq -er --arg id "$host_id" '.[]|select(.id==$id)|.port')"
	pin_known_host "$host_id" "$host_address" "$host_port" \
		"$KEYS_ROOT/$host_id-ssh-host-ed25519.pub"
done

CURRENT_LINK="$HOME/.cache/hmux/current"
if [ -L "$CURRENT_LINK" ]; then
	current_target="$(readlink "$CURRENT_LINK")"
	case "$current_target" in
	releases/*/hmux)
		current_version="${current_target#releases/}"
		current_version="${current_version%/hmux}"
		if ! printf '%s\n' "$current_version" |
			grep -Eq '^[0-9]+(\.[0-9]+){1,3}([-+][A-Za-z0-9][A-Za-z0-9.-]{0,31})?$'; then
			current_version=
		fi
		if [ -n "$current_version" ]; then
			release_dir="$HOME/.cache/hmux/releases/$current_version"
			if [ -d "$release_dir" ] && [ ! -f "$release_dir/manifest.json" ]; then
				mv "$release_dir" "$BACKUP/incomplete-release-$current_version"
				rm "$CURRENT_LINK"
			fi
		fi
		;;
	esac
fi
install -m 700 "$REPO/scripts/hmux-bootstrap" "$HOME/.local/bin/hmux"

validate_effective_ssh() {
	alias="$1"
	expected_jump="$2"
	effective="$(ssh -G "$alias" 2>/dev/null)"
	printf '%s\n' "$effective" | grep -Eq '^forwardagent no$'
	printf '%s\n' "$effective" | grep -Eq '^identitiesonly yes$'
	if printf '%s\n' "$effective" | grep -Eq '^stricthostkeychecking no$'; then
		echo "host-key checking is disabled for $alias" >&2
		return 1
	fi
	if printf '%s\n' "$effective" | grep -Eq '^userknownhostsfile /dev/null$'; then
		echo "known_hosts verification is disabled for $alias" >&2
		return 1
	fi
	if [ -n "$expected_jump" ]; then
		printf '%s\n' "$effective" | grep -Eq "^proxyjump $expected_jump$"
	fi
}
validate_effective_ssh hmux-dmz ""
validate_effective_ssh hmux-home hmux-dmz
ssh -o BatchMode=yes hmux-dmz -- true
"$HOME/.local/bin/hmux" update
if ! ssh -o BatchMode=yes hmux-home -- true 2>/dev/null; then
	echo "HOME_MAC authorization request created: $REQUEST/home.pub" >&2
	echo "authorize it on HOME_MAC and rerun this command" >&2
	exit 2
fi
"$HOME/.local/bin/hmux" version --json >/dev/null
PROVISIONED_TMP="$(mktemp "$HOME/.config/hmux/.provisioned-client.XXXXXX")"
printf '%s\n' "$CLIENT_ID" >"$PROVISIONED_TMP"
chmod 600 "$PROVISIONED_TMP"
mv "$PROVISIONED_TMP" "$HOME/.config/hmux/provisioned-client"
echo "hmux provisioning complete; private keys remained on this Mac"
