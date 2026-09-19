# Validation

This page records checks for the public source tree. Older implementation and
private-deployment checkpoints are preserved in
[the historical validation log](archive/VALIDATION_PRE_PUBLICATION.md).
They are not claims about a user's independent deployment.

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
