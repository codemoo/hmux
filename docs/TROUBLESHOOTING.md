# Troubleshooting

## Returning to work

- A disconnected tab keeps its previous screen. Use **Reconnect** (`⇧⌘R`) to
  replace that connection; a missing/replaced Home session cannot be reconnected.
- Reopen an accidentally closed tab with `⇧⌘T`. This only works while the same
  session still exists, within the current app run.
- App launch restores the last tab layout only after a successful catalog for
  the same configured Home source. Changed SSH/client settings, expired sessions
  or rejected local state can prevent restoration. After a Home reboot, the
  updated Home agent first restores its checkpoint; tabs can follow only verified
  recovery lineage. Same-boot deletions and same-name unrelated sessions are not
  substitutes. See [Home recovery](RECOVERY.md).
- If connection settings change while the app is open, reopen HMux before
  opening or changing sessions. Existing terminal views remain available.
- If **Create and Open** succeeds on Home but opening fails, **Retry Opening**
  targets that same session. Cancel leaves it in the Home session list.

Use the embedded helper explicitly so a personal shell function cannot
intercept maintenance commands:

```bash
HMUX_HELPER="$HOME/Applications/HMux.app/Contents/Helpers/hmux"
"$HMUX_HELPER" --no-update-check version
"$HMUX_HELPER" --no-update-check doctor --json
```

## App is Connecting, Reconnecting or Offline

Connected requires a valid first snapshot. A failed connection retains the last
catalog with a Reconnecting indicator briefly, then shows Offline. Click the
Offline footer to read the full error and retry; an empty workspace also offers
Try Again. On Home check `hmux-agent doctor` and
`hmux-agent capabilities`; on a remote Mac check effective SSH configuration:

```bash
ssh -G hmux-home
ssh -o BatchMode=yes hmux-dmz -- true
ssh -o BatchMode=yes hmux-home -- true
```

Verify ProxyJump, client-local identities, host-key trust and forwarding off.
Do not bypass host-key verification. Old agents can trigger compatibility
polling; install a compatible agent before diagnosing WSS as a UI problem.

## Flicker while switching tabs or panels

Current native tabs keep their terminal wrappers mounted between selections.
Only reconnect replaces a renderer; panel and banner transitions do not animate
terminal geometry. If flicker remains, record whether it happens on tab selection,
sidebar/details changes, or opening a search/management sheet, plus the app version
and display scaling. The isolated surface-deck test verifies view retention; it
cannot prove real Metal/compositor behavior on every display.

## List is empty or a session is hidden

An empty connected catalog is valid. If Active or Attention is selected, use
Show All Sessions to clear the filter. Search with ⇧⌘F even when the sidebar is
hidden; Return opens the first result and Escape returns to the terminal. Hidden sessions remain running; open the
hidden-session manager to restore their visibility. Creation requires a valid
profile in the private inventory and an existing Home working directory.

If a catalog is rejected, investigate malformed metadata or mismatched
protocols. Do not weaken bounds or rename an unrelated live session to satisfy
a test.

## Terminal tab is disconnected

Catalog connectivity and the terminal SSH/PTY connection have separate
lifetimes. A live catalog does not prove an old terminal surface is still
connected. Use Reconnect (⇧⌘R) on the affected tab after restoring SSH. This preserves
Home work; it is not a session restart. If the session identity changed, select
the new session explicitly.

Native grouped tabs share windows and do not hand off all other clients.
Window size follows tmux's policy for shared windows. Direct CLI/mobile
handoff applies to its target session; do not assume it covers grouped siblings.

## Workflow badge is absent or stale

Hooks are prospective and do not reconstruct old turns from pane content.
Check matching Home agent capabilities, hook JSON and the required Codex trust
review in [Codex workflows](CODEX_WORKFLOWS.md).

`stale` means no lifecycle update for two hours, not permission to kill or
restart work. Attention also includes input/approval waits, failures and
interruptions. Inspect the workflow and actual agent UI. HMux never answers
agent approval prompts.

## Session attachment or metadata looks inconsistent

The catalog counts clients attached to every member of a tmux session group,
including HMux's hidden terminal views. A client attached to a native view must
therefore count as attached on the visible original session. Work/activity is
separate from attachment; the A–Z list does not move rows between state sections.

Aliases and Hide/Restore are Home-owned changes. The app projects a requested
change immediately and confirms it with a fresh catalog. Older stream or deferred
snapshots must not undo that confirmation. A failed write rolls back the local
projection and reports an error. Hidden sessions remain running and can be
restored through Hidden Sessions; hiding an open session leaves its tab intact.
The alias editor closes when Home acknowledges a successful write. Background
catalog confirmation may continue afterward; it must not keep the editor on
`Saving…`. A failed write keeps the editor open with a retryable error.

## Usage is unavailable

Collection runs on Home as the existing signed-in CLI user. Check that user's
CLI login and read-only credential accessibility. A CLI account switch is
authoritative on the next collection pass. Do not copy credentials to remote
Macs, edit provider auth files through HMux or introduce a separate usage daemon.

Codex LB shows the configured codex-lb account pool's weekly (`1w`) remaining
quota. Its detailed account rows use only the aliases assigned in codex-lb;
missing aliases show `Account N`, never an email fallback. Per-account detail
comes from the existing Home `codex-lb-accounts.json` export; missing or expired
rows are unavailable rather than invented. An unavailable configured codex-lb
source remains visibly unavailable instead of switching to one OAuth account.

Claude first reads existing cswap metadata (`~/.claude-swap-backup/sequence.json`),
its schema-v2 usage cache (`cache/usage.json`), and the active email/organization
from the Claude config file, using cswap’s legacy `.config.json` precedence and
absolute `CLAUDE_CONFIG_DIR` override (otherwise `~/.claude.json`). HMux never runs
cswap account switching or token refresh.
The footer follows the unique active identity; account details show emails and an
ACTIVE badge. Cached measurements older than five minutes or with a fetch error
are marked stale, and expire after thirty minutes or their quota reset. If the
cache is empty, open cswap normally to let it collect usage. HMux does not create
a refresh daemon. An existing `claude-swap-accounts.json` export remains a fallback;
`TOKEN_USAGE_CLAUDE_SWAP_ACCOUNTS` explicitly selects an export instead.

Without cswap data, Claude reads the Home user's existing `~/.claude/.credentials.json`
OAuth login file. Sign in through Claude Code on Home if no login exists; HMux
does not log in or refresh tokens itself. A Keychain-only login is not currently
supported by this collector. Do not manually export or copy tokens to work
around it. `No credentials` and `Auth failed` require checking the CLI login;
`Rate limited` does not by itself require signing in again.

Claude's `Rate limited` status means its usage API returned HTTP 429. HMux waits
for the provider's bounded Retry-After interval before trying again. Usage
details show `Retry available after` with a localized time when a deadline is available.
Collection normally retries on the next 60-second tick after that time. Existing
recent quota may remain visible with a stale marker. A missing window is shown
as unavailable, not as 100% left; a cswap list uses only its unique active account. If that account has no quota,
HMux shows it as unavailable rather than showing another account’s quota.

## File drop is waiting

Ready to paste is expected if selection, surface or active application changed
during transfer. Return to the originating tab and explicitly insert the paths.
Only regular files within the documented size/count limits are accepted.
Reconnect/close cancels the old surface's transfer; drop again if necessary.

## Native installation or update fails

The shell installer requires the exact VERSION archive plus SHA-256 sidecar at
the documented trusted Home source path. Building locally does not publish it.
Normal app updates instead require a valid role-bound signed DMZ manifest.

Verify size, hash, platform, public-key identity and safe bundle ownership.
Do not change the pinned key to suppress a signature error. Use a
current-user-owned install directory such as `~/Applications`.
See [Rollback](ROLLBACK.md) before changing a failed installation.

## CLI frame, font or keys differ from the app

The native app uses its bundled Ghostty configuration and SwiftUI shortcuts.
Standalone Ghostty frames and fzf use separate keys and managed fragments.
Follow [CLI compatibility](CLI_COMPATIBILITY.md); do not install keys or UI
options into the user's real tmux server.

## Provisioning or Termius is incomplete

New device keys require public-fingerprint authorization and a second
provisioning run. See [external provisioning](../scripts/EXTERNAL_PROVISIONING_README.md).
Private keys never enter the shared bundle.

CSV generation and opening Termius do not import hosts or prove Vault sync.
Use the supported UI and report only the attained
[validation level](TERMIUS_INTEGRATION.md#validation-levels).

## Home CPU/GPU/RAM shows —

Click the Home metrics control beside Claude/Codex for its current reason.
These values come from the Mac running tmux, regardless of which tab is open.
A remote Home agent must advertise `host-metrics-v1`; older agents and catalog
polling keep sessions usable but omit metrics. For Home-role apps, the bundled
helper supplies the new collector. A missing GPU value means macOS did not
provide utilization; it does not mean zero load. Stale/offline samples are hidden
after the freshness checks. Clock mismatch requires checking the two Macs’ clocks.

## Codex conversation reader

Use the reading button beside the all-tabs chevron, or ⇧⌘D, to switch between
the terminal and conversation. Answers are shown by default; the display menu
can include questions and code blocks. Search operates on the displayed text.
Copy copies that text, and Latest moves to the last visible message without
forcing scroll while you read. Returning to the terminal keeps its existing
connection and screen.

The tmux session’s active window/pane must lead to a running Codex process with
one unambiguous open main conversation record. Open subagent records are excluded
using their session metadata. Custom Codex roots are recognized from validated
open rollout paths. Tabs continue to reference tmux sessions; provider association
is shared with the catalog rather than maintained separately for each tab. The reader does not search other
projects for a plausible conversation or scrape the terminal. Multiple or
missing matches display an explanation. Remote agents need `conversation-v1`.
Only the bounded newest part of large records is shown; incomplete final
records appear after Codex finishes writing them. Internal reasoning and
tool execution records are excluded.

## Home reboot recovery

Update both the native app and the Home agent for checkpoint recovery. The app
must have connected before the reboot to create a checkpoint. Save a fresh one
with `~/.local/bin/hmux-agent recovery save` on Home before a planned reboot.

If Home recovery cannot complete, run `~/.local/bin/hmux-agent recovery sync` on
Home to get the local error. Check that saved directories, provider binaries and
provider configuration still exist. A session name already present is skipped;
it is not treated as a recovered tab. Do not delete the private recovery state
as a first troubleshooting step, since it holds the previous checkpoint.

A resumed provider may still ask for authentication or project trust. HMux does
not bypass either prompt or replay commands/tools that were running before the
reboot. See [recovery behavior and limitations](RECOVERY.md).

## Web/PWA installation and terminal colors

Android Chrome is the verified path for terminal colors and PWA installation.
Refresh HMux, then use Chrome's menu → Add to Home Screen → Install, or the
HMux login/settings install button. Use a normal Chrome tab rather than incognito
or an embedded browser. A home-screen shortcut alone does not verify PWA installation.
iOS installation uses Safari's Share → Add to Home Screen flow.

If Chrome does not offer installation, check HTTPS, the linked manifest, its
192/512px icons and the service worker. Inspect the document's Content Security
Policy: `manifest-src 'self'` is required alongside `default-src 'none'`; keep
`worker-src 'self'` explicit. HTTP 200 for the manifest alone does not prove the
browser can use it. Do not weaken the policy to wildcard sources.

Samsung Internet's home-screen app has a reported digit/symbol background issue
that does not occur in Chrome. HMux already provides its own dark palette and
opts out of browser recoloring where supported. Compare with Samsung Internet's
website dark-mode setting disabled, or use Chrome. Do not change the shared ANSI
palette or Android input/viewport behavior merely to compensate for an unverified
browser-specific effect. Samsung Internet acceptance remains pending.

See [Web HMux](WEB.md) for implementation/operations and
[Validation](VALIDATION.md#current-web-verification-2026-09-09) for confirmed results.

## Mobile input and native clipboard

After a frontend update, reload at a convenient stopping point; HMux does not
force-reload an active terminal. A browser view reconnects to the existing tmux
session. Do not clear credentials/site storage as the first troubleshooting step.

- iPhone: type directly in the terminal. Native Korean edits remain visible until
  a boundary such as space/Enter commits them. Long press near the input caret for
  the OS Paste menu. Its slightly slow first appearance was accepted; its cause
  was not measured.
- iPhone output selection: selecting text dismisses the keyboard. This is the
  accepted limitation, not evidence that the input fix has regressed.
- Android Chrome PWA: open the keyboard, then long press the current input row.
  The full-row target received initial positive feedback. A touch on output outside
  that row is text selection, not necessarily an editable Paste gesture. The older
  cursor-sized target was reported ineffective.

If a regression recurs, distinguish failure to show the menu from failure to insert
text after choosing Paste. Record platform/browser/PWA mode, keyboard state, tab
switch history and the touched row. Do not apply iOS Hangul interception to Android
or change viewport math based only on a clipboard symptom. Native gesture ownership
is shared by both platforms in `web/src/native-clipboard.ts`.

For an iPhone Hangul regression, use `/input-diagnostic.html` with test text only,
then “기록 표시·복사”. It observes the iOS input bridge and stock xterm on other
platforms; it does not reproduce Android's app-specific Paste geometry. A Blob URL
is not a server file. Reports remain local and must not contain credentials or be
committed. See [iOS input details](IOS_INPUT.md) and [current web behavior](WEB.md).
