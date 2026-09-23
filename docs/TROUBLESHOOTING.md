# Troubleshooting

## Connection or terminal is unavailable

Check HTTPS/gateway health and that `hmux-web connect` is running on Home. Home
initiates outbound WSS and must use the matching private connector token. Check
bounded diagnostics under Settings before refreshing; never share tokens or
terminal contents in a public report. Transient failures should retry within the
current login; expired/revoked logins must authenticate again.

On Home, use `hmux-agent doctor` and `hmux-agent catalog` for local diagnostics.
A missing tab retains its `{id, created_at}`; do not reconnect by a matching name.
Close/reopen only the browser view if necessary. Never kill original tmux sessions
or reset private state to troubleshoot a transport problem.

## Automatic Home service

Use `hmux-web service status` and the private `<state_dir>/home-service.log` (plus
its single `.1` rotation). Running means the process is supervised; it does not
prove gateway authentication or connectivity. Repeated "Gateway connection
unavailable" can indicate network/TLS failure, a mismatched token or an older
manual connector still occupying the gateway. Do not delete credentials or the
connector lock. Stop the known foreground connector or use verified adoption.

If a CLI works in Terminal but not through the service, stop the service and
reinstall with explicit URL/token/config from that working terminal so PATH and
configured provider paths are captured again. `--from-running` keeps the existing
connector's environment. Version-manager
paths can change when Node is upgraded. The service does not source shell startup
files or copy API keys from the environment; use persistent provider authentication.
An explicit missing Home config is a startup failure, not a fallback to new paths.

On macOS, the user must have logged in to a GUI session and the Mac must be awake.
No Terminal window is required. On Linux, check `systemctl --user status
hmux-home.service` and `loginctl show-user USERNAME -p Linger`; lingering must be
explicitly enabled for startup before login and continuity after logout. Details
and removal commands are in [Operations](OPERATIONS.md#automatic-home-startup-macos-and-linux).

## Settings fail after upgrading

New installations use `home.toml`; existing `client.toml` loads only when it is
absent. Unknown fields, unsafe paths or non-Home roles fail explicitly. Preserve
state/inventory paths and consult [MIGRATION.md](MIGRATION.md). Do not delete account
or session files: doing so changes login continuity and may lose revocation state.

## Usage is unavailable

Collection runs on Home as the existing signed-in CLI user. Check that user's
CLI login and read-only credential accessibility. A CLI account switch is
authoritative on the next collection pass. Do not copy credentials to browser devices, edit provider auth files through HMux or introduce a separate usage daemon.

Codex LB shows the configured codex-lb account pool's weekly (`1w`) remaining
quota. Its detailed account rows use only the aliases assigned in codex-lb;
missing aliases show `Account N`, never an email fallback. Per-account detail
comes from the existing Home `codex-lb-accounts.json` export; missing or expired
rows are unavailable rather than invented. An unavailable configured codex-lb
source remains visibly unavailable instead of switching to one OAuth account.

Web source selection is explicit in Settings → 사용량 표시. The cswap web source
uses the installed `cswap list --json`; inspect `usageStatus` and `usageFetchedAt`
there when an account is unavailable. A keychain error is not zero usage. The
CLI sources query the current signed-in provider account independently.

Codex account plan badges require the existing export producer to preserve the
allowlisted upstream `/api/accounts` field `planType` as `planType` (also accepted:
`plan_type` or `plan`). Older exporters may omit it even when codex-lb knows the
plan. Preserve that field in the producer; never infer Pro/Plus from quota size.
The relevant producer mapping is, for example:

```python
"planType": acct.get("planType") if acct.get("planType") in {
    "free", "plus", "pro", "team", "business", "enterprise", "edu", "go"
} else None,
```

Weekly reset countdowns require `resetAtSecondary` in account exports or
`resets_at` in normalized provider quota. Omit missing `fiveHourPct` instead of
filling it with zero; the web UI then hides that absent window. Existing export
schedules remain operator-managed; HMux does not install a background job.

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

## Upload is interrupted

Return to the original tab and retry. Tab/account changes, cancellation and lost
connections invalidate the transfer; an upload never silently moves to another
session. File and total-byte limits are enforced before committing. Files expire
three hours after completion. An uploaded path is pasted with quoting, without Enter.

## Host metrics are unavailable

Metrics describe Home, not the browser or gateway. Unsupported CPU/GPU observations
are omitted; a missing value is not zero load. Check connector health and host/browser
clock agreement. Platform collectors remain host-specific.

## Codex conversations are unavailable

Home needs one exact provider binding for the active pane. Ambiguous, missing or
changing rollout files are unavailable rather than guessed. The reader returns only
a bounded tail of complete records, excluding tools and internal context. Markdown
rendering does not execute raw HTML. See [WEB.md](WEB.md#conversation-reader).

## Reboot recovery

The Home connector must have saved a checkpoint before reboot and must be restarted
afterwards. Before planned maintenance use `hmux-agent recovery save`. For errors,
run `hmux-agent recovery sync` locally and check directories/provider installations.
Do not delete checkpoints as a first repair step. Provider authentication/project
trust prompts still apply. See [RECOVERY.md](RECOVERY.md).

## Workflow metadata

Install/review the optional Home hooks in [CODEX_WORKFLOWS.md](CODEX_WORKFLOWS.md).
`hmux-agent workflow --json` reads bounded metadata without terminal scraping.
Unavailable hook data must not block Codex or terminal access.

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
[Validation](VALIDATION.md#browser-and-device-evidence) for confirmed results.

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
