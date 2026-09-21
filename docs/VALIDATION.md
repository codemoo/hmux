# Validation

This page records checks for the public source tree. Older implementation and
private-deployment checkpoints are preserved in
[the historical validation log](archive/VALIDATION_PRE_PUBLICATION.md).
They are not claims about a user's independent deployment.

## 2026-09-22 — Pace terminal output at browser consumption

Negotiated output credits now connect xterm write completion to each disposable
Home PTY reader. Each view is limited to 32 outstanding frames / 512 KiB plus one
read, preventing both tiny-frame gateway saturation and browser byte overflow.
ACKs are FIFO, view-scoped, generation-scoped and passive for login activity.
An oldest unacknowledged frame reaching 30 seconds releases only its view with
4002; Home has a separate 40-second credit-wait deadline. Original tmux/provider
sessions survive. Rolling upgrades retain legacy behavior until both endpoints
support render-aware flow control.

Verification: full Go tests with isolated fake-Home socket tests, Go vet, gateway
race tests and focused final race checks; web type/style checks, 148 tests and
production build; native smoke tests and the isolated tmux view lifecycle test.
The socket fixture sends 4,096 one-byte frames followed by 16 MiB per stream,
checks exact SHA-256/order, independent healthy-view/control progress, legacy
clients, malformed ACK rejection, passive login activity and stalled-renderer
cleanup. Unit checks cover Home replacement binding and trickle ACK deadlines.
Chromium with synthetic binary events and real xterm consumed 32,811,968 bytes
in 6,145 frames without reconnecting; maximum pending output was 512,000 bytes.
Delayed writes resumed, the final marker rendered, FIFO ACK sizes matched and
abandoned callbacks did not credit a replacement connection. These are automated
fixtures, not physical mobile-device acceptance or a tmux RSS benchmark.

Release `20260921T181921Z` updates web, gateway and Home. Gateway-only follow-up
`20260921T182209Z` preserves passive login activity on ACKs. Previous releases
and the Home binary are retained for rollback. HTTPS asset hashes, anonymous
API barriers, PWA CSP and gateway service health are checked after activation.

## 2026-09-22 — Display recent measurements during refresh failures

A live `cswap list --json` observation returned recent `lastGoodUsage` values
alongside `token_expired` and `keychain_unavailable` decision statuses. HMux was
incorrectly hiding those display-grade measurements because it required status
`ok`. Recent observations now remain visible with their actual update age and
refresh status. Missing timestamps on failed observations, ages over 30 minutes,
invalid quotas and reset windows remain unavailable; no credential was changed.
The reset text uses at most two duration units and a concise `후 초기화` suffix.
The open dialog now repaints on state response instead of delaying new values
until its 30-second timer.

Web checks, 146 tests and build passed. Chromium reproduced recent measurements
with both cswap failure statuses, verified visible gauges and timestamps, and
confirmed state changes appear within the five-second poll. Source switches,
visibility, persisted preferences and mobile layout remained functional.
Release `20260921T175810Z` updates web assets with the gateway/Home processes
unchanged; HTTPS hashes, anonymous authentication barriers and service health
are verified after activation. This corrects display and refresh timing, not
upstream credentials or API availability.

## 2026-09-22 — Usage dialog refinement

Codex now appears first in the footer, dialog and settings. Footer labels remain
Codex/Claude; the dialog separates combined and account-level capacity with concise
labels, larger summary percentages and compact source/plan badges. Missing reset
timestamps are omitted. An active Codex account without a five-hour window hides
the combined five-hour gauge too; inactive accounts do not affect that rule.

Web type/style checks, 144 tests and production build passed. Chromium fixtures
confirmed provider order, compact footer labels, combined-window suppression,
settings persistence and desktop/mobile layout; screenshots were inspected.
Release `20260921T174535Z` updates only web assets using an atomic release switch.
The gateway and Home processes remain running, preserving existing connections.

## 2026-09-22 — Selectable usage sources, plans and reset countdowns

Settings now stores independent Claude/Codex display switches and source choices
per authenticated web account. The Home collector opts into separate CLI/cswap
and CLI/codex-lb snapshots. cswap is queried with the installed `list --json`
command; CLI requests cannot delay the alternate sources. Codex plan badges use
explicit upstream plan fields, absent five-hour windows are hidden, and weekly
reset countdowns update while the dialog is open. Missing source data never uses
another source. Unavailable accounts retain status and explicitly marked last-known
reset timestamps without presenting stale percentages as current capacity.

Checks completed:

- Full root Go tests and vet; gateway/client/agent race suites. Settings tests
  cover account/profile isolation, restart persistence, private file mode,
  optimistic revision conflicts, auth/Origin/CSRF and failed writes.
- Full vendored collector tests and vet; stream/cswap/account/state race suites.
  Review fixes cover measurement-age expiry through command failures, strict
  source-key/provenance matching and atomic activity merge during quota refresh.
  Focused stream race/vet checks passed after the final concurrency correction.
- Web type/style checks, 142 tests and production build. Chromium synthetic
  API/WebSocket tests verify independent source values, both visibility switches,
  saved settings after reload, Plus/Pro labels, absent five-hour rows and weekly
  resets. Desktop and 390×844 screenshots were inspected. These are automated
  browser checks, not physical Safari/iOS/Android acceptance.
- `make native-smoke` passed after rerunning with access to its isolated local
  test server. No existing tmux session was used by tests.
- A bounded real collector run verified both source maps, codex-lb account plans,
  optional five-hour windows and account reset timestamps. At that observation,
  cswap reported token-expired/keychain-unavailable accounts and Claude CLI was
  rate-limited; those source failures remain visible, not fabricated quota.
  The existing local export producer was backed up and corrected to retain the
  upstream plan field, with its 15 focused tests passing. No scheduler was added.

Release `20260921T172939Z` (UTC) updates gateway, web and Home connector. All 46
staged file hashes were verified; previous releases and the previous Home binary
were retained. Public HTTPS assets, anonymous API rejection (including usage
settings), no-store/PWA CSP, gateway health and Home transport were checked after
activation. Existing tmux/provider work and private login/push stores were preserved.
Reload the browser/PWA to receive the new settings and display.

## 2026-09-22 — First production diagnostics investigation

The inspected account-scoped diagnostic snapshot contained four macOS Safari
terminal failures (`1006`), each followed by recovery in 1.19–2.455 seconds, and
one short push API network failure. There were no runtime exceptions, reported
capacity failures or browser output-overflow events in that snapshot. Gateway
restart count was zero after the prior deployment; inspected proxy records had
no corresponding HMux API error. This does not prove a historical root cause or
exclude intermittent network loss. Private records remain outside the repository.

Inspection found that xterm can fit a one-row viewport while resize/redraw sent
its raw size to a gateway requiring at least two rows. All size producers now
share the existing open bounds. Known gateway shutdown paths now send fixed close
codes instead of all appearing as abnormal network closure: Home unavailability,
gateway output overflow, disposable view exit and invalid client frames. Output
pressure retains bounded cooldown and does not affect other views. The original
four `1006` events cannot retrospectively distinguish these paths.

Checks completed:

- Full Go tests and vet; full gateway race suite including isolated WebSocket
  tests with a fake Home. New cases verify bounded/isolated output overflow,
  accepted two-row resize, rejected one-row frame, close codes and no raw error
  disclosure. No pre-existing tmux sessions were used by tests.
- Web type/style checks, 134 tests and production build.
- Chromium against the production build with synthetic APIs/WebSockets: one-row
  and oversized viewport dimensions are clamped, manual redraw uses the same
  bounds, restoring the viewport does not reconnect, and server output overflow
  is recorded correctly without bypassing its retry cooldown. This is automated
  Chromium verification, not physical Safari or mobile device acceptance.

Independent review found no remaining material defects. Release
`20260921T165620Z` (UTC; September 22 in Korea) was deployed with all 36 staged
file hashes verified and the previous release retained. Public HTTPS assets,
anonymous API rejection, no-store/PWA CSP, service health and Home transport
were checked after activation. Home and existing tmux/provider processes were
not replaced. Existing diagnostic history and private account/push stores remain.
Browsers must reload to receive the size fix and detailed close-code mapping.

## 2026-09-21 — Account-scoped frontend diagnostics

Added bounded browser connection/API/runtime diagnostics, an authenticated private
server store, and Settings → 접속 진단 → 진단 로그 다운로드. Exports combine the
current account's server history with this device's recent records; failed server
access falls back to the device copy. Fixed categories exclude raw exceptions,
stacks, URLs, terminal/input content and authentication secrets.

Checks completed:

- Full Go tests and vet; gateway race tests including isolated WebSocket tests.
  After final review changes, full Go tests/vet and targeted diagnostics race tests
  passed again. Coverage includes account/profile isolation, CSRF/Origin checks,
  deduplication, rate/retention limits, private storage and concurrent intake.
- Web type/style checks, 132 tests and production build. Tests cover bounded
  retry, transport failure isolation, sanitized fields, logout/account transitions,
  expired outbox sequence continuity and API failure callbacks.
- Chromium against a production build with synthetic API/WebSocket responses:
  terminal failure/recovery, runtime error redaction, failed upload without login
  or terminal loss, reload/retry, JSON download and account transition isolation.
  Desktop and 390×844 settings screenshots were inspected. These checks are not
  physical Safari/iOS/Android or real-network acceptance.
- Independent review identified and resolved sequence reuse after local expiry,
  semantic validation of persisted records and a too-small reload decoder limit.
  The full 2,048-record persisted history now survives restart; final focused
  review found no remaining material defects.

Release `20260921T102410Z` (UTC) updates the gateway and frontend, with the previous
release retained and all 36 staged file hashes verified. The Home connector was
not replaced. Existing tmux/provider sessions and account/push stores are preserved.
Public HTTPS assets, anonymous API rejection, no-store/PWA CSP and Home transport
were checked after activation. No production diagnostics records were present at
that check: collection requires browsers to load the new frontend. Historical
incidents cannot be reconstructed by this collector. HTTP 202 acknowledges memory
intake; up to ten seconds of server data can be lost on an abrupt gateway crash.

## 2026-09-21 — Connection recovery regression

Fixed recovery paths where refreshing the page succeeded but the existing client
remained disconnected: permanent output-overflow pause, retained transient backoff
on resume, and silent sockets still reporting OPEN. Output budgets now persist
across socket generations; reconnect waits for queued xterm writes to drain.
The gateway advertises five-second application heartbeats, with a 20-second
browser silence deadline. Capacity/output cooldowns and one visible view remain.

Moved completion discovery out of the catalog publication path into one worker
with one latest pending snapshot. Notification failures no longer cancel the
connector. Completion binding skips unrelated provider/state scans and checks
cancellation between bounded reads/records. Peer writer deadlines include queueing.

Checks completed:

- Full Go tests and vet; race tests for catalog and web gateway, including opt-in
  isolated WebSocket tests with a fake Home, heartbeat delivery and logout closure.
- Web type/style checks, 124 tests and production build. Added checks cover output
  draining across generations, heartbeat expiry/disposal, bounded automatic retry,
  worker coalescing/cancellation, writer contention and completion read cancellation.
- Chromium against the production build with synthetic API/WebSocket responses:
  initial 503/502 recovery without login loss; stalled shared workspace; capacity
  cooldown; output overflow auto-recovery; repeated foreground resume; healthy
  idle heartbeat versus silent OPEN socket; offline/online and BFCache restoration.
  The fixture observed at most one live browser terminal view. These are automated
  browser checks, not physical Safari/iOS/Android or real-network acceptance.
- Independent review findings about abandoned output and completion scan cost
  were fixed; the updated source review found no remaining material defects.

Release `20260921T033805Z` (UTC) was deployed after verifying 36 staged file hashes.
Gateway, web assets and Home connector were updated; the prior gateway release and
timestamped Home binary backup were retained. Public HTTPS assets match staged
hashes, authentication still rejects anonymous API requests, and no-store/PWA CSP
remain intact. One updated Home connector has an established transport. Existing
tmux/provider sessions and persistent account/push stores were preserved.

## 2026-09-20 — Codex PWA completion notifications

Codex completion notifications are opt-in per authenticated browser/PWA login.
One Home observer uses bound rollout lifecycle records before catalog publication
suppression; it never infers completion from inactivity. Delivery checks the
account's shared workspace and exact tmux lifetime. Notifications include the
requested tab name and completion status, and clicking opens that exact tab.
They do not include conversation text. Logout/expiry and unknown or mismatched
service-worker authentication suppress delivery/display.

Checks completed:

- Full Go tests and vet, plus race tests for catalog, client and web gateway.
  After independent review fixes, gateway tests, race tests and vet passed again.
- Web type/style checks, 119 tests and production build. Cases cover initial
  baseline/no replay, fast turns, truncated/partial records, exact account/session
  matching, VAPID/encrypted payload decryption, subscription transfer/revocation,
  natural expiry cancellation, outbound address limits, exclusive state locking,
  notification click routing and stale settings disposal.
- Chromium against the production build with synthetic API/WebSocket/PushManager
  responses: explicit permission request, opt-in/out, test success/failure UI,
  `$0` notification deep link, existing-window target selection and wrong-login
  rejection. Settings screenshots were inspected at desktop and 390×844 sizes.
  The single browser console error was the deliberately injected HTTP 502.
- Independent backend review findings were fixed and re-reviewed with no material
  residual defects. Tests did not attach to or change pre-existing tmux sessions.

Release `20260919T160548Z` (UTC) was deployed after verification of 45 staged file
hashes. Previous gateway/frontend release and timestamped Home binary backup are
retained. Gateway and the one foreground Home connector were updated; no macOS
LaunchAgent or automatic-start item was added. Public HTTPS HTML, JS/CSS, manifest
and worker hashes match the build; no-store/PWA CSP and anonymous API 401 responses
remain intact. Private VAPID state is service-owned mode 0600. The service is active,
and exactly one updated Home connector was verified with established connections.

These checks verify implementation and deployment, not delivery to a physical
user device or acceptance by every push provider. After reloading, enable Settings
→ 완료 알림 → 알림 켜기 in the installed PWA and use 테스트 알림 to verify device
permission and OS delivery. No real device subscription was created by the tests.

## 2026-09-19 — browser connection recovery

Browser API requests now belong to the originating login scope; disposal aborts
them and late responses cannot affect another account. Workspace synchronization
no longer blocks terminal reconnection. Transient startup/state failures preserve
authentication and recover through retries. WebSocket recovery uses bounded
backoff, keeps failure history across short connections, and pauses on output
overflow until an explicit reconnect. Disposal clears pending connection timers.

Checks completed: web type/style checks, 105 tests, production build and whitespace
checks. Independent implementation review found no material defects. Chromium
with synthetic API/WebSocket responses verified initial session 503 and state 502
recovery, reconnection during a delayed workspace response, connection-limit
backoff, output-overflow pause/manual recovery, and background/foreground cleanup
without duplicate connections. These checks did not use existing tmux sessions
and do not establish physical-device or Safari acceptance.

Frontend release `20260919T045638Z` was deployed after verifying all 35 staged file
hashes. The previous release and identical gateway binary were retained; the
service stayed active with its PID unchanged. Public HTTPS HTML, app JS/CSS,
manifest and worker hashes match the build. HTML no-store and PWA CSP remain
intact; anonymous session/state requests return 401. Existing browsers need a
reload to use the new client. No gateway restart or authentication-store change
was required.

## 2026-09-17 — conversation Markdown

The conversation reader now renders Markdown through a token-to-DOM renderer.
Checks completed: web type/style checks, 98 tests and production build; dependency
audit reported no known vulnerabilities. Regression cases cover aligned tables,
escaped pipes, nested/task lists, code visibility, literal HTML, entity decoding,
unsafe links and images without automatic network loads.

Chromium with synthetic responses verified table alignment and formatting at
1280×900, plus internal table/code scrolling without reader overflow at 390×844.
Screenshots were inspected; the browser reported no console errors. This is local
browser verification, not a production deployment or physical-device acceptance.

Deployment follow-up: frontend release `20260917T134005Z` was published after
verification of all 35 staged file hashes. The prior release and gateway binary
were retained; the gateway PID stayed unchanged. Public HTTPS HTML, app JS/CSS,
manifest, worker and new license files match the deployed build. The no-store and
PWA CSP headers remain intact; anonymous session/state APIs return 401. Physical
device acceptance of Markdown remains separate from these deployment checks.

## 2026-09-17 — component cleanup

Web presentation is separated into conversation and usage views, with shared typed
DOM helpers and fixed SVG icons. `main.ts` retains connection/request lifecycle,
abort and session-identity guards. Account settings retain their own disposal guards.
The gateway centralizes session persistence failures in a locked helper that
invalidates active connections; startup session loading keeps its separate path.

Checks completed locally:

- Web type/style checks, 95 tests and production build.
- Full Go tests and vet; race tests for `internal/webgateway`, including existing
  session persistence, revocation, account isolation and storage-failure coverage.
- Chromium with synthetic API/WebSocket responses: usage values and unknown/zero
  distinction, literal external text, conversation question/code filters and return
  controls, and TOTP/login-session settings rendering. Usage-dialog screenshots
  were inspected at 1280×900 and 390×844.
- Independent review of the frozen implementation found no material defects.

Browser checks used no live host or tmux sessions. Responsive Chromium checks do
not establish physical iPhone/Android or Safari IME behavior. This cleanup preserves
existing input code and does not redeploy the running service.

## 2026-09-16 — initial public-source preparation

The repository now presents HMux as one persistent host for Codex, Claude and
shell sessions, accessed primarily through the web/PWA client. The old standalone
terminal packages/configuration/tests are under `archive/terminal/`; the optional
native app remains under `macos/`. Shared host services and native bridge entrypoints
remain buildable. The module path is `github.com/codemoo/hmux`.

Publication excludes private settings, caches, binaries, traces and local screenshots.
The first commits import the current implementation by component without inventing
past development commits. Direct runtime dependency and bundled-font notices are
included; the native static-library SBOM/signing gates remain open.

Checks completed locally:

- Full Go tests, race tests and vet; in-tree collector tests.
- Go formatting, ShellCheck and shell formatting.
- Web type/style checks, 92 tests, production build and deployment archive build.
  The archive contains the gateway, frontend and direct runtime license notices.
- Native smoke checks: models, catalog/workspace changes, surfaces/input/search,
  conversation, catalog stream, usage, metrics, files, helper lifetime and restart.
  The loopback catalog test required execution outside the restricted sandbox.
- Synthetic provisioning/bootstrap and archived frame/config/keybinding/shell/font
  checks passed. No existing user tmux sessions were used.
- Candidate-file and independent publication reviews found no live credentials or
  private deployment identifiers. Generated caches, bytecode and binaries are ignored.
  Markdown entrypoint links and staged whitespace checks pass.

Existing stale/permission-restricted local dependency caches failed the first runs;
clean task-specific Go/npm caches resolved them without changing dependency versions.
No full native app build, notarization or complete legacy live integration suite was
performed for this source reorganization. Historical device acceptance remains
separate from these checks. GitHub CI results are visible on the repository Actions tab.

Source publication does not redeploy the running web service or restart existing
terminal sessions.

### Public CI follow-up

The first macOS CI run used system LibreSSL, which lacks the Ed25519 verification
option. CI now installs OpenSSL 3 explicitly. Core Go tests/vet/race and web checks
passed on GitHub. The optional native surface-deck contract failed on macOS 14 even
after replacing a fixed 20ms sleep with a bounded condition wait. Native CI targets
the locally validated macOS 15 baseline; this is not a claim that macOS 14 is fixed.
