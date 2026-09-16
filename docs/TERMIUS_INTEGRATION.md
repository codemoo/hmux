# Termius integration

## Supported baseline

Create only:

1. `DMZ` jump host.
2. `Home Sessions`, targeting the home Mac through that jump, with startup:

```bash
exec ~/.local/bin/hmux-agent select --mobile
```

The home selector runs fzf with a narrow layout and no default preview. Attach
continues in the same TTY. A cancelled selector exits cleanly. New tmux sessions
appear live without a new Termius host.

## Capability gate

The project does not assume that the archived `termius-cli` works with a
current encrypted Vault. It never reads or writes Termius LevelDB, SQLite,
Electron storage, tokens or private-key storage.

For the installed Termius version, complete these checks before enabling any
automated adapter:

- current official, documented API/CLI and supported headless import;
- stable ID preservation and idempotent update/merge;
- imported ProxyJump conversion into a jump-host chain;
- startup snippet support in the import artifact;
- duplicate behavior over at least ten add/update/no-op runs;
- official Vault sync status visibility.

Unless all pass, `hmux-control reconcile` produces
`rendered/termius/hmux-hosts.csv` and reports
`termius_mode=generated-import-artifact`. the embedded helper's `termius-sync --dry-run` reports
that boundary plus a masked row count and hash. A non-dry run validates and
backs up the generated CSV, prepares it under the local hmux generated-config
directory, and opens Termius on macOS. It still returns an incomplete status
because opening the app does not perform supported UI import or Vault sync.
Those steps must be completed and verified separately.

## Desktop import procedure

1. Run reconcile and inspect the CSV diff without secrets in screenshots.
2. In Termius desktop, use its documented host import UI.
3. Map the DMZ host, then configure `Home Sessions` to use it as the jump host.
4. Add the startup snippet above.
5. Confirm no duplicate, then wait for the app's own Vault sync indicator.
6. On iPhone, attach only to a disposable `hmux-e2e-*` session first.

Do not bulk-delete stale hosts during reconcile. Reverse sync from hand-created
Termius hosts is unsupported without an official API.

## Validation levels

- Level 0: CSV generated only.
- Level 1: desktop import and duplicate check confirmed.
- Level 2: desktop Vault sync indicator confirmed.
- Level 3: physical iPhone jump, selector and attach confirmed.

Reports must state the achieved level; desktop observation is not evidence of
an iPhone test.
