# Operations

The supported setup has a macOS Home running tmux/providers and a Linux HTTPS
gateway. Browsers/PWAs are the only clients. Use your own credentials and domain.
Detailed gateway/authentication settings are in [WEB.md](WEB.md).

## Build and install Home

Install Go 1.24+, Node.js 22+, tmux and the desired provider CLIs. Authenticate the
providers as the Home user, then build:

```sh
make build
```

Outputs are `dist/web-linux-amd64/` (gateway/assets) and
`dist/web-darwin-arm64/` (Home connector/admin helper). Build does not deploy.
Install the Home binaries explicitly:

```sh
python3 deploy/web/install-home.py
```

The installer requires Python 3, preflights both source/target files, rejects
symlinks and unsafe ownership/permissions, and creates private timestamped backups.
Each binary is staged and atomically replaced. It never changes configuration,
starts services or stops a running connector. `--source-dir` and `--bin-dir` support
an explicit alternative build/install location. Coordinate connector restart
separately; restore backup bytes with executable mode if rolling back.

Create private configuration only for a new installation:

```sh
mkdir -p "$HOME/.config/hmux"
chmod 700 "$HOME/.config/hmux"
if [ ! -e "$HOME/.config/hmux/home.toml" ] && [ ! -e "$HOME/.config/hmux/client.toml" ]; then
  install -m 600 config/home.example.toml "$HOME/.config/hmux/home.toml"
fi
if [ ! -e "$HOME/.config/hmux/inventory.toml" ]; then
  install -m 600 config/inventory.example.toml "$HOME/.config/hmux/inventory.toml"
fi
```

Edit the inventory's profiles to use installed commands and existing directories.
Commands are argument arrays selected by profile ID. Existing `client.toml` files
continue to load; follow [MIGRATION.md](MIGRATION.md) before replacing configuration.
Keep the same `state_dir` to retain aliases, hidden state, workflows and recovery.

## Start the gateway and connector

Initialize private credentials with `hmux-web init`; configure HTTPS and the
unprivileged gateway service using [WEB.md](WEB.md#linuxnginx-deployment).
Copy only the connector token to private Home storage over a trusted channel.

```sh
~/.local/bin/hmux-web connect --url wss://YOUR_HOST/connect --token-file /PRIVATE/connector.token
```

The connector remains running while web access is needed. No login item or
LaunchAgent is installed. Restart it after a Home reboot; recovery runs before
the first catalog publication. Stopping it disconnects web views without ending
the original tmux/provider processes.

## Administration

`hmux-agent` provides `doctor`, `catalog`, `recovery`, `workflow`, `workflow-hook`,
`workflow-report`, `conversation`, `workspace`, `create`, `alias-set`, `hidden-set`,
`terminate`, `metadata-migrate` and `version`. These are headless administration
operations, not an alternate user interface. Destructive termination requires
both `--confirmed` and `--created-at`; ordinary browser tab closing never terminates work.
Optional workflow hooks use [CODEX_WORKFLOWS.md](CODEX_WORKFLOWS.md).

Keep credential/session/profile/push stores and Home state private and outside
release directories. Inspect bounded frontend diagnostics through Settings;
never include tokens, transcripts or production topology in public reports.
