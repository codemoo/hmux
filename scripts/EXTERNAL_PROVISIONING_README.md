# hmux external Mac provisioning

Each client creates two distinct private keys locally: one for the DMZ and one
for the Home Mac. Private keys never enter the shared provisioning directory.

On the Home Mac, refresh the shared public bootstrap bundle whenever the DMZ
connection metadata or provisioning scripts change:

```bash
cd /path/to/hmux
./scripts/sync-external-provisioning-assets.sh
```

This writes a mode-0600 `dmz-bootstrap.json` plus public host/release keys and
generic scripts under `~/.config/hmux/provisioning`. The JSON contains connection
topology but no password, token, identity path or private key. Existing files
are backed up under a timestamped `Keys/hmux/backups/` directory.

Set `HMUX_KEYS_ROOT` to an absolute directory if you use a different location.
The Home commands use your existing trusted `hmux-dmz` SSH alias by default;
set `HMUX_DMZ_BOOTSTRAP_ALIAS` or pass another trusted alias explicitly.
Each client needs a source checkout and its built `hmux-control`. If running a
copied provisioning script, export `HMUX_REPO_ROOT=/absolute/path/to/hmux` first.
Never place this private provisioning directory inside a Git checkout.

On the matching Mac:

```bash
cd ~/.config/hmux/provisioning
./prepare-external-mac.sh office-mac    # or macbook
```

No pre-existing `linux-server` or other local SSH alias is required for this
provisioning step. Complete it before running `scripts/install-hmux-app.sh`
from the repository: the native installer uses the resulting `hmux-home` alias
and does not run the provisioner itself. An already configured Mac may still pass a
trusted local DMZ alias as the second provisioning argument for compatibility.

The DMZ must run `hmux-control 0.1.3` or newer and serve a signed current
release. The installer verifies this before installing generated client
configuration.

The first installer run:

1. backs up existing SSH/hmux config;
2. generates this client's private keys directly under `~/.ssh`;
3. pins the shared DMZ host key before any connection;
4. writes both DMZ and Home public-key requests under
   `Keys/hmux/requests/<client>`;
5. stops with a precise pending-authorization message if those new keys have
   not yet been approved.

On the Home Mac, review and authorize the fingerprints:

```bash
cd ~/.config/hmux/provisioning
./authorize-external-request.sh office-mac  # or macbook
```

The approval command shows both fingerprints and requires typing the exact
client ID. It adds only the reviewed Home public key locally and sends only the
reviewed DMZ public key through the Home Mac's already trusted DMZ alias. Then
transfer the public request/approval bundle over your trusted channel and rerun the matching provisioning script. The missing
local completion marker makes that script resume provisioning. It downloads
the private inventory over the newly authorized connection, installs the
generated ProxyJump fragment and pinned signed client, then verifies both DMZ
and Home connections without querying live tmux sessions. After it completes,
run the repository's `scripts/install-hmux-app.sh` to install and launch a
prepared native release. For daily use, open the installed HMux.app directly.

The legacy CLI entrypoint can install `fzf` locally for the interactive selector
and install the verified official Monatendard release under the current user's
`~/Library/Fonts` when it is missing. The native-app installer does not perform
those provisioning steps. To install `fzf` manually:

```bash
brew install fzf
```

Ghostty remains an application-level prerequisite for the legacy CLI. The
native HMux bundle embeds its terminal runtime and managed configuration.
