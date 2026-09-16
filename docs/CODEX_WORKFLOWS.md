# Codex workflows

hmux presents Codex orchestration as a hierarchy attached to the tmux session
that owns the Codex pane:

```text
tmux session
└─ Codex turn or codex-orchestra workflow
   ├─ native Codex subagent
   └─ detached codex-orchestra task
```

The native session list, workflow inspector and compatibility selector use
bounded workflow state. The compatibility selector and tabs use a compact badge. `2▶ 1✓ 1!` means two
running nodes, one completed node and one node needing attention. Attention
combines approval/input waits, failures, interruptions and stale nodes. The
detailed view is available locally and remotely:

```bash
HMUX_HELPER="$HOME/Applications/HMux.app/Contents/Helpers/hmux"
"$HMUX_HELPER" --no-update-check workflow
"$HMUX_HELPER" --no-update-check workflow <session-name-or-stable-id> --json
```

## Sources and lifecycle

`hmux-agent workflow-hook` accepts the official Codex `UserPromptSubmit`,
`SubagentStart`, `SubagentStop`, `PermissionRequest`, `PreToolUse`,
`PostToolUse`, `Stop` and `SessionEnd` events on standard input. It resolves
the inherited `TMUX_PANE` to a stable tmux session ID plus `session_created`.
The creation time prevents a later tmux session from inheriting state after a
stable ID is reused.

A separately installed compatible `codex-orchestra` runner may pass its safe task key through
`hmux-agent workflow-report --task-id ...`. Reports are optional and fail
open: inability to find hmux or write state never changes the worker result.
Likewise, the hook command always emits the valid JSON object required by
Codex and suppresses internal errors, so visibility cannot block a Codex turn.

Lifecycle states are `running`, `waiting_approval`, `waiting_input`,
`completed`, `failed`, `interrupted` and `stale`. An active item with no update
for two hours is displayed as stale. Terminal workflows are retained for up to
seven days and deleted transactionally on the next catalog read or lifecycle
write. State is capped at 1,024 workflows, 128 nodes per workflow and 16 MiB;
old terminal history is evicted first when space is needed. Only the 32 newest
matching workflows are exposed for one live session.

## Privacy and storage

The Home Mac is the workflow source of truth. State lives below the existing
mode-0700 hmux state directory and is protected by current-user ownership and
symlink checks, a no-follow lock, file locking, mode-0600 atomic writes and
directory fsync. Hook input is bounded to 256 KiB.

hmux stores only timestamps, sanitized model/type/provider labels, lifecycle
states and SHA-256-derived identifiers. Codex session, turn, agent and task IDs
are not stored verbatim. Prompts, responses, cwd, transcript or rollout paths,
pane content, complete process arguments, tool inputs/results, tokens and
credentials are not modeled or persisted. Remote clients validate every
workflow status, bound and hashed identifier before rendering catalog data.

No DMZ daemon or public workflow API is required. If centralized history is
introduced later, it should be a bounded service on a Unix socket or loopback
and be reached through SSH; it must not become an Internet-facing endpoint.

## Installation and trust review

Build and install `hmux` and `hmux-agent` first. Then merge the managed
handlers into the global Codex hook file:

```bash
scripts/install-codex-workflow-hooks.sh
```

The installer preserves unrelated handlers, including cmux handlers, and
owns only commands containing `HMUX_WORKFLOW_HOOK=1`. A changed file is backed
up under `~/.config/hmux/backups/<UTC timestamp>/codex-hooks.json`, written
atomically and set to mode `0600`. It refuses symlinks, non-regular files,
invalid JSON and files larger than 1 MiB. Merge input comes from a private
snapshot; the live file is compared with that snapshot immediately before
replacement so a concurrent unrelated edit fails closed instead of being
silently overwritten. Re-running the installer is idempotent.

After any hook change, open Codex `/hooks`, inspect the eight hmux handlers and
approve the trust review. Existing Codex sessions may need to be restarted
before newly discovered global hooks are active.

Optional detached reporting requires a separately installed runner that supports
`--task-id` and invokes `hmux-agent workflow-report` with bounded lifecycle
metadata. HMux does not install or maintain that runner. Its execution,
credential and recovery policy belongs to its own versioned documentation;
HMux tests only hook integration and report ingestion.

## Rollback

Remove only the managed hmux handlers with:

```bash
scripts/install-codex-workflow-hooks.sh --remove
```

The removal also creates a timestamped backup and leaves all unrelated hook
handlers intact, including handlers sharing a matcher group with hmux. Review
the result in `/hooks`. To restore an exact earlier file or skill, compare and
copy the corresponding timestamped backup rather than overwriting newer
personal changes. Removing hooks stops future updates; retained lifecycle
state is pruned on the next ordinary hmux catalog access and is never used to
control Codex.
