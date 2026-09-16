# Operations

## Setup

Home owns tmux, agent metadata and provider authentication. Remote Macs own
their SSH keys and native UI. The DMZ owns private inventory and signed
releases. Do not commit private topology or credentials.

On Home, prepare private config from the examples before running the explicit
runtime installer:

```bash
mkdir -p ~/.config/hmux
chmod 700 ~/.config/hmux
# Copy an example only when the destination does not already exist.
cp -n config/client.example.toml ~/.config/hmux/client.toml
cp -n config/inventory.example.toml ~/.config/hmux/inventory.toml
chmod 600 ~/.config/hmux/client.toml ~/.config/hmux/inventory.toml
```

Set `role = "home"` and `client_id = "home-mac"`, use real private inventory values, and choose
existing profile directories and executable argument arrays. The signing
public key must come from a trusted source. Verify its fingerprint, then place
it at `~/.config/hmux/release-public-key.pem` with mode `0600`; compare and
back up an existing pin before any authorized key rotation.

```bash
make build
scripts/bootstrap-macos.sh
```

The bootstrap installs host runtime tools only; it does not install the archived
terminal UI unless `HMUX_INSTALL_LEGACY_UI=1` is explicitly set.
It installs the Go runtime/agent/controller with timestamped
backups and managed SSH includes. It does not install the native app. For a
Home setup, the include alone does not create working aliases: enroll the DMZ
host key and alias through the existing trusted SSH setup, then use the Go
helper's `sync --dry-run` and reviewed `sync` to install generated aliases.
For a remote Mac, use [external provisioning](../scripts/EXTERNAL_PROVISIONING_README.md)
instead; generate keys locally and authorize only their public fingerprints.

Check effective SSH settings before connecting: ProxyJump on the Home alias,
ForwardAgent no, IdentitiesOnly yes and normal known-host handling.
Personal overrides use OpenSSH's first-obtained-value rule.

## Native build and installation

Run from the repository root on an Apple Silicon Mac with Xcode, Go, jq and
required command-line tools:

```bash
macos/HMux/scripts/build.sh &&
macos/HMux/scripts/package.sh
```

Outputs are `macos/HMux/build/HMux.app` and
`macos/HMux/build/HMux-<version>-macOS-arm64.zip` with a SHA-256 sidecar.
Builds pin source/tool checksums and do not install or launch the app.

To install the verified local build into `/Applications/HMux.app` and launch it:

```bash
scripts/install-hmux-app.sh --local --system
```

This path uses the package just built in this repository and checks its embedded
source digest against the current build inputs. It does not download a release.
The destination must be writable by the installing user. The previous bundle is
kept with a timestamp, and the replacement must report readiness before earlier
instances at that exact destination are retired. `--local` without `--system`
installs to `~/Applications/HMux.app`.

The explicit `scripts/install-hmux-app.sh` downloads the exact `VERSION`
archive and checksum from `hmux-home:Dropbox/`. Publishing that prepared
archive at the expected Home path is a separate release step. The installer
does not search for arbitrary older ZIPs or build source. It validates archive
entries, bundle ID/version/CPU/config and code signature, keeps a timestamped
previous bundle, and launches a replacement with a nonce-bound readiness
handshake before retiring earlier app processes.

The installer trusts the already authenticated Home SSH source for both
archive and checksum. It is distinct from the Ed25519 DMZ update path.
No global shell function is installed by this repository's native installer;
use its explicit path. For ordinary use, open the installed `HMux.app` in
`/Applications` or `~/Applications`, according to the installation mode.

The app uses bundled Ghostty configuration. A standalone Ghostty installation,
fzf and its managed font setup are only needed for [CLI compatibility](CLI_COMPATIBILITY.md).

## Publish a signed DMZ release

Use a matching, host-native `hmux-control` binary on the trusted build Mac.
Do not run the cross-built Linux executable on macOS. Generate a signing key
only for initial setup, never over an existing private key:

```bash
dist/darwin-arm64/hmux-control keygen \
  --private "$HMUX_SIGNING_KEY" --public "$HMUX_RELEASE_PUBLIC_KEY"
```

Publish artifacts with `hmux-control publish --signing-key ... --artifact ...`
into a local staging root. Copy the complete release directory to a DMZ
temporary directory, verify and atomically rename it under releases.
Keep the private signer off the DMZ and retain at least three good releases.
A released version is immutable.

| Role | Signed platform |
| --- | --- |
| Go client | darwin-arm64 / darwin-amd64 |
| Home agent | darwin-arm64-agent / darwin-amd64-agent |
| Native archive | darwin-arm64-app |

The role is covered by the manifest signature. Runtime checks are bounded;
native checks occur at most every six hours while the app is running.
Installation preserves the current window and offers explicit Restart.
Use [Rollback](ROLLBACK.md) for a failed release.

## Session and inventory administration

```bash
HMUX_HELPER="$HOME/Applications/HMux.app/Contents/Helpers/hmux"
"$HMUX_HELPER" --no-update-check doctor --json
"$HMUX_HELPER" --no-update-check workflow --json
"$HMUX_HELPER" --no-update-check host add --dry-run --file host-request.json
"$HMUX_HELPER" --no-update-check host diff
```

After reviewing a host request, use the same host add command without dry-run.
The client sends bounded JSON on stdin to an allowlisted control command.
Inventory snapshots and render validation preserve last-good outputs on failure.
Deletion and automatic Termius reverse sync are absent.

Native session actions use Home-owned identity-checked metadata. Closing or
hiding a tab/session does not end work. Termination is separately confirmed.
Profiles contain executable argument arrays and existing Home directories;
creating a profile-backed session never submits a prompt.
Native create requires `structured-create-v1` from Home; update the Home agent
before using creation from a new remote app. Older agents remain usable for
their supported operations and return a clear upgrade error for creation.

## Diagnostics and optional integrations

```bash
hmux-agent version
hmux-agent capabilities
hmux-agent doctor
hmux-control health
systemctl --user list-timers 'hmux-*'
```

Run Home commands on Home and systemd commands on the DMZ. Do not paste private
topology or credentials into reports.

[Codex workflows](CODEX_WORKFLOWS.md) describes optional hook installation and
trust review. [Termius integration](TERMIUS_INTEGRATION.md) describes supported
UI import and phone validation. [Validation](VALIDATION.md) separates local
checks from real remote/device acceptance.

Root logind session cleanup is an unimplemented historical proposal, not part
of installation. No macOS launch agent or additional public service is required.
