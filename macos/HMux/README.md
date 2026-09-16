# HMux for macOS

This is the optional native client. The primary product is the [web/PWA client](../../README.md).

HMux is a personal AI-agent terminal built with SwiftUI/AppKit and an embedded,
checksum-pinned Ghostty frontend. The Go `hmux` bridge owns SSH and tmux access;
the Home Mac owns running sessions, aliases and hidden-session state.

## Build and install

Run from the repository root on an Apple Silicon Mac with Xcode:

```bash
macos/HMux/scripts/build.sh &&
macos/HMux/scripts/package.sh &&
scripts/install-hmux-app.sh --local --system
```

This builds, packages, verifies and installs `/Applications/HMux.app`, then
launches it. Outputs are `macos/HMux/build/HMux.app`,
`HMux-<version>-macOS-arm64.zip` and its `.zip.sha256` sidecar. The sidecar contains
one bare lowercase SHA-256 digest. Packaging verifies the extracted archive.

For a per-user local installation, omit `--system`. Without `--local`, the
installer fetches the matching prepared release from trusted Home and installs
it into `~/Applications`. See [operations](../../docs/OPERATIONS.md) for setup,
requirements and release handling.

The build keeps Ghostty, Zig, SwiftPM caches and DerivedData under this project.
It does not install Homebrew packages, edit Ghostty preferences or create launch
agents. When an execution sandbox blocks Xcode/SwiftPM, run the same command in
an ordinary local terminal. A successful Go build or Swift typecheck alone does
not mean a new app has been installed.

## Workspace

- The searchable session list uses alias-first A–Z ordering with natural numbers.
  All/Active/Attention filter the list; activity changes do not reorder it.
- Tabs occupy the compact native titlebar. Shortcut numbers lead directly into
  session names. The conversation button sits beside the all-tabs chevron,
  immediately before Search; Open and New remain separate actions.
- Tab dimensions and search-field geometry stay fixed across selection/focus.
  Terminal views remain mounted during tab switches; only reconnect replaces
  a surface. Inactive surfaces stop accepting input and pause rendering.
- Closing a shared tab removes it on every connected native/web client and releases
  each temporary grouped tmux view.
  It preserves the original session and other clients. Terminate is a separate,
  confirmed action. A disconnected tab retains its screen and offers Reconnect.
- Alias and hide/restore changes belong to Home and propagate to connected
  clients. Hiding a session leaves its existing visual tabs connected.
- Up to 32 tabs and their order are shared through Home, normally within five
  seconds. Selection and panel visibility stay on each device. A legacy local
  layout seeds Home only when no shared layout has been initialized. The ten
  most recently closed tabs can be reopened during the current run.
- Shared tabs require the current Home agent (`shared-workspace-v1`) and bundled
  helper (`workspace`). Hover the catalog status to see shared-tab sync status.
- The workflow inspector adapts between a split column, side overlay and bottom
  panel. The minimum window size is 960 × 640 pt.
- [Flexoki Dark](../../docs/THEME.md) covers chrome, terminal, sheets and popovers.
  The [macOS icon artwork](Brand/README.md) is maintained alongside the app.

The footer uses equal top/bottom insets. Provider quota and shared Home resource
metrics sit together; directory and attention information use remaining space.

### Conversation reading

Use the reading button beside the all-tabs chevron or **⇧⌘D**. The reader replaces
workspace content without adding a sidebar section or recreating the terminal.
It defaults to assistant prose with code blocks hidden. Display options enable
questions and code; search, selection, per-message copy and Latest support reading.

Each request carries the selected tmux session ID and creation time. Home
resolves that session’s active window and pane, Codex process and unambiguous
main record. A cached runtime label does not gate the request. The catalog and reader share one resolver. Main-session metadata distinguishes
the Codex conversation from open subagent records. HMux does not substitute the
newest project/global record or scrape terminal output. Missing or ambiguous
associations show an explanation; remote Home agents require `conversation-v1`.

Only public user/assistant messages are returned. System instructions, internal
reasoning and execution records are excluded. Reads are bounded to the newest
4 MiB of the record, at most 200 messages and 512 KiB of text. Polling runs every
three seconds while the reader is open and the app is active. Closing it or
switching tabs cancels the previous read. Bodies stay out of the catalog, logs
and disk cache. See [reader troubleshooting](../../docs/TROUBLESHOOTING.md#codex-conversation-reader).

### Claude and Codex usage

The footer shows weekly **1w remaining** quota. Click it or use **⌥⌘U** for
account and window details.

| Provider | Footer value | Account details |
| --- | --- | --- |
| Claude with cswap | Unique active account’s weekly quota | Emails, ACTIVE marker, weekly quota and reset times |
| Claude without a cswap list | Existing signed-in account’s provider quota | Available provider windows |
| Codex with codex-lb | Official capacity-weighted weekly pool quota | codex-lb aliases; `Account N` if no alias exists |
| Codex without codex-lb | Existing signed-in Codex quota | Available provider windows |

HMux reads cswap’s existing roster and schema-v2 usage cache on Home, matching
slot number, email and organization. Active identity uses cswap’s config-path
precedence, including legacy `.config.json` and an absolute `CLAUDE_CONFIG_DIR`.
Reads are throttled to two seconds. Measurements older than five minutes or
with fetch errors are marked Cached; they expire after thirty minutes or their
quota reset. An unknown active account or missing active quota never borrows
another account’s value. Open cswap normally if its cache needs refreshing.
An existing export remains supported through `TOKEN_USAGE_CLAUDE_SWAP_ACCOUNTS`.

The Home collector reads codex-lb’s key from `TOKEN_USAGE_CODEX_LB_API_KEY`,
`CODEX_LB_API_KEY`, or the private `~/.codex/lb-api-key` file (owned by the current
user, mode `0600`). Codex emails are excluded; Claude cswap emails are included
as explicitly requested account labels. Credentials and keys never leave Home.
HMux does not switch accounts, refresh tokens or create a usage daemon.
See [usage troubleshooting](../../docs/TROUBLESHOOTING.md) for authentication,
missing cache and rate-limit behavior.

### Home resource usage

CPU/GPU/RAM describe the single Home Mac running tmux, including when HMux runs
on another Mac. One catalog connection samples Home for all tabs. Click the
metrics control for memory used/total and sample status. Unavailable or stale
measurements are identified; client-Mac load is never substituted. Remote agents
require `host-metrics-v1`; older agents continue serving the catalog.

### Files and keyboard controls

Dropping 1–16 regular files stages them on Home with generic names. HMux inserts
quoted Home paths only if the originating tab, surface, session identity and
focus still match. Otherwise that tab offers Ready to paste. It never sends Enter
or replaces the clipboard. Reconnect cancels transfers bound to the old surface.

The [main README shortcut table](../../README.md#use) lists navigation, search,
conversation, usage and session-management shortcuts. Tab context menus support
reordering, reconnect, alias editing, hide/restore and explicit termination.

## Runtime and maintenance

The catalog uses a foreground `hmux app catalog-stream` helper with an ephemeral,
certificate-pinned WSS endpoint and one-use bearer. Home access uses a persistent,
host-key-checked SSH connection or direct local reads. Semantic changes update
stable row objects; interaction can defer background publication. Older agents
fall back to fifteen-second polling. Connected requires a verified snapshot;
Reconnecting retains the last valid catalog and becomes Offline after 20 seconds.

Each surface runs the fixed `hmux app terminal` command with validated
`{id, created_at}` metadata. A temporary grouped tmux view hides inner status
chrome while sharing the original windows. Shared sizing follows tmux policy.
The bundled `HMuxGhostty.config` isolates app behavior from user Ghostty settings.
General-purpose Ghostty window/tab creation, App Intents and keybindings are
restricted so terminals open through the HMux bridge.

Usage runs through the same foreground local-user/SSH authority with bounded,
leased stdio frames. Closing the app or SSH connection ends its collector.
There is no extra login, fixed usage port, tunnel or launch agent.
See [architecture](../../docs/ARCHITECTURE.md) for protocol and lifecycle details.
The Home catalog connection also saves [reboot recovery checkpoints](../../docs/RECOVERY.md)
and restores tmux/provider sessions before its first post-reboot snapshot.
Tabs follow only verified Home recovery lineage, preserving their order and selection.

Runtime update checks occur at most hourly; native-app checks at most every six
hours while running. Automatic app updates verify signed manifests and bundles,
retain a timestamped rollback copy and offer an explicit Restart action. Automatic
replacement requires a safe user-owned installation directory; a conventional
root-owned `/Applications` installation uses manual installation instead. See
[operations](../../docs/OPERATIONS.md) and [rollback](../../docs/ROLLBACK.md).

## Development and verification

Ghostty’s SurfaceView is not a supported public framework. The build verifies an
immutable source revision, applies the overlay and compiles Ghostty’s AppKit
frontend with HMux. The current private app is ad-hoc signed; public distribution
requires the third-party notice/SBOM, Developer ID and notarization gates.

```bash
make check
make build
macos/HMux/scripts/build.sh
```

`make check` includes Go checks, isolated integration tests and native smoke tests.
The full app build checks SwiftUI/Ghostty integration. Current evidence, skipped
checks and installation status belong in [validation](../../docs/VALIDATION.md),
not in the feature list. Live acceptance must use disposable `hmux-e2e-*` sessions
and must not attach to or mutate pre-existing user sessions.

Debug-only `HMUX_UI_TEST_SESSION_ID`, `HMUX_UI_TEST_SESSION_CREATED_AT` and
`HMUX_UI_TEST_CLOSE_AFTER_MS` can open a disposable cataloged session and close
its visual tab after a bounded delay. Opening still creates a temporary grouped
view. These overrides are absent from the distributable Release build.
