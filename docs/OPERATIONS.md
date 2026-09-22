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

The installer requires Python 3, preflights both source/target binaries, rejects
symlinks and unsafe ownership/permissions, and creates private timestamped backups.
Each binary is staged and atomically replaced. It then calls `hmux-agent setup-home`
to initialize Home configuration and Codex/Claude/shell profiles.

On a new interactive installation, it asks for the new-session base directory;
press Enter for `~/.hmux`. Noninteractive new installations use that default.
To choose a path explicitly:

```sh
python3 deploy/web/install-home.py --workspace-dir "$HOME/projects"
```

Reinstalling without `--workspace-dir` preserves every existing profile directory,
including custom paths. An explicit `--workspace-dir` updates all profile bases
with a timestamped inventory backup, preserving other settings and legacy fields.
Existing `client.toml` remains authoritative when `home.toml` is absent; `state_dir`
and provider command arguments are preserved. The installer never starts services
or stops a running connector. `--binaries-only` skips configuration entirely.
`--source-dir`, `--bin-dir` and `--config-dir` select alternative locations.
Coordinate connector restart separately; restore binary backup bytes with executable
mode if rolling back. Configuration errors stop the installer, but already installed
binaries remain; fix the configuration error and rerun.

Each inventory profile's `default_directory` is the **base**, not the session CWD.
A new session creates a private child directory derived from the entered name:
letters (including Korean), numbers, hyphens and underscores remain; spaces and
other punctuation become hyphens. Empty names use the profile ID. Names are bounded
for filesystem/tmux limits. Existing folders, files and symlinks are never reused;
a short random suffix distinguishes a repeated name. The tmux name combines the
actual child folder, a bounded profile ID and a random suffix. Two creates with
the same input always create separate workspaces and sessions. `create --dry-run`
only validates and prints the proposed folder slug; it reserves nothing.

For example, `My Project` with the Codex profile creates `<base>/My-Project` and
`My-Project-codex-<suffix>` in tmux. The base is created on first use if missing.
Codex and Claude profiles whose command starts with `codex` or `claude` return to
an interactive Home shell in the same directory after normal exit, failure or
Ctrl+C. The same lifecycle applies to recovered provider sessions; ordinary shell
profiles retain their normal exit behavior. Existing running panes are not rewritten.
A regular recovery checkpoint clears the resume reference after a provider exits;
until that checkpoint is saved, the preceding recovery snapshot may still resume it.

Commands are argument arrays selected by profile ID. Existing configuration remains
compatible; follow [MIGRATION.md](MIGRATION.md) before replacing it. Keep the same
`state_dir` to retain aliases, hidden state, workflows and recovery.

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

`hmux-agent` provides `setup-home`, `doctor`, `catalog`, `recovery`, `workflow`, `workflow-hook`,
`workflow-report`, `conversation`, `workspace`, `create`, `alias-set`, `hidden-set`,
`terminate`, `metadata-migrate` and `version`. These are headless administration
operations, not an alternate user interface. Destructive termination requires
both `--confirmed` and `--created-at`; ordinary browser tab closing never terminates work.
Optional workflow hooks use [CODEX_WORKFLOWS.md](CODEX_WORKFLOWS.md).

Keep credential/session/profile/push stores and Home state private and outside
release directories. Inspect bounded frontend diagnostics through Settings;
never include tokens, transcripts or production topology in public reports.
