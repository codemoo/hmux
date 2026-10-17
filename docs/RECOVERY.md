# Home reboot recovery

HMux checkpoints Home tmux while its connector is running. On the next connection
after Home reboots, it recreates missing sessions before publishing the first
catalog. Recovery itself installs no service. A connector started manually or by
the optional [Home user service](OPERATIONS.md#automatic-home-startup-macos-and-linux) recovers before
publishing its first catalog.

Restoring sessions starts the tmux server automatically. If the saved checkpoint
is empty after a reboot and no live sessions exist, HMux creates a detached
`hmux` login shell in Home's home directory to keep the server running. This does
not resurrect deleted work. Same-boot deletion stays empty; a first connection
without any checkpoint only records the current state.

## What returns

The checkpoint contains original sessions (not temporary grouped browser views),
windows, pane layout, active window/pane, working directories and Home display
metadata. An exactly bound Codex or Claude conversation resumes in its pane.
Other panes reopen as shells in their saved directories. Running shell commands,
tool executions, process memory and terminal scrollback are not replayed.

Codex resumes with its explicit conversation ID and original `CODEX_HOME`.
Existing codex-lb routing in that config stays effective; codex-lb is a provider
proxy, not a `resume` executable. Claude resumes with its explicit conversation
ID and original `CLAUDE_CONFIG_DIR`, including cswap account profiles. Credentials
are neither stored in the checkpoint nor copied between accounts. Provider
installation, configuration, transcript files and authentication must still be
available on Home.

The common resolver used by catalog and conversation also owns checkpoint
bindings. Every checkpoint resolves the running pane again, then checks a second
observation. `/new`, explicit resume and provider restarts update the saved ID;
ambiguous or changing bindings do not keep an old ID as the current conversation.
The initial recovered checkpoint retains the intended launch reference until
the next checkpoint: restore captures the layout, verifies all mapped panes and
merges its explicit saved IDs without requiring newly launched clients to expose
a transcript yet. Other live panes keep only bindings confirmed by both inspection
passes; missing observations are allowed only in this initial restored capture. Every subsequent save replaces it with the current verified
binding. A missing, incomplete, ambiguous or changing provider observation rejects
the whole save and leaves the previous checkpoint byte-for-byte intact. A verified
shell-only pane can clear its old provider reference; an unresolved running provider
cannot. Sessions sharing a working directory keep separate explicit conversation
IDs; neither cwd nor the newest transcript selects a conversation.

Codex background-server clients may not own a rollout file descriptor. For those
clients, recovery can read a full UUID from the current, otherwise empty composer's
footer when Codex is configured with `tui.status_line = ["session-id"]`. It verifies
the exact pane, foreground provider process, trusted rollout header and unchanged
HMux input epoch, then checks the footer again. The receipt exists only within
that save. Cached reader links, retained `/status` panels and unchanged process IDs
are insufficient recovery evidence.

This fallback is read-only: recovery never submits `/status` or `/statusline`,
changes display settings, widens windows, or replaces a draft. Missing/unsupported
footer configuration, a busy composer or an unresolved provider defers the entire
save. Existing clients need a visible full thread-ID footer or direct rollout
ownership before a fresh checkpoint can be guaranteed. In Codex 0.163.0, use the
interactive `/statusline` settings and select **thread-id**; inline arguments are
not a verified configuration interface. A thread ID appears only after a thread
has started. Other footer items can prevent the exact UUID-only fallback from
matching. Direct input from another terminal is outside HMux's input coordination;
see the reader's [limitations](WEB.md#conversation-reader).

## When it saves

A Home connector synchronizes reboot recovery before its first catalog. A valid,
completed checkpoint for the same boot, or no journal on first installation,
lets the connector serve immediately;
its shared background owner attempts a refresh after five seconds. Successful
saves repeat approximately every 30 seconds, including temporary Gateway/network
disconnections.
Multiple browsers share that same locked checkpoint owner. Without a running
Home connector, it does not continuously record changes. Before a planned reboot,
make a fresh checkpoint on Home:

```sh
~/.local/bin/hmux-agent recovery save
```

For manual Home maintenance:

```sh
~/.local/bin/hmux-agent recovery sync
~/.local/bin/hmux-agent recovery restore
```

`sync` checks the OS boot identity and only automatically restores after a boot
change. `restore` explicitly requests recovery from the saved checkpoint. Existing
session names are never overwritten, attached, detached or killed. Within the
same boot, ordinary checkpointing does not recreate deliberately closed sessions.

macOS uses `kern.bootsessionuuid`, which remains stable through wall-clock
corrections; the old `kern.boottime` value could change microseconds without a
reboot and silently prevent later saves. Legacy timestamp checkpoints migrate
only when a saved tmux session's exact ID, birth time and name still survive.
Without that continuity evidence, normal boot recovery applies and a plain save
does not overwrite the older checkpoint. Linux retains its kernel boot UUID.
An interrupted legacy recovery upgrades its pending journal separately only after
verifying an existing restored target and its exact pane map. An unresolved legacy
journal without that evidence is preserved with a `Changed` error; finish it with
the previous binary before upgrading rather than deleting it.

Capture has a bounded 20-second save budget, including two-pass provider
inspection and read-only footer checks within a shared ten-second bound. No
resident worker or parallel process is added per pane. An unavailable observation
or exhausted budget defers the save; check freshness before a planned reboot.
Capture compares original-session pane data
before/after inspection, ignoring temporary grouped-view churn. Changes to an
original session lifetime, pane PID, cwd or layout reject the save and preserve
the previous checkpoint. Failed observations do not become successful empty
provider bindings.

Home logs fixed `stage=checkpoint` categories at initialization, on save failure
category changes/recovery and at most every five minutes for an unchanged result.
`reason=published` confirms a committed checkpoint; failures retry after five
seconds without stopping terminals. Diagnostic records contain no session labels,
paths, provider IDs or transcript contents. Check `recovery/state.json`'s
`updated_at` privately when verifying freshness; do not publish its contents.

Recovery first saves a private construction intent, then creates an owned temporary
tmux session. All panes wait on a private Home readiness file until the complete
topology and its identity mapping are saved. The temporary session then receives
its saved name. A crash before the mapping is committed can be retried using the
verified private intent. Exact pane IDs are checked before release and finalization.
Releasing the readiness file is idempotent: reconnects
do not kill or restart a provider that has already resumed. If construction fails
before the mapping is saved, only newly created recovery resources may be cleaned
up; existing sessions remain untouched.

## Browser continuity and private state

Tabs keep their tmux identity. Only Home's verified old-to-new identity mapping
lets a saved tab follow a recovered session. A matching name or recycled tmux ID
is insufficient. If a dialog blocks reconnection or terminal creation fails, the
web client retries while retaining the missing tab.

Each completed recovery also rebases the shared web tab store before
pruning older mappings, so tabs follow multiple reboots without an intermediate
client connection. This uses verified identity links, never name matching.

The owner-only recovery state lives under the configured Home `state_dir` in
`recovery/`; writes use a lock and atomic replacement. It contains local paths,
provider IDs and tmux metadata, so keep it private. It contains no prompts,
responses, arbitrary saved argv, auth tokens or passwords. Provider IDs and config
paths are not sent to clients; only tmux recovery lineage is included in catalogs.

Recovery tests use fake providers and isolated `hmux-e2e-*` tmux sessions. They do
not reboot Home or manipulate pre-existing sessions. Check
[validation](VALIDATION.md) for the actual validation performed on this change.

Command reference: [Codex CLI resume](https://developers.openai.com/codex/cli/reference/).
The locally installed Codex `resume --help` and Claude `--help` were also checked
for explicit session-ID resume arguments during implementation.
