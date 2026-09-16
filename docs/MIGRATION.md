# Migration

Migration is an explicit operating task. A source cleanup or local build does
not authorize changing live sessions or remote services.

## From the CLI to the native app

1. Preserve the private Home inventory, client configuration and pinned key.
2. Build and validate matching Go/native artifacts.
3. Install a compatible Home agent using the backed-up runtime installer.
4. Validate SSH and catalog access, then install the native app explicitly.
5. Open a disposable session on an isolated test socket for lifecycle checks.
6. Use the app for daily work; keep the Go helper for diagnostics and recovery.

Native tabs use grouped views and native shortcuts. The old two-server frame,
fzf input modes and standalone Ghostty fragments belong only to the
[compatibility client](CLI_COMPATIBILITY.md).

## Old target-tmux UI

If an old installation contains HMux status options, keys or metadata in the
real target server, first inspect the precise diff and timestamped backup.
The dedicated `scripts/clean-live-target-tmux.sh` migration verifies
session/window/pane/client identities before and after changes and migrates
only recognized metadata.

This is not a normal upgrade prerequisite for a clean native installation.
Do not run the migration as a test or source the retired `archive/terminal/config/tmux.conf`.

## Another Mac or rebuilt DMZ

Use [external provisioning](../scripts/EXTERNAL_PROVISIONING_README.md) for
new client keys and trusted host enrollment. Restore DMZ private inventory and
signed releases from backup, install matching control binaries, validate and
reconcile before enabling the documented user timers.

Rebuilding the DMZ never requires a Home private key. Termius migration uses
supported UI and remains subject to its separate validation levels.
