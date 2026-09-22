# Home reboot recovery

HMux checkpoints Home tmux while its foreground catalog connection is open. On
the next connection after Home reboots, it recreates missing sessions before
publishing the first catalog. No LaunchAgent, login item or daemon is installed;
recovery starts when HMux connects, not before Home login/SSH becomes available.

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
the next checkpoint. Every subsequent save replaces it with the current verified
binding, including clearing unavailable/ambiguous references.

## When it saves

A Home stream checkpoints on connection and approximately every 30 seconds while
connected. Multiple clients share the same locked Home state. With no HMux
connection, it does not continuously record changes. Before a planned reboot,
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
