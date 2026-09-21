# Architecture

## Components and ownership

```text
Browser / PWA -> HTTPS/WSS gateway <- outbound WSS <- Home connector
                                                     -> tmux
                                                     -> Codex / Claude / shell
                                                     -> private metadata / usage

Optional macOS UI -> Go bridge -> local Home calls or authenticated SSH to Home
Legacy standalone terminal UI -> archive/terminal/
```

The web client is the primary interface. Go owns execution, authentication and
validated host operations; Home owns long-running sessions, files and provider
authentication. The HTTPS gateway is trusted with terminal authority. Its role is
different from the optional SSH DMZ, which is a TCP jump/control plane and holds
no Home authentication private key. Swift/Ghostty remains an optional client.

| Layer | Source |
| --- | --- |
| Primary web UI / gateway | `web/`, `internal/webgateway/`, `cmd/hmux-web/` |
| Native UI and bridge clients | `macos/HMux/Overlay/Sources/HMux/` |
| App request dispatch and streams | `cmd/hmux/app*.go`, `cmd/hmux/usage_stream.go` |
| Home command dispatch | `cmd/hmux-agent/` |
| Session metadata and runtime detection | `internal/catalog/`, `internal/agent/` |
| Transport and bounded stream bridge | `internal/client/`, `internal/catalogstream/` |
| Private metadata | `internal/sessionstate/`, `internal/workflow/` |
| File staging | `internal/filestage/` |
| Signed release lifecycle | `internal/release/`, `internal/control/` |
| Go/fzf compatibility UI | `archive/terminal/ui/`, `archive/terminal/frame/`; shared tab storage in `internal/tabstate/` |

## Session identity and lifecycle

Native operations use tmux `{id, created_at}`, never display names, as the
session identity. Home repeats expected-identity checks for sensitive actions.
A recycled stable ID must not inherit an earlier tab's authority.

Each native visual tab owns one Ghostty surface running the fixed
`hmux app terminal` command. Go creates a randomly named temporary grouped
tmux view sharing the original session's windows. The temporary view carries
its own status setting and is excluded from the catalog. Original session
options, keys and metadata remain unchanged.

Native views are shared connections. `attach-session -d` on a new grouped
sibling cannot hand off clients attached to the original session; it must not
be described as doing so. Other grouped/native/mobile clients can affect shared
window sizing according to tmux policy.

Closing or reconnecting a visual tab releases its local surface/PTY and
cleans up only its temporary view. The original session remains running.
An unsolicited terminal-close callback retains the visual tab and old surface
for inspection and explicit reconnect; it cancels any transfer on that surface.
Only explicit tab closure or confirmed termination removes the visual tab.
Termination is a separate expected-identity operation with explicit UI
confirmation. Hiding changes Home metadata and leaves open tabs connected.

Native creation requires the Home agent's `structured-create-v1` capability.
Home returns the ID, creation time and reuse status directly; a new identity
comes from tmux's creation response. Reusing an existing name preserves its
profile metadata. Blank names generate a fresh random suffix and always request
a new session; only explicitly entered names can reuse existing work. The
client does not infer the result by comparing catalogs.
If creation succeeds but catalog lookup/open fails, the sheet retries opening
the returned identity without creating another session. Cancel leaves the
created Home session available in the catalog.

The direct CLI/mobile attach defaults to `attach-session -d` on the original
session; explicit `--shared` omits detachment. The compatibility CLI frame
always uses shared attach. These are distinct interfaces; see
[CLI compatibility](CLI_COMPATIBILITY.md).

## Workspace continuity

`internal/sharedworkspace` owns the single Home workspace for native and web
clients: up to 32 exact `{id, created_at}` references and their order. The private
`state_dir/shared-workspace/workspace.json` uses a file lock and atomic writes.
Clients submit a base and new layout with a revision and operation ID. Home merges
only explicit opens/closes/reorders, preserves unseen concurrent changes, and
retains a bounded replay history. Semantic conflicts return the current snapshot
with `conflict: "workspace_conflict"`; clients show a warning and adopt Home state.
Only uncertain transport failures retry the same operation. Selection stays local
and does not create shared writes. Native and web poll at five-second intervals
and submit edits promptly. No terminal contents, names, paths or credentials are
stored in this workspace.

Native uses `hmux app workspace` and the source-bound Home `hmux-agent workspace`
bridge; the web connector calls the same Go implementation. Missing references
remain in the shared workspace. Recovery rebases them at every completed boot
checkpoint, even without a client sync between two reboots. Exact verified
lineage is required; a matching name or recycled numeric ID is insufficient.

`HMuxWorkspaceState.swift` retains a bounded local snapshot for selection, panel
visibility and one-time migration when Home is uninitialized. An initialized
empty Home layout is authoritative. A closed tab on one device closes on other
clients without killing the original tmux session. A background remote selection
never changes the current device's input focus.

The local Go helper hashes domain-separated, length-delimited client settings
and local hostname/UID for Home mode, or bounded effective `ssh -G` output for
remote mode. This key travels in the local stream bootstrap or polling app
envelope. It identifies configured routing, not a durable remote-machine instance.
A remote `Match exec` may run user-configured local commands while resolving SSH
settings. Terminal, mutation and workspace helpers require the expected scope.
The interval between resolving SSH settings and starting SSH remains a
configuration-change race; host-key checking still applies.

`HMuxSurfaceDeck` keeps each open terminal's SwiftUI/AppKit wrapper mounted
inside a retained native hosting view. Selection sets AppKit `isHidden` on
inactive hosts and routes hit testing only to the selected host; transparent
SwiftUI layers alone cannot isolate native event monitors. Ghostty's local
mouse monitor verifies the clicked target through the visible window hierarchy,
and Command key-up is delivered only to the actual visible first responder.
Reconnection replaces only that surface's UUID. Chrome transitions do not animate terminal geometry. The
Store pauses rendering for inactive tabs and forwards window occlusion changes
to Ghostty, then requests a refresh for the selected visible surface. This avoids
recreating the native scroll wrapper and reparenting its Metal view on every switch.
The terminal canvas also observes Ghostty's theme and dynamic background color,
so its backing layer matches the terminal while the next frame is prepared.

Connection/file notices occupy a structural stack above the terminal. The
inspector uses a side split on wide workspaces, a side overlay only when 500
points of terminal remain visible, and a bounded bottom overlay below that width.

Tab boxes, label weight and badge slots have fixed geometry across selection
and activity changes. Selecting a visible tab does not recenter the strip.
Search editors keep an intrinsic width independent of focus and query length;
search focus cancels delayed terminal focus requests.

Tab activation uses bounded recent history. Closing the active tab selects the
last-used survivor; undo-close retains ten identities/positions for the current
run and rechecks them against the current catalog. It never resurrects a killed
tmux session. Quick-switch search preserves its query and selection on open
failure. A cancellable focus request checks selection, app/window activity and
modal state before focusing; delayed focus cannot target an earlier tab.

## Catalog and UI publication

The catalog uses protocol v1 and bounded JSON. Home obtains session/window
metadata in batched tmux calls and one bounded process snapshot. It excludes
pane contents and full argv. Runtime/model/lifecycle readers inspect only
allowlisted metadata; unavailable evidence degrades to unknown state.

Aliases and hidden-state entries are keyed by stable identity in Home's
private state directory. Connected clients receive semantic changes from
that source; unrelated Home state roots are not cloud-merged.

The app starts one foreground `hmux app catalog-stream` helper. Swift reads
a bounded bootstrap, pins the exact ephemeral TLS leaf SHA-256 and sends a
one-use bearer token in the Authorization header to an IPv4 loopback WSS
endpoint. No fixed port or public listener is introduced.

The helper consumes a direct Home producer or one persistent SSH process
running `hmux-agent catalog-stream --stdio`. Frames are length-prefixed,
bounded at 32 MiB and strictly sequenced. Catalog-free heartbeats maintain a
50-second source lease. Unchanged catalogs are suppressed; `generated_at`
does not trigger a semantic change.

Swift shows Connected only after a valid snapshot. It preserves the last
catalog with an amber Reconnecting indicator through a 20-second grace, then
shows Offline. An explicit Retry restarts only the catalog transport, invalidating
old stream/poll generations while preserving terminal tabs and pending work. Unsupported
agents or repeated stream failures enter quiet 15-second compatibility polling.
Stable row objects and interaction leases prevent background membership,
ordering or publication changes from stealing input. Poll results cannot
overwrite newer streaming data.

The sidebar, quick switcher and hidden-session manager share natural A–Z
ordering by alias when present, otherwise by session name. Activity and
attachment updates do not move rows into sections. Attachment counts include
the original session's grouped tmux views, including the native app's hidden
view, so an attached native terminal remains visibly attached.

Alias and visibility writes publish immediately on the initiating client.
Per-identity mutation fences retain that value while Home acknowledges the
write and a direct catalog read confirms it. The same nanosecond Home catalog
watermark covers streaming, polling and direct confirmation; older arrivals
cannot overwrite the latest accepted projection. Input deferral keeps only
the newest catalog. A failed write restores the prior value, while confirmed
newer edits from another client remain authoritative. Failed confirmation reads
retry with exponential intervals capped at 30 seconds until the mutation is
confirmed, superseded or its Store lifecycle ends; timeout never reveals an
old pre-write value. The alias editor's opening snapshot stays immutable
while saving; busy state comes from the Store's pending write, and successful
acknowledgment dismisses that sheet independently of catalog confirmation.

## Workflow and usage

Optional Codex lifecycle hooks and detached-task reports project bounded
state onto the owning tmux identity. Provider IDs are one-way hashed before
persistence. Prompts, responses, transcript paths, pane contents, tool inputs
and tool results are excluded. State has count/size/retention limits and
failure cannot steer or block a Codex task. See [Codex workflows](CODEX_WORKFLOWS.md).

Usage is collected on Home through the embedded Token Terrier stream module.
A remote app uses `hmux-agent usage-stream --stdio` over existing SSH.
Direct CLI credential reads remain read-only Home inputs. The owner-requested codex-lb
account alias is transmitted as bounded, sanitized `display_name`. A missing
alias becomes `Account N`; email and other display-name fields are never used as
fallbacks. Claude cswap emails are explicitly authorized, bounded account labels.
Codex emails, account IDs and hostnames remain excluded from remote serialization.
No separate usage daemon, tunnel, login or credential transfer is required.

The footer reports weekly (`1w`) remaining quota. Codex prefers codex-lb’s
capacity-weighted `account_pool_usage.secondary`, scoped to the configured API
key’s account pool, rather than averaging per-account percentages or applying
a depleted 5h API-key allowance to the weekly headline. Window observation flags
keep missing windows distinct from a real 100% remaining value. Pool windows
never borrow API-key allowance values or reset times. Claude uses the unique
active cswap account’s weekly quota when an account list is available. Missing quota or ambiguous active flags never fall back to a
different account. Without a cswap list, the ordinary signed-in provider quota
remains the fallback. Account rows show every available
name and weekly window, including unavailable/limited status and reset dates.
HTTP 429 is classified as rate limited. The collector honors Retry-After
seconds or HTTP dates, bounded to 24 hours, with a five-minute fallback.
A recent successful value may remain visible as stale until its sticky TTL
expires; fetches remain suspended for the provider-requested interval.
Suspension is scoped to the account that received the rate limit and clears
after a successful upstream fetch. The optional `status.retry_at` timestamp
lets the usage detail show a localized retry-eligibility time, including while
last-good quota is stale. Automatic collection normally retries on the next
60-second tick after that deadline.

Legacy/native Claude cswap integration reads the existing roster, schema-v2 usage cache and
Claude active-account metadata on Home. Cached quota is joined by slot number
and email/organization identity, with roster/config rechecks across the read.
This legacy cache reader adds no cswap process, credential refresh or file writer.
Reads are throttled to two seconds and use cswap’s config-path precedence.
Freshness is per account (5 min warning, 30 min expiry; passed reset is unknown).
The optional existing JSON export remains supported; explicit export configuration
selects that source. Claude's unrelated OAuth fetch status cannot override a valid
active cswap cache reading.

The web connector opts into `RunWithSources` (remote: advertised
`usage-sources-v1`, fixed `usage-stream --stdio --sources`). Each provider frame
carries its allowlisted CLI and cswap/codex-lb snapshots separately. Legacy
`Run` consumers keep their original frames. The explicit web source selection
never falls back from a pool to CLI, or from unavailable CLI to a pool.

The web cswap source invokes the installed `cswap list --json` with a deadline,
bounded output and discarded raw stderr. It reuses cswap's shared scheduler and
structured usage/freshness statuses instead of guessing from stale cache files.
cswap may maintain its own existing cache/credentials; HMux does not directly
write those credentials or invoke account switching/login/service commands.
No additional daemon or launch agent is introduced. Account-scoped web settings
control visibility/source selection, while collection stays shared on Home.

## Native workspace chrome

Tabs occupy the native compact window toolbar in place of the HMux title. The
all-tabs menu sits immediately before Search; tab numbers lead directly into
session names. Footer controls are 22 pt high with equal 5 pt vertical insets.

The conversation control and all-tabs chevron share one toolbar item with a
2 pt gap, before Search. Search focus and tab selection preserve layout size.

## Agent session binding

`internal/catalog/session_binding.go` owns provider-session association for catalog metadata,
explicit conversation reads and Home recovery checkpoints. Tabs retain their existing tmux
session references. The resolver starts with the original tmux session’s active
window/pane, then selects the unique foreground Codex/Claude process
across independent process branches (stopping before nested agents). It does not
store provider identity per visual tab or follow an unrelated app-view selection.

Codex records come only from that process’s open descriptors. The bounded first
`session_meta` record identifies the unique main session; object-valued subagent
sources are excluded. Filename order, mtime and cwd never choose a session. The
header identity must agree with the filename. A custom Codex sessions root can be
inferred from a validated open rollout descriptor. Multiple main records remain
ambiguous. A single non-provider wrapper chain is an optional descriptor owner.

Claude uses an exact live provider PID registry under the default config or a
bounded cswap profile root, then a unique matching session-ID transcript. Versioned
Claude executable paths are recognized. Duplicate registries/transcripts remain
ambiguous; no newest-file fallback is used. Missing event metadata stays unknown
rather than borrowing a global default model.

All consumers use the same bounded, owner-checked, non-symlink record opener.
Recovery stores exact provider resume IDs and config roots in private Home state;
they never enter the client catalog. Each checkpoint resolves bindings afresh and
compares two observations, so provider replacement or ambiguity cannot silently
retain an older conversation ID. See [Home recovery](RECOVERY.md).

## Explicit conversation reading

The reading control beside the all-tabs chevron switches the workspace to a
text reader; it adds nothing to the sidebar. Terminal wrappers remain mounted with no selected
surface while reading. Terminal focus and rendering are suspended, then restored
when returning. Switching tabs cancels the previous read and discards its body.

`hmux app conversation` is an explicit, source-bound request with the exact
session ID and creation time. Home uses the shared agent-session resolver for the current Codex process and
main rollout file. It never accepts a caller-supplied path or guesses from the
latest file in a project. Ambiguous or missing associations are unavailable.
Remote clients require `conversation-v1` and use existing SSH host trust.
Cached runtime labels do not gate the request; Home resolves the selected
session’s active pane.

Only user-visible user/assistant text is returned in bounded responses; system
instructions, reasoning, tool calls/results and terminal contents are excluded.
The reader defaults to answers and hides code blocks, with optional questions,
code, search and copy controls. Body reads occur only while the reader is open.
Bodies do not enter the catalog, workflow, diagnostic logs or disk cache.
The owner explicitly authorized this exception to metadata-only collection.

## Home host metrics

CPU, GPU and RAM beside provider usage describe the single Home Mac running
all tmux sessions. `internal/hostmetrics` attaches one optional `host_metrics`
observation to each catalog-stream sampling cycle (normally five seconds).
One collector belongs to the foreground connection; opening or selecting tabs
does not create collectors. Home mode samples directly in the bundled helper.
Remote mode negotiates `host-metrics-v1`, then adds `--host-metrics` to the
existing SSH catalog stream. Legacy `catalog` and unflagged streams omit the
object so older strict decoders remain compatible. Compatibility polling has
no host metrics. No local-client fallback, daemon or extra polling connection
is used.

Fixed macOS commands run concurrently under a three-second deadline with
bounded output. CPU uses the second `top` interval sample. RAM is resident app
(anonymous minus purgeable), wired and compressor memory; file cache is excluded.
GPU uses the maximum reported accelerator utilization, when macOS exposes it.
Unsupported or failed measurements are omitted, including unavailable GPU;
a real zero remains distinguishable from missing data. No elevation is needed.

`HMuxHostMetricsStore` receives observations only after the catalog's source and
ordering checks. Its timer only ages data; it never samples the local machine.
Values older than 20 seconds, offline observations and uncertain clock values
are hidden. Invalid optional telemetry is dropped without rejecting session data;
clock corrections can recover on the next accepted catalog. The compact footer shows percentages; the Home popover provides
memory used/total in GiB, observation age and availability explanations.

## File staging

The bundled helper accepts 1–16 regular non-symlink files, up to 32 MiB per file
and 128 MiB per request. Original names and local paths stop at the client
helper; Home receives opaque bytes and generic names.

The private Home spool validates identity before reading and before commit,
hashes bytes, serializes quota accounting and atomically commits files.
It is bounded to 512 MiB/100 stages. Completed stages expire after 24 hours,
interrupted stages after 10 minutes, opportunistically on the next request.

Swift revalidates the response and binds it to the originating tab, surface
UUID, session identity, selection epoch and key window. A changed selection
leaves Ready to paste on the originating tab. Insertion is POSIX-quoted text
without Return and never uses tmux send-keys or the clipboard.

## Inventory and releases

Private TOML inventory is validated before rendering. Mutations snapshot
history and preserve last-good outputs on failure. Generated SSH configuration
uses a managed include and never puts RemoteCommand on the catalog alias.
SSH uses normal host-key checking, explicit forwarding prohibition and
client-local identities.

Signed DMZ manifests bind version, platform role, artifact, size, SHA-256,
publish time and minimum protocol. Client, agent and native app are separate
roles. Cache selection and app swaps are atomic and retain rollback copies.
Native update checks are app-owned tasks; restart is explicit.

The shell native installer is a separate trusted-Home bootstrap path using
SCP plus a checksum and bundle validation. It must not be described as a
DMZ-signed-manifest installer. Details and preconditions are in
[operations](OPERATIONS.md) and [threat model](THREAT_MODEL.md).

## Termius boundary

One Home Sessions object uses the DMZ jump and
`exec ~/.local/bin/hmux-agent select --mobile`. Session discovery is live and
does not create a Termius host per tmux session. Host reconciliation produces
an import CSV; supported UI import and Vault sync are separate user actions.
No private Termius storage access or automatic deletion is implemented.

### Web disk capacity

The Home web connector uses `hostmetrics.DiskUsage` to enrich its one shared
catalog snapshot with `disk_used_bytes` and `disk_total_bytes`. Darwin statfs
reads the startup APFS data volume/container, with root-volume fallback. This
enrichment is web-only so older strict native `host-metrics-v1` consumers do not
receive extra fields. The Linux gateway never samples its own disk for this UI.
