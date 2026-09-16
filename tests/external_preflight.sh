#!/bin/sh
set -eu

[ "$(uname -s)" = Darwin ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0
command -v ssh-keygen >/dev/null 2>&1 || exit 0

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/hmux-external-preflight.XXXXXX")"
trap 'rm -rf "$TEST_ROOT"' EXIT HUP INT TERM
HOME_DIR="$TEST_ROOT/home"
KEYS_ROOT="$HOME_DIR/Dropbox/dev/Keys/hmux"
BIN_DIR="$TEST_ROOT/bin"
mkdir -p "$HOME_DIR/Dropbox/dev" "$KEYS_ROOT" "$BIN_DIR"
ln -s "$ROOT" "$HOME_DIR/Dropbox/dev/hmux"

for host in dmz home; do
	ssh-keygen -q -t ed25519 -N '' -f "$TEST_ROOT/$host-host"
	cp "$TEST_ROOT/$host-host.pub" "$KEYS_ROOT/$host-ssh-host-ed25519.pub"
done
cat >"$KEYS_ROOT/dmz-bootstrap.json" <<'EOF'
{
  "schema_version": 1,
  "dmz": {
    "address": "dmz.example.invalid",
    "user": "hmux-test",
    "port": 22
  }
}
EOF
chmod 600 "$KEYS_ROOT/dmz-bootstrap.json"

REAL_SSH="$(command -v ssh)"
cat >"$BIN_DIR/ssh" <<'EOF'
#!/bin/sh
set -eu
if [ "${1:-}" = "-G" ]; then
	exec "$HMUX_TEST_REAL_SSH" "$@"
fi
printf '%s\n' "$*" >"$HMUX_TEST_SSH_ARGS"
case "${HMUX_TEST_SSH_MODE:-pending}" in
pending) exit 255 ;;
old)
	printf '%s\n' 'hmux-control 0.1.2 schema=1'
	exit 0
	;;
*) exit 2 ;;
esac
EOF
chmod 700 "$BIN_DIR/ssh"

cat >"$TEST_ROOT/hmux-control" <<'EOF'
#!/bin/sh
exit 99
EOF
chmod 700 "$TEST_ROOT/hmux-control"

run_prepare() {
	HOME="$HOME_DIR" \
		PATH="$BIN_DIR:$PATH" \
		HMUX_KEYS_ROOT="$KEYS_ROOT" \
		HMUX_CONTROL_BIN="$TEST_ROOT/hmux-control" \
		HMUX_TEST_REAL_SSH="$REAL_SSH" \
		HMUX_TEST_SSH_ARGS="$TEST_ROOT/ssh.args" \
		HMUX_TEST_SSH_MODE="$1" \
		"$ROOT/scripts/prepare-external-mac.sh" office-mac
}

if run_prepare pending >"$TEST_ROOT/pending.out" 2>"$TEST_ROOT/pending.err"; then
	echo "external provisioning passed before public-key authorization" >&2
	exit 1
else
	status=$?
fi
[ "$status" -eq 2 ]
grep -Fq 'public-key authorization is pending for office-mac' "$TEST_ROOT/pending.err"
for target in dmz home; do
	test -f "$HOME_DIR/.ssh/hmux_${target}_ed25519"
	test -f "$KEYS_ROOT/requests/office-mac/$target.pub"
done
if grep -R -E -l 'BEGIN (OPENSSH |EC |RSA )?PRIVATE KEY' "$KEYS_ROOT" >/dev/null 2>&1; then
	echo "external private key entered the shared provisioning directory" >&2
	exit 1
fi
grep -Fq 'hmux-bootstrap-dmz' "$TEST_ROOT/ssh.args"
if grep -Fq 'linux-server' "$TEST_ROOT/ssh.args"; then
	echo "shared bootstrap unexpectedly depended on linux-server" >&2
	exit 1
fi
# The fake SSH must receive a literal remote-side $HOME expression.
# shellcheck disable=SC2016
grep -Fq '$HOME/.local/bin/hmux-control version' "$TEST_ROOT/ssh.args"

if run_prepare old >"$TEST_ROOT/old.out" 2>"$TEST_ROOT/old.err"; then
	echo "external provisioning accepted an incompatible DMZ control" >&2
	exit 1
fi
grep -Fq 'hmux-control 0.1.3 or newer' "$TEST_ROOT/old.err"
