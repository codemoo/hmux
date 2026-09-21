# Web HMux

This is the current web/PWA reference. Historical experiments are archived and
are not implementation instructions. HMux Web uses TypeScript, Vite and xterm.js
with a Go gateway and one outbound Home connector. This is the primary HMux client;
the optional macOS app is documented separately under `macos/`.

## Runtime and security

Browser → HTTPS/WSS → Linux Nginx → loopback Go gateway. Home → authenticated
outbound WSS → gateway. All terminal/catalog/provider operations originate on Home.
One shared catalog/usage collector serves every web client; computer resources
always describe Home, never the phone or Linux gateway.

The gateway terminates authentication/TLS and is trusted with terminal authority.
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

Primary native/web tabs share Home's store. Additional accounts use the same Go
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
unavailable, `4002` for gateway output queue overflow, `4003` for disposable view
exit and `1002` for invalid client frames. Browser output overflow and gateway
output overflow both retain the bounded pressure cooldown; the code distinguishes
their origin. Actual transport loss can still surface as `1006`. Close reasons
never contain raw Home errors. A `4003` ends only the disposable view and does
not establish that the original tmux session ended.

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
  Success briefly highlights the icon; failure shows an error. The same icon is available
  beside Attach in the keyboard-visible mobile row. Home limits redraws to one
  per second per view; unavailable views disable the control.
- Alias-first A–Z session list, Home profile session creation, rename and hide/show.
- Native HMux/Flexoki colors, 36px tab bar and 26px tab items, 296px desktop sidebar.
  Selected tabs use a blue border/underline without changing their dimensions.
- Alt+1–9 selects the corresponding tab in current sidebar-independent tab order
  (an absent tab number does nothing); Alt+Shift+Left/Right changes tabs; Alt+L or Alt+Backquote (` / ₩ on Korean layouts) toggles sidebar;
  Alt+W closes the active shared tab without terminating its tmux session; Alt+Q logs out; Cmd/Ctrl+K searches.
  Auto-reveal scrolls the tab strip horizontally, never the whole mobile page.
- Conversation reads the active Codex session resolved by Home/tmux. User messages
  are included by default and kept in order; user fenced commands remain visible
  when assistant code is hidden. Initial opening scrolls to the latest message.
- Codex headline is Home's weighted weekly pool; account aliases come from codex-lb.
  Claude headline is the cswap active account; account details show email addresses.
  Never sum quota percentages or treat missing/stale usage as available capacity.
- Footer stays on one line. Under 540px, computer resource details are hidden;
  full provider labels remain. Resource details are also in the usage dialog.
  CPU/GPU/RAM are percentages; disk is used/total capacity in decimal GB or TB.
  Disk describes Home's startup APFS container, not a sum of visible folder sizes.
  Older connectors omit disk; display unknown rather than inventing values.
- Vector Bedl frames reuse the native silhouette/order, use usage-label ink and
  render at 85% artwork scale. Fresh burn state controls speed; offline/stale data
  or reduced motion shows a still frame. No raster filters or CSS masks.
- Dialogs have 4px corners, a fixed title row, scrolling body and aligned actions.
  Settings has no background dimming; dialogs have no drop shadow.

## Mobile/PWA invariants

Android: Chrome is the recommended browser; the user confirmed both correct
terminal colors and successful PWA installation on 2026-09-09. Use Chrome's
menu → Add to Home Screen → Install, or HMux's login/settings install button.
iOS: Safari → Share → Add to Home Screen (Open as Web App when shown).
Edge can expose the install button but has not received device acceptance here.
Manifest PNG icons are
192/512px and Apple touch icon 180px. Standalone/browser logins may use separate
storage. PWA updates do not forcibly reload an active terminal. Network is required.

Samsung Internet's home-screen app showed unusual backgrounds on digits and
symbols; the same issue was absent in Chrome. Samsung Internet remains unverified
after the color-preservation mitigation; do not report it as fixed. See
[web troubleshooting](TROUBLESHOOTING.md#webpwa-installation-and-terminal-colors).

Terminal colors live in `theme.ts`. Bold text does not automatically promote ANSI
colors to bright variants. Extended indices 22/52 use muted Flexoki green/red for
observed tmux diff fills; other extended colors remain standard. Palette changes
affect foregrounds and backgrounds; explicit truecolor output is unchanged.
`color-scheme: only dark` opts out of user-agent auto-dark recoloring. Only xterm
uses `forced-color-adjust: none` to preserve ANSI semantics; app controls remain
eligible for accessibility color adaptation. Vendor overrides may behave differently.

Monatendard Nerd Font Mono Regular/Bold match `archive/terminal/config/ghostty.ghostty`, with bundled
licenses and all 11,172 modern Hangul syllables. Load both faces before measuring;
Android explicitly registers FontFaces with WOFF2 then TTF fallback. Resume/network
recovery retries loading and redraws terminals. Mobile font defaults to 10px,
desktop 14px, both adjustable 8–24px with separate preferences.

`viewport.ts` owns visualViewport height/offset and safe-area geometry. Reserve
insets once, retain top inset during keyboard transitions, reset on rotation, and
subtract already-excluded bottom space. The keyboard-closed workspace reduces the
remaining bottom inset by 12px; login/drawers/dialogs retain the full inset.
Android resize-content shrinks both viewports, so keyboard detection tracks the
expanded height per orientation. Never subtract keyboard height a second time.

Keyboard-visible layout hides normal tabs/footer and exposes a top-right floating
tab button. Auxiliary keys stay compact; pinch zoom is disabled. Remeasure on tab,
dialog, focus, viewport and resume transitions, including settled measurements.

Desktop macOS Safari uses the native-input bridge for the physical Safari 18.6
Hangul trace: `insertText` arrives before keydown229 and selected
`insertReplacementText` updates the local run. The browser owns syllable changes,
including no-op replacements, resyllabification and Backspace. An inline preview
remains local until space, Enter, navigation, a shortcut or paste flushes it once.
Pending text and its short-lived echo preview use the cursor cell's background
and foreground, including RGB, indexed theme colors and inverse video. Colors
refresh on terminal redraw, so black and gray Codex input rows keep their own fill.
This shared behavior also applies to the iPhone bridge.
Modifiers alone do not flush. No remote delete/replace or Hangul reassembly is used.
Desktop textarea geometry and mouse selection remain stock; standard composition
stays with xterm. On 2026-09-10 the user confirmed the deployed Mac Safari input
works and accepted the current behavior ("잘된다. 이정도면 괜찮음."). Automated
regression checks and this user confirmation are separate evidence; no additional
Mac input acceptance is pending. The diagnostic page retains its stock desktop
baseline for comparison.

Android retains stock xterm 6.0.0 input. iOS uses the same native xterm textarea
with a targeted bridge for the physically observed keyCode-0 Hangul path: Safari
updates syllables with deleteContentBackward + insertText and no composition
events, while stock xterm sends raw jamo from keypress. The bridge leaves native
DOM editing intact, shows the pending Korean run inside the terminal and commits
it once at a boundary (space, Enter, navigation/control, paste or active-view blur).
Backspace during that run edits the native value; it is not sent as remote repair.
There is no separate editor, Hangul library, syllable-tail reassembly or NFC rewrite.
Standard composition events use xterm. Safe view transitions flush to the original
live connection first; unexpected disconnection cancels unfinished input so it
cannot leak across reconnects/tabs. The user accepted the current Korean input,
deletion and space-to-echo behavior. See [iOS terminal input](IOS_INPUT.md).

Mobile browser-native selection on rendered rows remains enabled. Long press and
selection-handle drags are excluded from tmux touch scrolling; a native selection
is not replaced by xterm's internal selection. On iOS the existing cursor-positioned
textarea is exposed for native long-press hit testing, with a small touch area and
native callouts. Its gestures bypass desktop selection/refocus/scroll handlers
without preventing browser defaults or rewriting its value. Paste still uses
xterm's clipboard path, flushing pending Hangul first. No second input, clipboard
dialog or custom menu is used. The user confirmed native Paste works; its slightly
slow first menu appearance was accepted without further tuning.
On rendered text, a touch context menu is kept native before its range appears;
long-press/drag clicks do not refocus the input. Only a new short tap resumes it.
On iPhone, selecting output still dismisses the keyboard. Selection/Copy with the
keyboard kept open is not supported by the accepted behavior. The user chose to
keep this behavior and stop that work; no additional confirmation is pending.

Android keeps its top-left input anchor when opening the keyboard. Once the
textarea is focused and the keyboard is visible, `android-native-paste.ts` exposes
that same textarea across the current input row through a visual transform for
native long-press Paste. Blur/keyboard dismissal restores pinned geometry. iOS and Android share
editable gesture protection; Android's xterm input/composition/paste handlers are
unchanged. The user reported that the full-row target appears to work in Chrome
PWA and chose to keep it. This is initial feedback, not exhaustive device coverage.
Inactive-host protections remain. iOS input positioning follows xterm normally.
Tab/dialog transitions do
not automatically focus Android input. Touch scrolling uses local history or tmux
wheel events. Preserve these accepted input/viewport behaviors when maintaining
the clipboard path.

## Accepted mobile behavior

| Platform | Current behavior | Limits / decision |
| --- | --- | --- |
| iPhone | Inline native Korean editing, smooth space handoff, native long-press Paste | User accepted for current use; first Paste menu can be slightly slow |
| iPhone output selection | Browser-native selection/Copy | Keyboard dismisses; user accepted this and stopped keyboard-open selection work |
| Android Chrome PWA | Default xterm input; long press on the current input row with keyboard open | Full-row Paste target appears to work per initial user feedback; keep current behavior |
| Samsung Internet | Color-preservation mitigation exists | Token-background issue remains unverified; Chrome is the confirmed path |

The input caret area on iPhone and current input row on Android expose the real
editable control. Output elsewhere remains browser-selectable terminal text.
No custom clipboard menu or silent clipboard access is used. See
[mobile input troubleshooting](TROUBLESHOOTING.md#mobile-input-and-native-clipboard)
for regressions and [current evidence](VALIDATION.md)
for build/deployment details.

## Source ownership

| File/module | Responsibility |
| --- | --- |
| `web/src/main.ts` | UI composition, tab/connection lifecycle, API and workspace coordination |
| `dom.ts`, `icons.ts` | Typed text-only DOM construction and fixed local SVG icons |
| `conversation-view.ts`, `markdown.ts` | Markdown conversation display, question/code filters and return controls; request/epoch ownership stays in `main.ts` |
| `usage-view.ts` | Usage footer, account gauges and usage dialog; quota interpretation stays in `usage.ts` |
| `account-security.ts`, `login-sessions.ts` | Account settings and login-session dialogs with abort/disposal ownership |
| `viewport.ts`, `mobile.ts` | Viewport/keyboard state, font preference bounds |
| xterm 6.0.0 / `ios-native-input.ts`, `ios-native-input.css` | Default input; iPhone native Hangul transaction, echo preview and editable Paste target |
| `native-clipboard.ts` | Native rendered-text selection/Copy and touch gesture ownership |
| `android-native-paste.ts` | Keyboard-visible Android input-row Paste target; pinned keyboard-opening anchor |
| `fonts.ts` | Font loading/registration and family selection |
| `pwa.ts`, `public/sw.js` | Install UI and network-only navigation fallback |
| `terminal-scroll.ts`, `terminal-session.ts` | Touch scroll routing and generation-safe view release |
| `shared-workspace.ts`, `preferences.ts` | Validated workspace shape and guarded local storage |
| `usage.ts`, `types.ts`, `theme.ts` | Usage formatting, data types and Flexoki terminal colors |
| `style.css` → `ios-native-input.css` → `chrome.css` → `dialogs.css` | Base/viewport → iOS input → workspace UI → dialog overrides |
| `internal/webgateway` | Auth, bounded gateway/Home protocol, PTYs and account profiles |
| `internal/hostmetrics` | Home platform resource collection, including disk allocation |

See [web maintenance instructions](../web/AGENTS.md). Preserve CSS order and edit
owning rules rather than appending a new conflicting override.

## Build and local provisioning

```sh
npm ci --prefix web
npm run check --prefix web
npm test --prefix web
npm run build --prefix web
go test ./internal/webgateway ./internal/client ./cmd/hmux-web
go build -o dist/hmux-web ./cmd/hmux-web
```

`deploy/web/build.sh` builds Linux amd64 gateway assets and an Apple Silicon Home
connector. Build dependencies are lockfile-pinned; the deployed server needs only
its binary, built assets and private configuration. `npm audit --prefix web`
checks the frontend dependencies; it is not a full security guarantee.

On the trusted Linux host, run the interactive initializer with private paths:

```sh
hmux-web init --credentials /PRIVATE/credentials.json --token-file /PRIVATE/connector.token
```

It asks for a password without echoing it, displays a TOTP seed/URI for enrollment,
checks an actual code and refuses to overwrite existing files. Copy only the
connector token to private Home storage using the established trusted SSH channel.
Never commit either file or include them in an app/archive backup for public release.

## Linux/Nginx deployment

Use `deploy/web/hmux-web.service` for the dedicated unprivileged `hmux-web` user.
Place immutable releases under `/opt/hmux-web/releases/`, then atomically update
`/opt/hmux-web/current`; retain the old release for rollback. Private runtime files
live in `/var/lib/hmux-web` (0700 directory, 0600 files, service-user owned).
`/etc/hmux-web/runtime.env` defines `HMUX_WEB_ORIGIN=https://YOUR_HOST`.

Render `deploy/web/nginx.conf.example` with the public hostname and local certificate
directory. Install it as a separate site, keeping timestamped backups of prior HMux
site/service files. Do not replace global Nginx configuration. First expose only
`/.well-known/acme-challenge/` on HTTP and return 503 elsewhere, then issue:

```sh
sudo certbot certonly --webroot -w /var/www/hmux-acme -d YOUR_HOST
```

Enable the HTTPS site only after successful issuance and `nginx -t`; reload Nginx,
not unrelated applications. Retain the HTTP ACME location for renewals. A Certbot
deploy hook should run `nginx -t` and reload Nginx when this certificate renews.
The Go server refuses public bind addresses and cleartext public origins.

Systemd restricts writable paths, capabilities, devices and Home-directory access,
with a 256 MiB gateway memory limit. Do not run the gateway as root. Gateway logs
contain startup/error categories, not terminal bytes, prompts or credentials.

## Codex completion notifications

Settings → 완료 알림 enables Web Push for this browser/PWA and this login.
Permission is requested only after clicking 알림 켜기. Use 테스트 알림 to check
OS delivery. iPhone/iPad require iOS/iPadOS 16.4 or later and an installed Home
Screen web app; desktop/Android require a browser supporting Push API.

Notifications show the tab's alias/name and completion status, without prompts,
responses, paths or terminal output. Clicking focuses the existing app or opens
HMux and selects the exact `{id, created_at}` session. A notification belonging
to another or expired login cannot open a tab under the current account.

One Home observer reads authoritative `task_started` / `task_complete` records
from the exact bound Codex rollout. Shared catalog fetches (normally five seconds)
feed one separate worker with one latest pending snapshot; slow notification work
does not block catalog publication or cancel the connector on discovery/send
failure. The worker skips Claude metadata and redundant Codex state-tail scans,
checks cancellation between bounded file chunks/records, and waits five seconds
after an over-budget scan. Peer write deadlines include waiting for the shared
writer. It does not infer completion from idle output or CPU. The first scan,
changed/ambiguous bindings and oversized/truncated history establish a baseline
without replaying old work. A turn already running at baseline can still notify
when it completes. Discovery errors leave terminal operation available. The
connector must be running and Home awake; this is not a durable offline event
queue or a notification for Claude/shell completion.

The gateway checks each subscribed login against its account's authoritative
shared workspace and sends only for matching open tabs. Visible/focused clients
report only their selected session; a live presence lease suppresses that login's
notification for the same tab. Leases expire after 45 seconds if a browser closes
without reporting blur. Logout, revocation, credential-policy invalidation and
seven-day login expiry stop future sends. Re-login requires explicit opt-in;
subscriptions are never silently transferred to another account. Disabling
notifications affects this login's device. Expired provider subscriptions (HTTP
404/410) are removed. Events and outbound work are bounded, deduplicated, and have
a two-minute push lifetime; delivery also depends on browser/OS push service.
Before displaying a queued notification, the service worker verifies the current
login with the gateway. Unreachable authentication or a different login suppresses
the notification rather than exposing another account’s tab name.

The first upgraded gateway run creates `<credentials-path>.push.json` (0600)
with its persistent VAPID key pair and login-bound subscriptions. An owner-only
`.lock` file prevents overlapping gateways from rewriting that state. Keep this file
private and outside release archives, and preserve it across upgrades; losing
the key requires devices to subscribe again. No provider account or API key is
required. Outbound HTTPS is restricted to Apple, Google/FCM, Mozilla and Windows
push-service endpoints with private-address and redirect checks. Payloads use
standard encrypted Web Push and VAPID. A backend and Home connector update are
required in addition to the frontend; restarting these does not end tmux work.

## Home operation and continuity

```sh
hmux-web connect --url wss://YOUR_HOST/connect --token-file /PRIVATE/connector.token
```

Run as the owner of Home tmux, with the existing HMux Home-role configuration.
`--config` may explicitly select that private configuration. The connector validates
normal public TLS certificates and initiates the connection; no inbound Home port,
SSH host-key bypass or agent forwarding is needed. Disconnect/reconnect does not
resume provider processes itself; common Home recovery owns that behavior.

The connector must remain running while remote web access is needed. No macOS
LaunchAgent/login item is installed. After a Home reboot the connector must be
started again; its catalog path then invokes the existing tmux/provider recovery.
A foreground connector launched from an administrator terminal is not an automatic
boot-start solution. Do not claim unattended reboot availability without a separately
authorized persistent startup mechanism.

Closing a tab removes it from the account's shared workspace and therefore from
every connected client of that account (normally within five seconds). Closing a browser
window or losing its socket releases only its PTYs; the shared tabs remain.
The PTY cleanup touches only its generated grouped view.
A view-local tmux client-detached hook also removes an attached web view after
abrupt connector death. An exceptional crash between view creation and the first
attach can leave an unattached marked view; there is no unsafe prefix-wide sweep.
Original tmux sessions continue. Grouped windows still share tmux sizing policy.
On network loss the current terminal remains visible with automatic retry and a manual reconnect action;
reconnection uses exact tmux identity, not an old provider ID or display name.

## Verification and acceptance

Run the build/check commands above. Optional live checks use isolated resources:

```sh
HMUX_RUN_WEB_SOCKET_TEST=1 go test ./internal/webgateway -run TestWebSocketOriginAndLogout
HMUX_RUN_WEB_TMUX_TEST=1 go test ./internal/client -run TestWebAppViewWithIsolatedTmux
```

The socket test uses fake Home/loopback. The tmux test uses a dedicated socket and
`hmux-e2e-*` names; never attach or alter pre-existing sessions for testing.
`TestBrowserPreview` serves fake data only in tests and is not in production.

Automated coverage does not prove physical-device UI acceptance. Verify Android
tab → keyboard transitions, background/resume, iOS direct Korean composition, safe areas,
modal focus, fonts, and footer overflow on actual devices when these paths change.
User-confirmed fixes and remaining limits are in [Validation](VALIDATION.md).
A frontend deployment needs no gateway restart when the process serves assets
through the current symlink. Backend changes require restart; Home-only disk
collection also requires the updated Home connector. Never claim disk is live
merely because the frontend or Linux binary was updated.

Account usage dialog presents provider summary cards and per-account weekly/5-hour
remaining-capacity gauges. Low remaining capacity (15% or less) is red, 35% or
less amber, otherwise green; unavailable values show a patterned waiting track.
Percentages retain existing quota freshness/reset validation. Inactive tabs have
a subtle border; the active tab retains its blue emphasis.


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
Native app staging retains its existing lifetime. Partial uploads are cleaned up
on cancellation; spool ownership, symlink checks, 512 MiB quota and 100-stage cap
continue to apply. Only the temporary uploaded copy is removed; the original file
on the attaching device is untouched.
