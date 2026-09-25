# Web HMux

This is the current web/PWA reference. Historical experiments are archived and
are not implementation instructions. HMux Web uses TypeScript, Vite and xterm.js
with a Rust gateway and one outbound Home connector. Web/PWA is the only HMux UI.

## Runtime and security

Browser → HTTPS/WSS → Linux Nginx → loopback Rust gateway. Home → authenticated
outbound WSS → gateway. All terminal/catalog/provider operations originate on Home.
One shared catalog/usage collector serves every web client; computer resources
always describe Home, never the phone or Linux gateway.

Nginx terminates public TLS; the gateway owns authentication and terminal authority.
It is not an end-to-end encrypted SSH relay. An authenticated terminal permits an
interactive shell. Additional account profiles isolate tab layouts, not files,
provider credentials, aliases, hidden metadata or shell permissions.

- Password: salted PBKDF2-HMAC-SHA256, 600,000 iterations. TOTP: SHA1, six digits,
  30 seconds, ±1 step; persist replay counters before issuing sessions when enabled.
  TOTP is enabled by default and can be disabled per account in Settings → Account
  security after password plus a fresh existing authenticator-code check. Disabled
  accounts log in with a password only. Password verification precedes the optional
  TOTP challenge; a challenge creates no cookie or authenticated session.
- No public signup, reset, recovery-code or authenticator enrollment API.
- Secure/HttpOnly/SameSite=Strict `__Host-hmux` cookie; no browser-stored secrets.
- Eight logins per account, seven-day absolute expiry from login with no idle expiry. A ninth
  login evicts that account's least recently used login. Passive polling/output
  does not extend the seven-day deadline. Sessions persist in a private atomic
  `<credentials-path>.sessions` file and survive gateway restart with the same
  deadline. Only cookie-token hashes and independent display IDs are stored.
  Revocation is persisted before success; logout closes that login's connections.
  Session-store failure rejects authenticated access until repaired/restarted.
- Exact Host/origin checks, CSRF mutations, WebSocket Origin checks, no CORS.
  Loopback proxy traffic alone may assert Nginx-overwritten X-Real-IP.
- Five login attempts/minute/source, two concurrent password hashes, eight total
  terminal views, eight concurrent Home operations and sixteen pending requests.
  Wire frames are bounded at 4 MiB; terminal output chunks at 32 KiB.
- Credential files: private owner-controlled files, mode 0600; directories 0700.
  Back up configuration with timestamps. Never place real credentials in the repo.
- CSP allows only same-origin manifests and workers (`manifest-src 'self';
  worker-src 'self'`) while retaining `default-src 'none'`. A manifest returning
  HTTP 200 is insufficient if the page policy blocks it.
- All assets/API responses use no-store. The worker intercepts navigation only
  and shows an anonymous offline message on fetch failure. No Cache API, offline
  terminal content, credential or conversation persistence.

## Conversation reader

Codex and Claude public user/assistant messages are available from the active pane's
authoritative provider binding. Claude uses the same PID registry and exact
transcript binding as the catalog, including supported cswap roots; the reader
never selects a transcript by directory recency. Provider and tmux identities are
rechecked after reading. Claude tool results, thinking blocks, sidechain messages,
metadata and compaction summaries are excluded. The assistant label follows the
server-provided provider. Markdown tables and existing filters apply to both.

Messages render GitHub-flavored Markdown: headings, emphasis, nested/task lists,
quotes, links, fenced/indented code and aligned tables. Wide tables and code blocks
scroll within the message on narrow screens. The question filter remains available;
the code toggle hides assistant code blocks while keeping inline code and user code.
The renderer creates allowlisted DOM elements from parsed tokens and inserts text
with `textContent`. Raw HTML stays literal, only absolute HTTP(S) links without
embedded credentials open in a new tab with `noopener noreferrer`, and images
are explicit links rather than automatic external requests.

Home filters internal Codex handoffs before returning conversation text. Private
channels, tool records, injected environment/instruction wrappers and
`codex_internal_context` goal reminders are excluded. Assistant text matching the
summary in a `compacted` record is also hidden, regardless of its heading. Matching
uses exact trimmed text from the model-continuation envelope, not broad words such
as “Goal”, “Task” or “Next steps”. Hidden summaries do not consume the public message
count limit.

The reader inspects only complete records in its bounded 4 MiB tail. For compaction
matching it may read an envelope from a record above the public-message line limit;
replacement history is never returned. If the matching record is outside the tail
or incomplete, the existing narrow `## Task and constraints` → `Workspace:` fallback
still applies. Other unconfirmed summary formats are not exhaustively classified.
User-authored task specifications, quoted examples and normal replies remain visible.
Source rollout files are unchanged; reopen the reader to fetch the filtered view.

## Accounts, tabs and continuity

New Gateway installations use a one-time web setup flow. `init-web` creates a
private connector token and a separate `<credentials-path>.bootstrap` setup token;
passwords and TOTP are entered in the browser. Until setup completes the Gateway
serves only static assets and bounded setup endpoints, not protected APIs or Home
connections. Setup requires the private token and the configured same-origin HTTPS
boundary. Creating credentials atomically retires setup; normal login is still
required. Existing credentials never reopen setup. The legacy interactive `init`
command remains available for manual administration.

The primary credential is the configured `--credentials` file. Up to eight extra
credential files live in `<credentials-path>.users/`; duplicate names are rejected.
Restart the gateway after administrative credential-file changes. Changing an account's password,
TOTP secret or identity invalidates that account's stored logins; other accounts
remain signed in. TOTP replay-counter changes do not invalidate sessions.
Account security's TOTP switch applies immediately and persists across restart.
Both enabling and disabling require the current password plus an unused code from
its retained authenticator enrollment; no QR/seed is exposed in the web app. The
current login survives with its original expiry, other logins of that account are
revoked, and other accounts remain unchanged. Reauthentication is limited to five
attempts/minute/account and shares the bounded password-hash workers. The default
omitted `totp_disabled` field preserves legacy enabled credentials and session
fingerprints. Credential changes are backed up privately under
`<primary-credentials>.backups/`, outside the strictly parsed extra-account folder.
Storage failures fail closed; do not restore an old credential/session backup as
an ordinary rollback because it may restore an older authentication policy.

Authenticator recovery uses trusted administration, private timestamped backups,
new credential paths and enrollment; it has no weaker web bypass.

The primary web account uses Home's tab store. Additional accounts use the native
merge implementation at `web-profiles/<username-sha256>/shared-workspace/` beside
the credential file. Only authenticated identity selects a profile; browser profile
injection is rejected. Back up profile state during server migration.

Each profile has up to 32 shared tabs/order. Selection, font size and sidebar
preference remain device-local. Session identity is always `{id, created_at}`.
The same account shares tab additions, closes and ordering across its clients.
Closing a tab changes that profile's shared layout but never ends original tmux.
Local changes trigger a sync immediately; other visible clients poll every five
seconds, and background clients refresh on resume. This is eventual synchronization,
not an instantaneous or durable offline outbox guarantee. Web API requests have a
30-second deadline including response body reads, so a stalled request cannot hold
workspace synchronization indefinitely. Uncertain writes retain their operation ID
for retry. All API requests belong to their originating login and are aborted on
account disposal; stale responses and delayed bodies cannot update a new login or
send it back to the login page. Only a current authenticated request's 401 expires
the UI login. Transient startup errors retry without discarding authentication,
and font downloads do not block initial state loading.
Each visible browser keeps only its selected view connected. Hidden documents
release their view; resume/network recovery refreshes state and reconnects the
selected valid session. Terminal recovery runs independently of shared-tab sync.
Connection setup has a 20-second timeout. Network/protocol failures retry with
jittered exponential delays capped at 15 seconds. Connection-limit/unavailable
responses and output pressure wait at least 10 seconds, with a 60-second cap.
Output pressure recovers automatically after pending xterm writes drain; its
1 MiB queue budget belongs to the terminal across socket generations. A healthy
connection resets failure history after 10 seconds, or when intentionally released.
Foreground/network recovery expedites transient retries but preserves capacity
and output cooldowns. Ordinary polling never bypasses backoff. Offline/page-hide
events release the view; resume replaces stale state requests and BFCache restores
reopen the selected view. Gateways advertise application heartbeats every five
seconds; a browser replaces a socket silent for 20 seconds even if WebSocket
still reports OPEN. Older gateways without that capability do not activate the
watchdog. Tab release/disposal cancels open, retry and heartbeat timers.
The status tooltip and notice explain the last failure; browser console entries
record only category, close code, attempt and retry delay, never terminal content,
account identifiers or raw server reasons. The original tmux/provider processes
remain running.

### Login session management

Settings lists only the signed-in user's login sessions, showing browser/OS,
login IP, approximate city/region/country, login time, recent activity, expiry,
and the current-browser badge. Each login can be revoked, including the current
one. Revocation uses same-origin + CSRF and verifies ownership server-side;
opaque public IDs grant no authentication. Existing tmux sessions are not ended.
Recent activity follows existing active-input/action semantics (passive polling
is not activity), with disk updates throttled to five minutes.

With owner authorization, public login IPs are looked up server-side over HTTPS
using `ipwho.is`; no username, cookie, terminal content or other credential is
sent. Results are approximate, particularly with VPNs/mobile networks. Lookups
are bounded to two seconds/two concurrent requests, cached for 24 hours on
success and one hour for provider failures, and capped at 256 IPs in memory.
Cancelled requests are not failure-cached. Private/reserved IPs are not sent.
Location lookup does not gate login or revocation; unavailable results show
“위치 확인 불가”. Provider reference: https://ipwhois.io/documentation.

The initial upgrade from the old memory-only version requires one fresh login;
subsequent gateway restarts preserve new sessions. Back up the private session
store with configuration, but do not restore an old session snapshot after
revocations: it can restore previously valid logins. For emergency sign-out,
stop the service, timestamp-backup/remove the session file and restart.

## New-session workspaces

Home installation selects each profile's workspace base (new-install default:
`~/.hmux`; existing custom bases are retained). Creating a session allocates a new
child folder from its name and a unique folder/profile-prefixed tmux name. Repeated
names never attach to existing work or reuse existing directories. Codex/Claude
exit returns to an interactive shell, including for resumed sessions. See
[Operations](OPERATIONS.md#build-and-install) for naming and installation flags.

## Fast workspace restoration

After authentication, the browser can show a validated, account/profile-scoped
local preview of the session list and open tabs before state synchronization.
The preview stores only names, aliases, provider labels and exact session identities;
no paths, provider state, credentials, transcripts or terminal output are cached.
It is limited to 256 sessions, 32 tabs and seven days. Logout removes the current
preview. A live online catalog is required before connecting a cached tab; shared
workspace synchronization remains authoritative and reconciles remote changes.
Existing profile-specific tab preferences remain a migration fallback; the old
unscoped key is not imported into an account-scoped preview without ownership.

Home shared-workspace requests use the basic catalog plus existing metadata,
visibility and recovery overlays, avoiding process/provider transcript scans.
Regular shared catalog collection still supplies current provider state.

### Bounded background work and tab initialization

Restored tabs keep lightweight identity/UI state until first selected; only selected
tabs allocate xterm and its input bridges. Already visited tabs retain their terminal
state, within the existing 32-tab limit. Only the visible view connects. Usage dialogs
skip unchanged polls and reconcile changed text/attributes while preserving mounted
nodes and scroll; time-dependent labels refresh on the existing 30-second cadence.

Conversation loading uses the live catalog runtime to show Codex or Claude;
unknown/unverified runtime uses a neutral label. The server response still owns
final provider attribution. A compact message skeleton respects reduced-motion
preferences and does not imply progress percentages.

### Connection diagnostics

Authenticated browsers automatically report bounded connection diagnostics to
the same gateway at `/api/diagnostics`. Settings → **접속 진단** shows the current
account's recorded error count and downloads its server records together with
this device's recent records as JSON for analysis.

Events include terminal disconnect/recovery, API failure, offline/resume and
uncaught JavaScript/rejection categories. Metadata is restricted to fixed reason
and route codes, HTTP/WebSocket status, retry count/delay, elapsed time, script
line/column, online/visible/PWA flags and the frontend bundle identifier. The
server adds receipt time and a browser/OS family label. Raw exception messages,
stacks, URLs, IP addresses, terminal text, keystrokes, request/response bodies and
authentication secrets are excluded. Records are diagnostic client claims, never
authorization evidence. No external analytics service receives them.

The device keeps at most 100 sanitized records for 24 hours in optional
`sessionStorage`, keyed by the server-issued public login ID. Unsent records
survive refresh in that tab; logout/account disposal clears its outbox. Repeated
same-category errors within a second are collapsed. Uploads have one in-flight
request, a five-second deadline, batches of at most 20 and retries spaced 10–60
seconds apart. Upload failures do not generate more diagnostic events or change
the app's login/terminal state. Storage blocking does not prevent app startup.

The gateway derives ownership from the authenticated username/profile, applies
the usual Origin/CSRF checks, deduplicates client UUID/sequence pairs and limits
each login to six batches per minute. Private `<credentials-path>.diagnostics.json`
holds at most 2,048 records globally and 256 per account, pruned after seven days.
The existing exclusive gateway state lock covers this adjacent store. One worker
persists/prunes every ten seconds using atomic replacement and mode 0600; disk
writes do not hold terminal or auth locks. HTTP 202 acknowledges in-memory intake,
so an abrupt gateway crash can lose the last ten seconds of server records. Recent
device records remain available in the download. A storage failure is reported in
settings while terminal access remains available; malformed/private-mode-invalid
files disable diagnostic writes without overwriting the original file.

All terminal open, resize and redraw requests use the same 2–500 column and
2–250 row bounds. A one-row browser viewport must not emit an invalid PTY resize.
Known gateway endings use fixed WebSocket close codes: `4001` for Home transport
unavailable, `4002` for output overflow or stalled rendering, `4003` for disposable view
exit and `1002` for invalid client frames. Browser output overflow and gateway
output overflow both retain the bounded pressure cooldown; the code distinguishes
their origin. Actual transport loss can still surface as `1006`. Close reasons
never contain raw Home errors. A `4003` ends only the disposable view and does
not establish that the original tmux session ended.

Terminal output negotiates `terminal-output-flow-v1` across the browser, gateway
and Home. Home keeps at most 32 unacknowledged frames (512 KiB, 16 KiB per frame)
per disposable view, plus one pending PTY read. The browser returns credit only
from xterm's ordered write callbacks. ACKs recheck login validity without
extending idle activity; old socket callbacks cannot credit a new
connection. Home waits for credit without blocking its shared message reader.
Both frame count and bytes are bounded, including bursts of very small writes.
No terminal bytes are discarded to relieve pressure.

An unacknowledged browser frame older than 30 seconds releases only that view
with `4002`, checked on the five-second heartbeat tick. Small trickle ACKs do not
extend older frames' deadlines. Home also bounds a blocked credit wait at 40
seconds. These deadlines prevent a frozen renderer from retaining an attached
PTY indefinitely; the original tmux session/provider is not terminated. Existing
overflow guards remain for legacy or noncompliant peers. The bounds cover HMux
buffers, not tmux's internal per-client buffering or overall process RSS.

For rolling upgrades, old browsers receive Home credit after the gateway writes
to their socket; render-aware pacing requires loading the new browser assets.
Old Home connectors keep their prior protocol and never receive unknown ACKs.
Negotiation, ACKs and cleanup are bound to the exact Home connection used to open
the view, so replacing Home cannot route stale controls to its successor.

For investigation, compare event times, bundle/browser, API status and terminal
failure/recovery pairs in the exported JSON. The `counts` field groups recorded
errors by kind/reason. Server operators can inspect the private JSON locally;
never commit it or include it in releases. A deployed collector cannot reconstruct
errors that occurred before the browser loaded that version. Fixes still require
review and verification; log collection does not execute code or apply changes.

## User interface

- The redraw icon immediately left of Attach resynchronizes the PTY size, sends
  SIGWINCH to the active pane's foreground process group and runs
  `tmux refresh-client` for the existing browser view's exact client. This lets
  the running program recompute its display as well as repainting tmux's stored
  screen. Home derives and rechecks pane/process identities; the browser cannot
  supply a PID or device. It does not reload, reconnect or send application input.
  Success briefly highlights the icon; failure shows an error. The top toolbar
  retains this control; the compact mobile accessory row omits it to keep room for
  input keys. Home limits redraws to one per second per view; unavailable views
  disable the control.
- Mobile accessory keys reserve enough width for their labels, including `Ctrl+C`.
  Tapping Esc/Tab/Ctrl/Ctrl+C/arrows preserves terminal input focus and sends once.
  Swipe/cancel/multitouch does not send a key; horizontal overflow remains scrollable.
  File attachment keeps its separate native picker behavior.
- Alias-first A–Z session list, Home profile session creation, rename and hide/show.
- Neutral graphite chrome, restrained slate accents and compact 4–5px corners. A 44px tab
  bar contains 34px desktop tabs with visible inactive borders and an active muted
  top accent. The wide desktop sidebar is 268px; the status bar stays on one line.
- Alt+1–9 selects the corresponding tab in current sidebar-independent tab order
  (an absent tab number does nothing); Alt+Shift+Left/Right changes tabs; Alt+L or Alt+Backquote (` / ₩ on Korean layouts) toggles sidebar;
  Alt+W closes the active shared tab without terminating its tmux session; Alt+Q logs out; Cmd/Ctrl+K searches.
  Auto-reveal scrolls the tab strip horizontally, never the whole mobile page.
- Conversation reads the active Codex or Claude session resolved by Home/tmux. User messages
  are included by default and kept in order; user fenced commands remain visible
  when assistant code is hidden. Initial opening scrolls to the latest message.
- Settings → 사용량 saves independent Claude/Codex visibility and source
  choices to the authenticated web account. Claude selects Claude CLI or cswap;
  Codex selects Codex CLI or codex-lb. Defaults remain cswap/codex-lb. Choices
  synchronize through state polling and survive gateway/browser restarts. A stale
  device cannot overwrite a newer revision; it must reload the settings.
- Source choices select genuinely separate quota snapshots. CLI means the current
  provider login; cswap means its registered accounts with the unique active
  account as headline; codex-lb means the configured weighted account pool.
  Codex-lb account details use `accounts_updated_at` from the account export,
  independently of pool quota freshness. Its `last_refresh_at` is OAuth token
  refresh time and must not hide recently observed account details. Account-list
  observations over 30 minutes old or without a valid timestamp remain unknown.
  This timestamp proves when the list was observed, not when the upstream provider
  last measured its quota.
  Missing/failed sources never silently use a different source. Account aliases
  come from codex-lb; cswap account details show authorized email labels. Never
  sum percentages or treat missing/expired quota as available capacity. Keychain
  failure is an unavailable-account status, not zero usage.
- Codex shows verified plan badges (including Plus/Pro) on CLI summaries and
  pool accounts when supplied by the selected source. Missing plans are not
  inferred. Five-hour gauges are omitted where that window is absent. The Codex
  combined five-hour gauge is also hidden whenever any active account lacks
  that window; inactive accounts do not control it. Both
  providers show weekly reset countdowns, refreshed every 30 seconds while the
  dialog is open, with the local reset date in the tooltip. Unknown timestamps
  remain unknown; expired windows wait for fresh quota instead of showing 100%.
  A refresh error does not hide a measurement younger than 30 minutes: the
  dialog shows its source observation time and refresh status. A generated
  transport timestamp cannot make failed or missing measurements appear fresh.
  Reset labels use at most two units (for example `6일 23시간 후 초기화`).
  The open dialog updates when each new state response arrives, without waiting
  for the separate 30-second countdown timer.
- Visibility controls display only. One shared Home collector serves all web
  accounts; hiding usage does not change provider logins, execute an account
  switch or stop another user's collection. Private settings live in
  `<credentials-path>.usage-preferences/` as atomic mode-0600 per-account files.
- The Home connector invokes the embedded source-aware collector directly in
  process. Provider-matched `sources` snapshots travel over its existing WSS
  connection; no SSH usage stream or external helper is involved.
  The cswap adapter uses the installed `cswap list --json` command with bounded
  execution/output and no shell. cswap owns its existing shared quota/cache and
  credential handling; HMux never calls switch/login/service-install commands.
- Remaining-capacity gauges use red at 15% or less, amber at 35% or less,
  otherwise green. Unknown values show a patterned waiting track; freshness and
  reset rules still apply.
- Footer shows only `Codex` / `Claude` provider labels, with Codex first in the
  footer, usage dialog and settings. Source names remain inside the dialog.
  The dialog separates combined capacity from account rows, using larger summary
  values and compact labels; unknown reset times are omitted. Desktop uses a
  dedicated dialog up to 1180px wide with Codex/Claude side by side. Narrow screens
  stack providers and account labels above their gauges. A single enabled provider
  uses the full width, and live refresh preserves the dialog scroll position.
- Footer stays on one line. Under 540px, computer resource details are hidden;
  full provider labels remain. Resource details are also in the usage dialog.
  CPU/GPU/RAM are percentages; disk is used/total capacity in decimal GB or TB.
  Disk describes Home's startup APFS container (macOS) or root filesystem (Linux),
  not a sum of visible folder sizes. On a Linux Home, CPU is the busy share of two
  `/proc/stat` readings 250 ms apart, RAM is `MemTotal - MemAvailable`, and GPU is
  omitted.
  Older connectors omit disk; display unknown rather than inventing values.
- Vector Bedl frames use the maintained silhouette/order, use usage-label ink and
  render at 85% artwork scale. Fresh burn state controls speed; offline/stale data
  or reduced motion shows a still frame. No raster filters or CSS masks.
- Dialogs have 4px corners, a fixed title row, scrolling body and aligned actions.
  Settings has no background dimming; dialogs have no drop shadow.

### Terminal selection and links

When tmux mouse tracking is enabled, ordinary primary dragging selects local
terminal text for Cmd+C on macOS or Ctrl+C elsewhere. macOS Ctrl+C remains
a terminal interrupt. Primary clicks and wheel remain remote mouse
input; modified gestures retain xterm behavior. Local dragging takes priority
over remote pane dragging. Mobile selection and IME behavior remain unchanged.
HTTP(S) text URLs and OSC8 links show a small URL popover on click, with an
explicit new-window action (`noopener noreferrer`). Unsafe schemes and embedded
credentials are not opened. Wrapped links retain the full address. Dragging a
URL selects text without opening the popover; Escape/outside click/scroll,
settings opening and tab switch dismiss it.

On iPhone and Android, a short single-finger tap on a terminal HTTP(S) URL or
OSC8 link opens the same popover. Long presses (350 ms or more), movement over
5 px, multiple fingers and existing native selections do not activate links.
Editable input/Paste targets retain native gesture ownership. Tapping a link does
not focus the keyboard; the explicit “새 창에서 열기” action opens a new tab/window.
Touch scrolling and native selection/copy keep their existing behavior.
The shared popover is constrained to the visible viewport and uses 44 px action
targets. Mobile hit testing reuses the pinned xterm 6 synchronous link providers;
validate repeated taps, wrapped URLs and OSC8 if upgrading xterm.


### Web file attachments

Drop images/files onto the work area on desktop, or use the paperclip in the
terminal toolbar. On mobile the native file picker offers the platform's file/photo
sources; the keyboard-visible auxiliary row also has an attachment button. Only
an active connected terminal accepts an attachment. Directories and empty files
are rejected. Limits are 1–16 files, 32 MiB per file and 128 MiB total.

An attachment streams through an authenticated same-origin WebSocket (`/api/upload`)
to the existing Home connector, with CSRF in the first frame. Each binary chunk
is at most 256 KiB and is acknowledged before the next. Global concurrency is two,
with one per login; the overall deadline is five minutes and idle timeout 30 seconds.
The connector advertises `web-upload-v1`; older connectors fail closed. Existing
Nginx HTTP request-body limits do not need changing for this WebSocket path.
Original names and local paths stay in the browser: only size and sanitized
extension accompany the bytes. Home stores files in the existing private staging
spool with generated names and verifies exact tmux identity before and after the
transfer. Gateway validates response identity, generated paths, sizes and SHA-256.

Successful uploads insert POSIX-quoted paths through xterm paste without Enter.
Automatic insertion requires the same original tab instance, identity, terminal
connection generation and selection epoch, with the app visible and focused.
Tab/dialog/reader/focus changes leave an explicit “경로 넣기” action for the original
tab. Closing that tab, cancelling or starting logout cancels the transfer. Pending
state belongs to the current browser/account and is cleared at disposal.

Web attachments expire **three hours after upload completion**. The foreground Home
connector sweeps expired files on startup and every minute, including during its
network reconnect loop. If Home is asleep or the connector is stopped, cleanup
runs when it resumes/restarts; no separate daemon is installed. Paths left in a
terminal/conversation no longer refer to an available attachment after cleanup.
Partial uploads are cleaned up
on cancellation; spool ownership, symlink checks, 512 MiB quota and 100-stage cap
continue to apply. Only the temporary uploaded copy is removed; the original file
on the attaching device is untouched.

## Mobile/PWA invariants

See [Mobile/PWA invariants](BROWSER_INPUT.md#mobilepwa-invariants).

## Accepted mobile behavior

See [Accepted mobile behavior](BROWSER_INPUT.md#accepted-mobile-behavior).

## Source ownership

See [Source ownership](BROWSER_INPUT.md#source-ownership).

## Usage after connecting

See [Usage after connecting](PROVIDERS.md#usage-after-connecting).

## Usage source fallback

See [Usage source fallback](PROVIDERS.md#usage-source-fallback).

## Session list latency

Home polls tmux every five seconds, but operations that change tmux state
(`create`, `alias`, `hidden`) and the end of a terminal view request an
immediate catalog poll through the Home peer's change notification. The browser
refreshes shortly after such operations and after a terminal socket closes, and
a newly created session is opened as soon as it appears in the catalog (checked
for up to 12 seconds) instead of relying on a single refresh.

## AI provider setup

See [AI provider setup](PROVIDERS.md#ai-provider-setup).

## Build and local provisioning

See [Build and local provisioning](OPERATIONS.md#build-and-local-provisioning).

## Linux/Nginx deployment

See [Linux/Nginx deployment](OPERATIONS.md#linuxnginx-deployment).

## Codex completion notifications

See [Codex completion notifications](PUSH.md#codex-completion-notifications).

## Home operation and continuity

```sh
hmux-web connect --url wss://YOUR_HOST/connect --token-file /PRIVATE/connector.token
```

Run as the owner of Home tmux, with the existing HMux Home-role configuration.
`home.toml` is preferred, with read-only fallback to existing `client.toml`.
See [Home installation](OPERATIONS.md) and [migration](MIGRATION.md).
`--config` may explicitly select private configuration. The connector validates
normal public TLS certificates and initiates the connection; no inbound Home port,
SSH host-key bypass or agent forwarding is needed. Disconnect/reconnect does not
resume provider processes itself; common Home recovery owns that behavior.

The connector must remain running while remote web access is needed. Optional
native service installation provides a per-user macOS LaunchAgent or Linux systemd
user service, preserving the user's PATH and existing Home configuration. See
[automatic Home startup](OPERATIONS.md#automatic-home-startup-macos-and-linux) for
installation, verified migration from a foreground connector, and stop/uninstall.
The OS restarts an exited connector; the connector reconnects after network loss.
Its state-directory lock prevents concurrent instances from duplicating collectors.
macOS startup is after GUI login and requires an awake host. Linux boot/logout
continuity requires explicitly configured lingering. On Home reboot, the catalog
path invokes existing tmux/provider recovery; process-running status alone does
not prove a live gateway connection.

Closing a tab removes it from the account's shared workspace and therefore from
every connected client of that account (normally within five seconds).
A tab whose tmux session has ended (for example exiting its final shell) is
closed the same way once the session is missing from two consecutive catalogs
while Home is online, instead of lingering as "세션 없음"; a transient gap or an
offline Home never closes a tab. Closing a browser
window or losing its socket releases only its PTYs; the shared tabs remain.
The PTY cleanup touches only its generated grouped view.
A view-local tmux client-detached hook also removes an attached web view after
abrupt connector death. An exceptional crash between view creation and the first
attach can leave an unattached marked view; there is no unsafe prefix-wide sweep.
Original tmux sessions continue. Grouped windows still share tmux sizing policy.
On network loss the current terminal remains visible with automatic retry and a manual reconnect action;
reconnection uses exact tmux identity, not an old provider ID or display name.

## Verification and acceptance

Follow [contributor checks](../CONTRIBUTING.md) and [native verification](../tests/RUST.md).

Run `make integration` for the native Gateway/Home runtime and isolated tmux
lifecycle checks. It uses only disposable `hmux-e2e-*` resources and never targets
an existing tmux server.

The socket test uses fake Home/loopback. The tmux test uses a dedicated socket and
`hmux-e2e-*` names; never attach or alter pre-existing sessions for testing.

Automated coverage does not prove physical-device UI acceptance. Verify Android
tab → keyboard transitions, background/resume, iOS direct Korean composition, safe areas,
modal focus, fonts, and footer overflow on actual devices when these paths change.
User-confirmed fixes and remaining limits are in [Validation](VALIDATION.md).
A frontend deployment needs no gateway restart when the process serves assets
through the current symlink. Backend changes require restart; Home-only disk
collection also requires the updated Home connector. Never claim disk is live
merely because the frontend or Linux binary was updated.
