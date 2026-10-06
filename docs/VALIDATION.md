# Validation status

Evidence summary updated 2026-10-06. Deployment observations describe one maintained
installation, not every HMux deployment. [Rust runtime status](RUST_MIGRATION.md)
owns the remaining acceptance queue; [Rust verification](../tests/RUST.md) owns
runnable checks. Detailed investigations, earlier CI failures and per-change
verification remain in the [dated record](archive/VALIDATION_HISTORY_2026-09-26.md).

## Source and automated checks

The native admission/recovery checkpoint is `7943059`. Gateway, Home, helpers,
service management and installation use Rust; web/PWA remains TypeScript. The
retired Go source and its validation remain in Git history and the dated record.

| Scope | Recorded result |
| --- | --- |
| Native admission/recovery | Full release workspace: 684 tests passed, 21 opt-in tests ignored; final refinements: 256 Home, 22 Gateway HTTP and 23 transport tests passed |
| Concurrent terminal startup | Eight overlapping starts leave action capacity available; both codecs passed on macOS and Linux |
| Native/static checks | Rust formatting, strict Clippy and ShellCheck passed |
| Web at native checkpoint | TypeScript, formatting, 192 tests and build passed |
| Web keyboard restoration (`ea136b5`) | TypeScript, formatting, 198 tests and production build passed; synthetic Chromium viewport checks described below |
| Release artifacts | macOS ARM64 and Linux AMD64 bundles passed all five packaging checks |
| Isolated runtime | Production-binary login, catalog, terminal ACK and reconnect passed; macOS WSS reconnect/signal/private-input checks passed with both codecs |

The prescribed two-thread Linux Gateway rerun passed after unrestricted test
concurrency hit the process-wide authentication startup limit. The stopped local
parallel build resumed with two build jobs. These are automated fixture results,
not physical-device or long-running stability acceptance. Earlier CI outcomes
are checkpoint-specific; see the dated record rather than inferring current CI
status from a prior local result.

## Guarded Codex status repair (2026-10-06)

Unavailable conversation discovery can now send one fixed `/status\r` to a stable,
foreground Codex pane with a recognized empty composer. A fresh status-panel UUID
is matched against a bounded, no-symlink rollout scan and validated headers/inodes.
Drafts, busy/approval screens, stale displayed status, copy mode, queued input,
changed identities and duplicate records fail closed. Saved repaired links are
reader-only and conditional writes preserve concurrent administrator links.

Independent review found and corrected a long-held input gate, the Linux `state`
versus `stat` foreground flag difference and an inherited 4 KiB capture ceiling.
The final send gate has a shared 350 ms deadline; process/file discovery and the
two-second status wait do not hold it. New HMux input abandons later repair. Captures
have an explicit 64 KiB cap. External native typing and same-process thread switches
remain limitations, as described in [Web](WEB.md#conversation-reader).

All seven focused checks passed on macOS, including an opt-in real tmux check on a
disposable isolated socket with a synthetic provider graph/raw-mode TUI. It verified
exact `/status\r` bytes, the status UUID link and expired lifetime rejection. Other
regressions cover >4 KiB screens, responsive input during delayed status output,
cooldown, cancellation, prompt/identity rejection, duplicates, symlinks and headers.
This is automated/synthetic-provider evidence, not actual Codex or browser-device
acceptance. Strict Home/helper Clippy, formatting and optimized native builds passed.
Independent follow-up review found no remaining material defect.

The final two-thread Home/helper regression passed 327 tests with ten opt-in or
external checks ignored. The separate opt-in status/tmux check above was executed.
An initial unbounded-concurrency run failed the existing synthetic refresh test
with a command error; the prescribed two-thread run and final rerun passed. This
is targeted Home/helper validation, not a new full-workspace/Linux/device run.

The maintained macOS Home/helper were deployed with native binary backups and
rollback support. The connector reconnected; both installed hashes matched the
build, configuration and LaunchAgent bytes were unchanged, and all 33 original
tmux lifetimes and three existing link records (including notification permissions)
were preserved. The installed helper read the existing selected Codex conversation
with `linked` attribution. The portable macOS foreground check returned true.
Gateway PID/restart count stayed unchanged; public HTTP 200 and anonymous session
HTTP 401 checks passed. Gateway/web assets were not replaced. A real Codex automatic
status-repair failure flow and physical-device/PWA behavior remain unverified.

## Pinned Codex completion notifications (2026-10-03)

Administrators can separately opt a selected conversation into completion detection
with `conversation-link --notify`. Older/default links stay reader-only. Generation
and provenance establish fresh baselines; pre/post validation rejects changes to
session lifetime, pane, provider PID/start stamp, permission/generation or transcript
identity. Automatic exact/ambiguous bindings are never overridden. The source is
explicitly pinned: same-process thread switches still require unlink/relink.

Rust formatting, strict Home/helper Clippy, 320 Home/helper tests and optimized
native builds passed with Rust 1.88. Nine opt-in/external/device tests were ignored;
this is not acceptance of those gates. Synthetic regressions cover historical
suppression, running baselines, deduplication, opt-in/relink and identity changes.
Peer integration verifies notification transport in both JSON and protobuf codecs.
Initial fixture runs exposed an undriven test runtime and unhandled WebSocket
heartbeat; both were corrected before the passing full run. Independent review
found no material defects.

The maintained macOS Home/helper installation was updated with backup/rollback
support. The connector reconnected, binary hashes matched, configuration and all
33 pre-existing tmux lifetimes were preserved, and the selected reader remained
available with notification permission enabled. Gateway and web assets were not
replaced. Actual OS/PWA receipt of a subsequent real completion remains unverified.

## Conversation reader recovery

The web reader now retries the same exact tmux identity up to three times within
45 seconds for transient network/deadline failures, HTTP 429/503/504 or an
`unavailable` conversation binding. It shows progress and offers retry/return
controls after failure. Ambiguous, malformed, authentication and protocol failures
stop immediately; HTTP 502 is not replayed because Gateway also uses it for
invalid protocol responses. Long/invalid server backoffs stop the foreground cycle.
Tab/reader/account cancellation covers active requests and delay timers. Every
retry uses existing Home discovery. Missing daemon thread associations now also
have the guarded status repair described below; directory/file recency is never used.

Web TypeScript/formatting, 209 tests and production build passed. Regression tests
cover finite attempts, nested-retry avoidance, account expiry, cancellation/late
responses, browser AbortError-to-timeout preservation, overall deadline, backoff
and invalid payloads. Production assets in an isolated Chrome browser with
synthetic API/WebSocket responses passed a busy→missing→ready recovery, persistent
failure stopping at three reads, manual retry and closed-reader late-response
checks. The browser's synthetic 503 console entry was expected. Initial browser
launch attempts failed on sandbox/cache permissions before the isolated run.
Independent review identified and removed HTTP 502 replay. This is web-only
validation; physical devices and live authenticated failure recovery were not tested.

The web-only production rollout passed 47 public asset hash checks, five anonymous
API authentication checks and PWA CSP/no-store checks. Gateway process identity and
restart count stayed unchanged; Home was not restarted and existing tmux/provider
work was untouched. These deployment checks confirm asset delivery and access
boundaries, separately from the synthetic browser recovery checks above.

## Explicit Codex conversation links

Shared-daemon Codex clients can lack a client-owned rollout descriptor. The reader
now supports an administrator-selected, visibly labeled transcript link, without
using cwd/newest-file guesses or changing provider execution. The 4 MiB tail and
public-output limits are retained. Process start checks are best-effort, as described
in [Operations](OPERATIONS.md#administration); switching threads in the same CLI
still requires relinking.

Automated macOS checks passed: 12 conversation peer tests across JSON/Protobuf,
private-link identity/inode/symlink/storage-cap checks, and a CLI integration test
covering create/read and unlink after tmux closure. Automatic bindings take
precedence and ambiguous bindings never expose linked text. Rust formatting and
strict Clippy passed for Home/helper targets. Web TypeScript/formatting, 199 tests
and the production build passed. Independent review of the lifecycle/storage
corrections found no remaining material issue. These are targeted checks, not a
new full-workspace, Linux-runtime or physical-device acceptance run.

The matching web assets and macOS Home/helper were deployed to the maintained
installation. All 47 web asset hashes, anonymous authentication rejections and PWA
headers passed; Gateway process identity was unchanged. The macOS bundle passed
five packaging checks. The installed helper returned 25 recent messages from the
explicitly selected live Codex thread, with `linked` attribution; conversation
contents were not copied into verification logs. Original tmux identities, the
provider process and configuration were preserved. Catalog and usage publication
were observed after Home connected. Authenticated browser rendering is not yet
user-confirmed.

The first rollout readiness check rejected launchd's transient `xpcproxy` process;
it did not complete an automatic rollback. The same PID subsequently executed the
verified binary and connected. Manual checks confirmed one LaunchAgent run,
expected binary hashes, preserved state and live conversation output. High host
load and pre-existing reconnect delays were observed separately; this conversation
change does not claim to resolve those broader latency symptoms.

## macOS Home scheduling under load

A subsequent live outage investigation found Gateway still running and serving
HTTP 200 while the Home connector repeatedly disconnected. The Home LaunchAgent
used `ProcessType=Background`; on a heavily loaded host, it remained runnable with
little CPU time and catalog publication took 15 seconds. macOS documents this
classification as CPU/I/O throttled. The live plist was backed up and changed only
to `Interactive`, preserving its arguments, environment, process-group policy,
configuration and original tmux sessions. Home connected immediately and initial
catalog publication fell to 316 ms. With host load still above 90, the same
connection subsequently served a terminal open in 55 ms and workspace requests
in 21–27 ms without another reconnect during the observation. This is a live before/after observation, not a
controlled benchmark or a claim that every connection failure has this cause.

The service template now emits the same classification. Service tests passed
(27 tests, two opt-in tests ignored); the native-manager opt-in test was not run.
The immediate repair changed the installed plist and restarted only Home; native
binaries and Gateway were unchanged. Older installed binaries still contain the
previous template until updated, so reinstalling with one can restore Background.

## Mobile keyboard restoration

A synthetic browser reproduction found that xterm could expand during Home view
startup while its resize event was skipped before `ready`. FitAddon then saw no
new local change, leaving Home with the smaller opening dimensions. The browser
now explicitly sends its fitted size after `ready`. After mobile keyboard
dismissal settles, it resends size and requests one redraw of the same active
view. A tab switch, reconnect, dialog, background transition or keyboard reopening
invalidates a pending redraw. Existing tmux sizing policy and input handling are
unchanged.

At 390×844, the baseline reproduction expanded xterm to 67 rows while Home had only
received the 31-row open frame. The production web build sent the corrected 67-row
size after ready. Synthetic Chromium checks modeled iOS overlay and Android
content-resize viewports, retained input focus, and a late final viewport height
without its own resize event. Both returned to the expanded size and sent one
settled redraw without another connection. Regression tests also cover stale tab,
generation, dialog and reopening transitions. Independent review caught and fixed
an ownership transfer while the keyboard was still visible. Initial browser
harness errors were corrected before these passing runs; they were not app failures.
These checks do not establish physical-device acceptance. The web-only update
did not rebuild native binaries or rerun the native suites above.

## Maintained deployment

The native admission/recovery update remains deployed on the maintained Linux
Gateway and macOS Home. Its initial checks verified catalog/usage publication,
singleton ownership and preservation of configuration and original tmux identities.
The detailed rollout observations remain in the dated record.

The keyboard restoration web build was subsequently deployed by an atomic release
switch. All 47 current public asset hashes, five anonymous API rejections, PWA CSP
and no-store headers passed. The Gateway process identity, start time and restart
count were unchanged. Native files and live configuration/account/session stores
were retained; Home was not restarted. Prior hashed web chunks remain available
to already-open pages. This is initial deployment verification, not a soak result.

Private deployment checks verified initial connectivity, catalog and usage
publication, configuration preservation, original-session preservation and
rollback readiness. These checks are separate from local builds and automated
fixtures. Deployment timestamps, installed binary hashes, release identifiers,
process/session identities, personal configuration and raw logs are kept out of
public documentation.

Binary rollback must retain current configuration, credentials, account policy
and session state; restoring an older state snapshot can revive revoked access.
See [Rollback](ROLLBACK.md). Initial connectivity does not establish physical
reboot/login, full browser/TOTP or long-running acceptance. Authenticated browser
rendering of the latest metrics change remains unverified.

## Browser and device evidence

- Synthetic Chrome checks covered cached workspace before live state, no premature
  terminal connection, account isolation, 32 restored tabs with one initial xterm,
  initialization on selection and unchanged usage polling without DOM mutations.
- A Claude loading card was reviewed at 390×844 with a held synthetic response.
  Codex and neutral fallback labels have regression coverage. Returned-message
  provider attribution remains server-owned. Live authenticated Claude conversation
  acceptance has not been recorded.
- Device-confirmed input behavior and accepted limits remain in
  [browser input](BROWSER_INPUT.md#accepted-mobile-behavior) and [iOS input](IOS_INPUT.md).
  Responsive emulation does not establish physical Safari/iOS/Android behavior.

## Resource and latency evidence

The [deployed-artifact memory comparison](../bench/hmux/README.md#deployed-artifact-memory-comparison-2026-09-25)
records isolated Go/Rust workloads and separate live Rust readings. Rust used less
Linux PSS in that synthetic comparison; no matched live Go sample or whole-product
memory-budget acceptance is claimed. README figures retain those measurement bounds.

Earlier catalog/startup and intermittent-request observations are preserved in
[the dated record](archive/VALIDATION_HISTORY_2026-09-25.md#performance-evidence-and-unresolved-work).
They are historical samples, not current Rust latency benchmarks. Neither every
latency source nor every connection interruption is claimed resolved. Further
device, service, soak and resource acceptance is tracked only in
[the runtime status](RUST_MIGRATION.md#follow-up-acceptance-limits).
