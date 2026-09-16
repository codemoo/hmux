#!/bin/sh
set -eu

[ "$(uname -s)" = Darwin ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0
command -v ssh-keygen >/dev/null 2>&1 || exit 0

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/hmux-external-flow.XXXXXX")"
trap 'rm -rf "$TEST_ROOT"' EXIT HUP INT TERM
HOME_DIR="$TEST_ROOT/home"
KEYS_ROOT="$TEST_ROOT/keys"
BIN_DIR="$TEST_ROOT/bin"
mkdir -p "$HOME_DIR/.ssh" "$KEYS_ROOT/requests/office-mac" "$BIN_DIR"
chmod 700 "$HOME_DIR/.ssh" "$KEYS_ROOT" "$KEYS_ROOT/requests" \
	"$KEYS_ROOT/requests/office-mac"

for host in dmz home; do
	ssh-keygen -q -t ed25519 -N '' -f "$TEST_ROOT/$host-host"
	cp "$TEST_ROOT/$host-host.pub" "$KEYS_ROOT/$host-ssh-host-ed25519.pub"
done
printf '%s\n' 'PUBLIC RELEASE KEY' >"$KEYS_ROOT/release-public-key.pem"
printf '%s\n' old >"$KEYS_ROOT/prepare-external-mac.sh"

cat >"$BIN_DIR/ssh" <<'EOF'
#!/bin/sh
set -eu
if [ "${1:-}" = "-G" ]; then
	printf '%s\n' \
		'hostname dmz.example.invalid' \
		'user hmux-test' \
		'port 2222' \
		'forwardagent no'
	exit 0
fi
printf '%s\n' "$*" >>"$HMUX_TEST_SSH_LOG"
EOF
chmod 700 "$BIN_DIR/ssh"
cat >"$BIN_DIR/scp" <<'EOF'
#!/bin/sh
set -eu
printf '%s\n' "$*" >>"$HMUX_TEST_SCP_LOG"
EOF
chmod 700 "$BIN_DIR/scp"

HOME="$HOME_DIR" PATH="$BIN_DIR:$PATH" HMUX_KEYS_ROOT="$KEYS_ROOT" \
	HMUX_TEST_SSH_LOG="$TEST_ROOT/ssh.log" \
	"$ROOT/scripts/sync-external-provisioning-assets.sh" trusted-dmz >/dev/null

jq -e '
  .schema_version == 1 and
  .dmz.address == "dmz.example.invalid" and
  .dmz.user == "hmux-test" and
  .dmz.port == 2222
' "$KEYS_ROOT/dmz-bootstrap.json" >/dev/null
cmp -s "$ROOT/scripts/prepare-external-mac.sh" "$KEYS_ROOT/prepare-external-mac.sh"
cmp -s "$ROOT/scripts/authorize-external-request.sh" "$KEYS_ROOT/authorize-external-request.sh"
test "$(stat -f '%Lp' "$KEYS_ROOT/dmz-bootstrap.json")" = 600
test "$(find "$KEYS_ROOT/backups" -type f -name prepare-external-mac.sh | wc -l | tr -d ' ')" -eq 1

backup_count_before="$(find "$KEYS_ROOT/backups" -type f | wc -l | tr -d ' ')"
HOME="$HOME_DIR" PATH="$BIN_DIR:$PATH" HMUX_KEYS_ROOT="$KEYS_ROOT" \
	HMUX_TEST_SSH_LOG="$TEST_ROOT/ssh.log" \
	"$ROOT/scripts/sync-external-provisioning-assets.sh" trusted-dmz >/dev/null
backup_count_after="$(find "$KEYS_ROOT/backups" -type f | wc -l | tr -d ' ')"
test "$backup_count_before" -eq "$backup_count_after"

for target in dmz home; do
	ssh-keygen -q -t ed25519 -N '' -f "$TEST_ROOT/office-$target"
	cp "$TEST_ROOT/office-$target.pub" "$KEYS_ROOT/requests/office-mac/$target.pub"
done
for _run in 1 2; do
	printf '%s\n' office-mac |
		HOME="$HOME_DIR" PATH="$BIN_DIR:$PATH" HMUX_KEYS_ROOT="$KEYS_ROOT" \
			HMUX_TEST_SSH_LOG="$TEST_ROOT/ssh.log" \
			HMUX_TEST_SCP_LOG="$TEST_ROOT/scp.log" \
			"$ROOT/scripts/authorize-external-request.sh" office-mac trusted-dmz >/dev/null
done

home_body="$(awk 'NR==1{print $2}' "$KEYS_ROOT/requests/office-mac/home.pub")"
test "$(awk -v body="$home_body" '$2==body{count++} END{print count+0}' \
	"$HOME_DIR/.ssh/authorized_keys")" -eq 1
grep -Fq 'dmz.pub trusted-dmz:.local/share/hmux-control/state/client-key.pub.incoming' \
	"$TEST_ROOT/scp.log"
# shellcheck disable=SC2016
grep -Fq 'trusted-dmz -- $HOME/.local/bin/hmux-control client-key authorize' \
	"$TEST_ROOT/ssh.log"

if grep -R -E -l 'BEGIN (OPENSSH |EC |RSA )?PRIVATE KEY' "$KEYS_ROOT" >/dev/null 2>&1; then
	echo "private key material entered external bootstrap assets" >&2
	exit 1
fi
