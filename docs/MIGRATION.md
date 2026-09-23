# Migration to web/PWA only

User interaction is exclusively through the browser/PWA. Desktop app bundles,
terminal selectors/frames, SSH client provisioning, app auto-updates and their
installers/build jobs have been removed. Their source remains in Git history.
Home still supports macOS, tmux, provider CLIs and `hmux-agent` administration/hooks.

## Existing Home installations

1. Back up private configuration and state with timestamps. Keep backups outside
   the repository and public release directories.
2. Build and install the Home binaries using [OPERATIONS.md](OPERATIONS.md).
3. Preserve the current `state_dir` and inventory profiles. Existing
   `~/.config/hmux/client.toml` loads when `home.toml` is absent; known old SSH,
   update and client keys are accepted but ignored. `role = "remote"` is rejected:
   remote devices now use the web URL.
4. Optionally create `~/.config/hmux/home.toml` from `config/home.example.toml`,
   carrying forward the exact inventory/state paths. The new file takes precedence;
   malformed new configuration fails rather than silently falling back.
5. Inventory now needs only `schema_version`, `revision` and `profiles`. Known old
   `clients`, `hosts` and `identity_refs` tables are decoded but unused. Unknown
   fields still fail. Loader compatibility never rewrites personal files.
6. Replace the running connector in a controlled handoff. Temporary browser
   tmux views retain their old marker/name prefix for rolling-upgrade safety.
   Original tmux sessions must remain untouched.

To move a foreground connector to automatic startup, use the opt-in
[Home service installer](OPERATIONS.md#automatic-home-startup-macos-and-linux).
`hmux-web service install --from-running` accepts only one current-user connector,
rechecks its PID/start time/arguments, and signals only that process before starting
the native service. It preserves the existing config, token reference and session
workspaces. New connectors hold a private singleton lock, but old binaries do not;
do not run a manual connector alongside the service. This migration enables a
macOS LaunchAgent or Linux systemd user service, not a new desktop UI.

## Gateway and clients

Keep credentials, persistent login sessions, account profiles, push keys and
usage settings outside releases and preserve them during upgrades. Follow
[RELEASING.md](RELEASING.md) for atomic activation and [ROLLBACK.md](ROLLBACK.md).
Open the web URL or install its PWA; refresh after new assets are published.

Repository cleanup does not uninstall apps, edit shell startup files, alter SSH
configuration or remove personal caches on user machines. Retire those manually
only after confirming they are unused, with timestamped configuration backups.
