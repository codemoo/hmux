# Rollback

## Native app

The signed native updater retains a validated previous bundle. Invoke the
installed bundle's helper explicitly:

```bash
~/Applications/HMux.app/Contents/Helpers/hmux --no-update-check app rollback-native
```

The helper derives its enclosing bundle, validates the owner-controlled backup,
exchanges it under the native update lock, revalidates and records a hold on the
broken version. A later signed version can still update. Quit HMux and reopen
the same installation after success; the original tmux work remains.

The shell installer's `HMux.app.hmux-shell-backup-<timestamp>` is a separate
backup scheme from signed updater backups. Do not assume rollback-native can
select it. On failed shell launch the installer restores its own backup;
manual recovery requires comparing exact bundle versions and signatures first.

## Go runtime and DMZ

```bash
HMUX_HELPER="$HOME/Applications/HMux.app/Contents/Helpers/hmux"
"$HMUX_HELPER" --no-update-check rollback --dry-run <known-good-version>
"$HMUX_HELPER" --no-update-check rollback <known-good-version>
```

This changes only the cached Go runtime selection after revalidating its
manifest, pinned key, signature, size and hash. It does not roll back the app
bundle. If the selected runtime is broken, invoke a known-good signed cached
helper explicitly; do not hand-edit the current link.

On the DMZ use `hmux-control rollback <known-good-version>` followed by
`hmux-control health`. Retain at least three immutable good releases.
Inventory recovery uses a selected history snapshot, validation, atomic
replacement and reconcile.

## Configuration and optional hooks

Compare timestamped installer backups against current user edits before
restoring managed fragments. Native HMux loads its bundled Ghostty config;
standalone Ghostty includes belong only to the compatibility client.
Never re-source the retired target-tmux fragment.

Remove only HMux-managed Codex handlers with:

```bash
scripts/install-codex-workflow-hooks.sh --remove
```

This makes another backup and preserves unrelated handlers. Review the JSON
and Codex trust state afterwards. A separately installed orchestration skill
owns its own installation/recovery; HMux rollback does not rewrite it.
Workflow metadata expires under normal retention.

## Uninstall

The explicit `scripts/uninstall-macos.sh` and `scripts/uninstall-dmz.sh`
tools move managed runtime/config material to recoverable locations.
They do not remove native app bundles, private keys, Termius data or tmux work.
Remove an unwanted native app separately after quitting it and checking its
location. Uninstall is not required for a normal rollback.
