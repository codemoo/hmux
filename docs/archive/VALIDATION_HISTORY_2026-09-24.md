> Historical evidence through 2026-09-24. Statements of pending work describe
> their date, not current deployment status. See [current validation](../VALIDATION.md).

# Validation

This page records web/Home checks and deployment evidence for the public source tree.
Retired product history is available in Git. Local results do not establish the
state of another user's independent deployment.

## 2026-09-23 — Cached workspace startup

Release `cache-20260923T143731Z` deployed the gateway/web assets and the local Home
service. Both binary hashes and all gateway assets matched staging; public asset
hashes, CSP/no-store and five anonymous API 401 barriers passed. Home published its
catalog and all 25 pre-existing tmux identities survived. Previous releases and
Home service/binary backups remain available. Home cold-start catalog readiness
still took about 14.8 seconds; this release accelerates cached browser presentation
and workspace queries, and does not claim that cold-start recovery is resolved.

Web checks, 170 web tests and agent/Home/shared-workspace race tests passed.
Chromium with synthetic routes verified cached tabs before a held state response,
zero terminal connections before catalog validation, exactly one connection after
fresh state, and no previous-account preview on account change. This is browser
fixture verification, not physical-device acceptance. Independent read-only review
identified the legacy profile-tab migration gap; a scoped fallback was added.

Read-only Home measurements compared basic metadata-preserving catalogs at
2040/28/15 ms with full catalogs at 1111/788/784 ms. The first basic read still
encountered startup/metadata overhead; these are samples, not latency guarantees.
Cached rendering avoids waiting for that backend work after authentication.

## 2026-09-23 — Initial loading timing investigation

Read-only live measurements on 25 existing sessions: basic catalog 9 ms,
process snapshot 50 ms, provider binding/state 729 ms, Codex record scans 617 ms
across 21 records, and full catalog 784 ms. The complete Home catalog path took
796/799/794 ms in three sequential reads. Synthetic local diagnostics wrote
1,000 short log records in 1.448 ms; 25 synthetic 8 MiB state scans took 525 ms.
These are local samples, not end-to-end browser measurements or disk-stall bounds.

Operational gateway requests still showed roughly 4–6 seconds. The existing
request log does not identify operation classes or split Home execution from
transport waits, so attribution of the remaining latency is unresolved. Default
(shared, unprofiled) workspace synchronization fetches the enriched catalog;
profile-scoped workspace synchronization uses the gateway snapshot instead.
No performance fix or deployment is claimed by this investigation. Live probes
print counts/timings only and require explicit environment opt-in.

## 2026-09-23 — Claude reader deployment

Release `claude-20260923T100218Z` updated the gateway and web assets, retaining the
previous release for rollback. The binary and all 47 asset hashes match staging;
public HTML/manifest/worker/JS/CSS hashes and no-store/CSP checks passed. Five
anonymous API endpoints return 401. The service is active and all 25 original tmux
session identities survived. Home binary activation is pending: the initial replacement and one retry both
timed out in automatic approval review without execution. This partial deployment
alone does not enable Claude transcript reading on an old Home.
Live authenticated Claude conversation acceptance remains unverified.

## 2026-09-23 — Claude conversation reader (local)

The reader now accepts authoritative Claude bindings and extracts public text from
Claude JSONL messages, preserving the existing identity checks and bounded reads.
Synthetic coverage checks text/string content, Markdown tables, tool/thinking
exclusion, sidechains, metadata, compact summaries, role mismatch and size bounds.
Catalog/agent race tests, vet, web checks, 166 web tests and production web build
passed. Active-Claude read-only acceptance was attempted but blocked by process
snapshot sandbox permissions; both escalation attempts timed out without running.
No live-Claude acceptance or deployment of this feature is claimed.

The user's subsequent Home service installation of the transport diagnostics build
was verified by matching the installed binary hash and a `catalog-published` log.
This completes the previously pending Home diagnostics deployment below.

## 2026-09-23 — Transport diagnostics deployment follow-up

Gateway diagnostics and cancellation isolation were deployed as described below.
The Home transport-write/catalog-publication extension is built but not installed:
both the replacement request and its one permitted retry timed out in automatic
approval review without execution. The existing managed Home remains connected;
the gateway received its first catalog. This supersedes earlier gateway deployment
status only; it does not claim Home replacement or physical reboot validation.

Diagnostics include fixed stages, process-local connection numbers, elapsed time,
request/open outcomes and duration, numeric close codes, and safe error categories.
Private rotating logs retain at most 2 MiB each. No payload, token, account/session
identifier, operation, address or raw error detail is logged.

## 2026-09-23 — Shared Home transport cancellation isolation

A local real-WebSocket fixture reproduced a gateway defect: canceling the caller
while a frame was being written closed the shared Home socket. Pending writes
still honor caller cancellation; writes already started now finish under an
independent five-second transport deadline measured after writer acquisition.
The queue has a separate five-second bound and honors caller cancellation.
This prevents browser refresh/request deadlines from canceling other views' shared
transport while preserving a bounded write budget. Completed operations are not
rolled back by cancellation.

Regression race tests cover cancellation and caller-deadline expiry during an
actual transport write, delivery of a subsequent frame, and canceled queued work.
The old implementation failed the in-flight cancellation test. This reproduces
a defect, not definitive attribution of the observed production incident.

The complete gateway/Home service race suites passed. A completed independent
read-only review identified queue time consuming the write budget; this was fixed
with separate deadlines and a real-WebSocket regression that queues for 4.5 seconds
before delaying the actual write another 750 ms. Focused race regressions, vet,
Linux amd64 and macOS arm64 builds and diff checks passed on the final code.

Gateway release `transport-20260923T030734Z` was activated with the prior release
retained for rollback. The running binary hash matches the build. HTTPS HTML,
manifest and worker hashes match retained assets; no-store/CSP headers and five
anonymous API 401 barriers passed. The gateway logged Home acceptance, hello and
first catalog readiness. The private transport log is mode 0600. All 25 original
tmux session identities survived the gateway restart. No authenticated browser
terminal or physical reboot acceptance test is claimed.

## 2026-09-23 — Home connection failure diagnostics

Home reconnect logs now classify dial, hello, heartbeat, read and catalog
collector/write failures. HTTP handshake statuses and WebSocket close codes are
recorded numerically; remote error messages, close reasons, response bodies,
addresses, credentials and terminal content are excluded. A synchronized recorder
retains the first observed worker failure before cleanup cancellation obscures it.
The existing reconnect interval and bounded, rotating lifecycle log remain.

Race tests passed for `internal/webgateway` and `internal/homeservice`. Regression
coverage includes an actual rejected WebSocket handshake, error-data redaction and
concurrent failure recording. Gateway vet, the native Home binary build and diff
checks passed. This improves evidence collection; it does not establish or repair
the underlying cause of previously observed connection interruptions.

Applying this diagnostics build to the running Home remains pending: both native
service replacement requests timed out in automatic approval review without
executing. The installed connector still uses the preceding logging behavior.

## 2026-09-22 — Native Home automatic startup

Added opt-in macOS LaunchAgent and Linux systemd user-service management to
`hmux-web`, plus installer integration. Services preserve the Home account,
configuration and allowlisted CLI environment, use no resident wrapper, retain
original tmux/provider processes and reconnect after network loss. The connector
holds a private state-directory lock. Lifecycle logs contain fixed categories
and are bounded to two 1 MiB files.

`make check` passed, including Go tests/race/vet, vendored collector checks,
ShellCheck, TypeScript/formatting and 165 web tests. `make build` and a Linux Home
connector cross-build passed. Six installer tests passed. Service tests cover
native plist parsing, unit argument escaping, original-environment adoption,
file/lock safety, bounded logging and verified termination of a disposable child
while retaining an unrelated process. The real macOS process query exposed
nonzero kernel bytes after a command name's terminating NUL; discovery now ignores
those bytes, with a regression test and a passing focused race test.

The installer, hooks and isolated session-create/provider-exit integration checks
passed. The web PTY integration could not complete under the sandbox because
`/bin/ps` was blocked; escalation requests timed out in automatic approval review
without executing. Native systemd verification requires Linux and is wired into
the new Linux CI job; it was not executed on this Mac. Independent post-fix review
found no remaining actionable service-lifecycle defect. These checks do not
establish physical reboot/login or Linux service activation behavior.

Local macOS activation remains pending. An initial adoption attempt safely refused
to stop the existing connector when command-name discovery missed it. After that
fix, activation requests timed out in automatic approval review and did not run.
The existing manual connector was not replaced. No gateway deployment is claimed.

## 2026-09-22 — Session workspaces and provider exit (not deployed)

Home installation now configures the workspace base: new installs default to
`~/.hmux`, while reinstalls preserve existing per-profile bases unless an explicit
workspace path is supplied. Explicit changes make timestamped inventory backups
and preserve legacy fields. New sessions atomically allocate safe child folders
and unique folder/profile-prefixed tmux names without reusing existing work.
New and restored Codex/Claude panes return to an interactive shell when the CLI
exits; existing running panes are unchanged.

`make check` passed, including Go unit/race/vet, the vendored collector checks,
ShellCheck, TypeScript/formatting and 158 frontend tests. `make build` passed.
Isolated tmux tests covered repeated names, concurrent folder allocation, child
symlinks, exact creation identity, literal command arguments, Unicode CWDs,
normal/error exit and both exiting and handled Ctrl+C. Recovery verified provider
exit leaves usable panes and the subsequent checkpoint clears resume bindings.
Four installer tests and a real installer CLI smoke test with disposable HOME
verified default/custom bases, backups and unchanged settings on reinstall.
Independent review's configuration-directory permission finding was fixed (0700).

All `make integration` checks passed. The web PTY integration was rerun with
permission for `/bin/ps`; it verified resize/redraw, grouped-view cleanup and
original-session survival. Initial approval-service failures did not modify
production. Deployment evidence will be recorded after activation. No additional
browser/device-input validation is claimed.

## 2026-09-22 — Web-only product and host services

Removed desktop app sources/bridge, terminal selector/frame archive, SSH client
provisioning, controller/update stack, installers and their dedicated tests/CI.
The web connector now calls local `internal/home` services. Grouped-view markers,
exact session identities, catalog observation, usage sources, upload reception,
workflow hooks and recovery remain. Known old Home configuration is decoded
without activating old transports; profile-only inventories are supported.

Go unit/race/vet, the vendored collector's unit/race/vet, ShellCheck, TypeScript,
158 web tests and the production web/Linux gateway/macOS Home builds passed.
Config tests cover legacy loading, new-file precedence, unchanged private files,
profile-only inventory and invalid-field/path/role rejection. Four isolated
installer tests cover backups, preservation and source/target safety refusals.

The hook and session-create integrations passed. The grouped web PTY test passed
with permission for `/bin/ps` (its initial sandboxed run could not inspect the
foreground process); it verified resize/redraw, view cleanup and original session
survival. Isolated reboot recovery with fake Codex/Claude providers and the fake
Home WebSocket origin/logout test also passed. All tmux test resources used private
sockets and `hmux-e2e-*` names. No new physical browser-input acceptance is claimed.

Independent code/documentation review found no remaining web runtime defect;
its installation-safety and stale-reference findings were resolved. Documentation
links, formatting and remaining retired-product imports were checked. Build/test
caches were redirected to temporary writable directories where sandbox policy
prevented using the user's cache directories.

Production release `20260922T050103Z` deployed the web assets, Linux gateway and
Home connector/helper built from `7bf8934`. Existing Home binaries received private
timestamped backups. One new connector is established; original tmux session IDs
and creation times survived the handoff. HTTPS asset hashes, no-store/PWA CSP,
anonymous API rejection, gateway health and private VAPID storage were verified.
GitHub source publication completed; this rollout did not alter user configuration
or authentication/profile stores.

## 2026-09-22 — Mobile accessory key labels and focus

The mobile key row no longer includes the redraw button; the top toolbar retains
it. Buttons use content-based widths so `Ctrl+C` stays readable at narrow widths.
Accessory touch handling preserves the terminal textarea focus, activates once
on a completed tap and suppresses duplicate compatibility clicks. Swipes,
canceled touches and multitouch do not send keys. Mouse/keyboard activation and
the separate attachment picker remain available.

Web check, 158 tests and build passed. Production-bundle Playwright checks in
WebKit/iPhone and Chromium/Pixel emulation verified 320/360/390/430/700px layouts
without clipped labels or row overflow. Esc, Tab, Ctrl+C and all arrows sent the
exact bytes once per tap; latched Ctrl plus typed c sent one interrupt and reset.
Neither browser recorded a textarea blur during these key interactions. The
keyboard viewport was simulated; physical-device OS keyboard behavior remains
separate from these automated focus checks.

Frontend release `20260921T194735Z` changes only web assets. HTTPS asset hashes,
authentication barriers, no-store and PWA CSP are verified after activation;
the gateway and Home stay running.

## 2026-09-22 — Bounded native input and wider usage details

The native pending-input preview now wraps across the full terminal width after
its first line. A contained layer starts at the cursor row; long runs scroll only
inside that local area, leaving earlier output and the page viewport in place.
The existing textarea follows resized terminal geometry, and its iOS Paste target
cannot extend past the right/bottom edges. One local caret replaces xterm cursor
decoration while pending. Echo copies contain no caret and correctly reconcile
wide-glyph wrap padding. Native composition/deletion and boundary sends are unchanged.

The usage dialog is up to 1180px wide on desktop, with Codex/Claude columns,
larger overview gauges and aligned account rows. Mobile stacks the same content;
source selection, quota freshness, missing-five-hour rules and scroll retention
remain intact.

Web check, 155 tests and production build passed. Chromium and WebKit real-xterm
fixtures exercised both native bridge modes, 1–1000 Hangul characters at the
right/bottom edge, deletion, flush/cancel/blur, scrollback, resize and wrapped echo.
WebKit also checked the production bundle, gray truecolor cells, theme changes,
first-line indent/full-width wrapping, and bar/block/underline cursor cleanup.
Screenshots were inspected. Chromium usage fixtures verified six account rows,
stale/missing quota handling, live refresh, single-provider/CLI views and widths
320/390/768/1024/1440. These synthetic browser checks are not physical iPhone or
macOS Safari IME acceptance.

Frontend release `20260921T193447Z` updates only web assets; the gateway and Home
processes remain running. HTTPS asset hashes, anonymous API barriers, no-store
and PWA CSP are verified after activation.

## 2026-09-22 — Restrained graphite and monochrome branding

Follow-up visual refinement replaces the two-color block logo with a single-color
bracket/H mark, neutralizes blue-tinted surfaces and lowers accent saturation.
The default HMux Dark terminal palette is similarly restrained; the four pinned
third-party palettes remain unchanged. Selected tab borders and focus indicators
stay visible. New versioned PWA icon paths keep the same installation identity.

Web check, 151 tests and build passed. Chromium rechecked five-theme switching,
persistence, existing/new terminals, no reconnect on theme changes, settings
keyboard navigation, four viewport widths and Korean Regular/Bold metrics.
Desktop/mobile settings, usage and workspace screenshots were inspected.
Frontend-only release `20260921T191051Z` retains the gateway/Home processes and is
verified through HTTPS asset/icon/font hashes, authentication barriers and PWA CSP.

## 2026-09-22 — Cool workspace design and terminal themes

The web UI now uses cool charcoal, blue/cyan accents, compact corners and flat
SVG/PWA branding. Settings has keyboard-accessible categories. Terminal themes
(HMux Dark, Tokyo Night Storm, Catppuccin Mocha, Dracula, Nord) persist per device
and update existing/future xterms without reconnecting or resetting output.
Pinned upstream palette licenses and the unmodified Pretendard Variable v1.3.9
UI font are bundled. Monatendard Regular/Bold remain the terminal fonts.

Web type/style checks, 151 tests and the production build passed. Independent
read-only review found no material defects; its viewport-variable cleanup was
applied before release. Chromium fixtures verified all five themes, existing/new
tabs, saved selection after reload, unchanged connection count on palette changes,
settings keyboard navigation, login, account/usage panels, Markdown tables/code,
and 320/390/768/1440px layouts. Regular/Bold measured ASCII 8.32999px and Hangul
16.65999px at 14px. Font-file audit confirms all 11,172 modern Hangul syllables;
UI font loading and source hashes were also checked. These are automated browser
checks, not new physical Safari/iOS/Android IME acceptance. Existing accepted
input/clipboard/connection mechanics are preserved.

Frontend-only release `20260921T185703Z` uses an atomic assets switch, retaining
Home and gateway processes. HTTPS asset/font/icon hashes, anonymous API barriers,
PWA CSP and gateway health are verified after activation. Installed launchers may
update the new icon on their own schedule; no forced reinstall/reload is added.

## 2026-09-22 — Restore codex-lb account details

Codex-lb uses `lastRefreshAt` for OAuth credentials. The web panel incorrectly
used that field as the quota freshness timestamp, hiding all six observed account
rows even though their exported list was less than one minute old and all had
weekly quota/reset data. Account details now use the existing account export's
`accounts_updated_at`. Independent pool refreshes cannot freshen an old list,
and expired/missing/invalid list timestamps keep details unknown. Cswap retains
its own per-account observation behavior. Account-list freshness is not a claim
about the age of the provider's underlying quota measurement.

Web type/style checks, 149 tests and production build passed. Chromium verified
production assets with old OAuth timestamps and recent account observations:
account gauges, Plus/Pro labels, resets, mixed five-hour availability, source
switching, saved visibility, and desktop/mobile-width layout. Regression tests
also cover an expired list with a fresh pool and a fresh list with a failed pool.
Release `20260921T182950Z` updates web assets only, retaining the gateway and Home
processes and the terminal flow-control fixes. HTTPS hashes and authentication
barriers are verified after the atomic release switch.

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
production build and the isolated tmux view lifecycle test.
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

## 2026-09-24 — catalog and browser performance follow-up

Implemented identity-only resume binding (both authority passes retained), stream-owned
async host metrics and periodic recovery checkpoints, lock-free committed recovery
reads with private-directory validation, lazy initialization of restored terminal
tabs, and incremental usage-dialog reconciliation. Independent read-only review
reported no material findings.

- `make check`: passed, including Go race/vet, collector tests, ShellCheck and all
  170 browser unit tests. Initial sandbox run was interrupted while socket tests
  stalled; the complete run in the normal host environment passed.
- `make integration`: passed using isolated disposable tmux resources.
- `make build`: passed using a writable temporary npm cache; the first attempt
  could not write the default cache. `git diff --check`: passed.
- Chrome with synthetic authenticated routes: 32 cached tabs restored while live
  state was held; zero premature terminal connections. After verification, one
  terminal and one connection; selecting a second tab created exactly one more
  terminal. Unchanged usage updates produced zero DOM mutations; metrics and
  timer updates preserved the mounted panel. Screenshot reviewed. These are
  desktop synthetic checks, not physical mobile/IME validation.
- Gateway/web release `perf-20260923T151121Z` and native Home deployed. All 47
  public asset hashes and both installed binaries verified; no-store/PWA CSP and
  five unauthenticated API 401 responses verified. Gateway active with zero
  automatic restarts; Home running. All 25 pre-deployment tmux identities survived.
- First Home catalog publication observed at 7,417 ms, versus the previous
  deployment's approximately 14,800 ms. This is one observation, not a controlled
  end-to-end latency benchmark. Recent request durations mostly 70–89 ms, but
  one 5,037 ms request remains in the deployment window; no claim that every
  latency source has been eliminated. Initial recovery still precedes publication.

## 2026-09-24 — latency attribution

Added allowlisted operation names and gateway send duration, Home processing/send
spans, startup recovery and recurring catalog fetch/publication spans, and slow
process/open-file/resume/conversation spans. No request identifiers or payloads
are logged. Logging uses the existing bounded sinks and adds no resident worker.
Focused Go tests, operation/redaction regression tests, vet and both native
binary builds passed. Browser assets and input behavior were unchanged.

Latency attribution deployment: `timing-20260923T152524Z`; gateway and native Home
updated. Full focused-package race run completed successfully (gateway 131.651 s);
a separate targeted logging race run also passed. Web check/build and 171 unit
tests passed after provider-aware conversation loading UI was added. Chrome
synthetic held conversation response rendered the Claude loading card at a
390×844 viewport; screenshot reviewed, Codex/unknown labels covered by tests.
Initial production observation: recovery sync 1,310 ms, catalog fetch 4,720 ms,
catalog publication 2 ms. These overlapping spans attribute the startup delay to
Home collection rather than frame transmission; deeper collection attribution
remains to be measured. Workspace requests observed at 61–171 ms, send_ms=0.
