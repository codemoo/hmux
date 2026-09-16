# Historical validation before source publication

This is historical evidence, not current deployment instructions.

# Validation

Native source candidate: VERSION 0.1.42; this is not a published native release.
Current web deployment and device acceptance are recorded in
[Current web verification](#current-web-verification-2026-09-09). Older machine-specific claims have been retired to a
short [historical note](VALIDATION_THROUGH_0.1.39.md).

## Reproducible gates

```bash
make check
make build
macos/HMux/scripts/build.sh
macos/HMux/scripts/package.sh
```

| Gate | Coverage |
| --- | --- |
| fmt/test/race/vet | Go core and embedded usage module |
| shellcheck/shfmt | Repository shell and bootstrap syntax/style |
| integration | Fake SSH/config provisioning; isolated hmux-e2e tmux/PTY/frame tests |
| native-smoke | Swift models, workspace persistence, native view lifetime across tab switches, WSS bridge, usage, staging, child lifecycle and restart readiness |
| build | Configured Go macOS/Linux architectures |
| native build/package | Full SwiftUI/Ghostty Release target; bundle signature and ZIP round trip |

Prerequisites include Go, shellcheck, tmux, fzf, jq, Python, zsh and expect;
native checks/builds require macOS Swift/Xcode. Dependency downloads may be
needed. Tests never attach to, rename, detach or terminate pre-existing tmux
sessions. Every live fixture uses an isolated socket and hmux-e2e names.

The native catalog smoke builds this checkout's Go helper and uses temporary
config plus a fake empty tmux catalog. It verifies Swift/Go bootstrap, exact
certificate pinning, first snapshot and clean shutdown without requiring an
installed app or private configuration.

Global skill runner tests belong to that separately installed skill. They are
not part of HMux's default gate because its API/version can change independently.
HMux retains its own hook merge and workflow-report ingestion tests.

## Previous UI baseline validation

Recorded on 2026-09-08 on macOS 15.6.1 arm64, Go 1.26.2, Apple Swift 6.0.3
and tmux 3.5a.

| Command/check | Observed result |
| --- | --- |
| `make check build` | Passed; selector focus PTY fixture explicitly skipped because interactive fzf was blocked by the execution sandbox |
| Isolated real tmux creation | Passed: detached named reuse keeps its identity; two blank-name requests create separate sessions |
| Isolated real tmux attachment | Passed: direct handoff, shared attachment and grouped-view closure preserve original sessions, clients, panes and options |
| Native catalog smoke | Passed with zero sessions using the checkout's helper and temporary configuration |
| Workspace state smoke | Passed: exact order/selection restoration, empty layout, source mismatch, recycled/ambiguous identities, corrupt/version/size/count rejection |
| Workspace source guards | Passed deterministic hashing, bounded/cancelled fake SSH resolution, required binding validation, and missing/matching/changed binding cases across all six terminal/mutation/staging commands |
| Swift catalog source acceptance | Passed predicate cases for missing, malformed, initially valid, matching and changed sources; Store uses this guard before applying a snapshot or marking Connected |
| Full Swift target typecheck | Passed `swiftc -typecheck` over the pinned Ghostty target with current HMux overlay sources and cached dependencies; upstream Sendable/Sparkle warnings remain |
| Full native Release build | Passed through the user-run build script after the execution environment blocked agent-launched Xcode; ad-hoc signature verified |
| Native package | Passed: source digest, signature, arm64 bundle, helper capabilities and ZIP extraction round trip |
| Independent code/documentation review | Modernization and continuity reviews completed; missing-binding and unverified-Connected findings corrected and re-reviewed with no remaining material findings |

Agent-launched Xcode was blocked by SwiftPM's nested sandbox and repeated
approval-review service failures. The user then ran `build.sh && package.sh`
successfully (exit 0, about 54 seconds). The UI baseline ZIP SHA-256 was
`00c2f0dcc27c0e430ecb3f9f86cc3ea3c12fd2f7717aaab85d3bbb1d90f0a7c4`.
The user launched that bundle and reported that it generally worked, then
reported catalog state/mutation and usage issues covered by the follow-up
below. This baseline artifact does not contain later source edits.
Upstream Ghostty umbrella-header, Swift Sendable/Sparkle and linker debug-symbol
warnings remained; the build was not warning-free.

No installed app, user configuration, remote host or pre-existing tmux session
was changed by this validation. No release was published. Optional staticcheck,
govulncheck and separate security scanners were not run.

## UI polish and icon verification

The 2026-09-08 UI pass adds a terminal-aligned tab strip, distinct Open/New
commands, filter-aware empty states, visible Reconnecting state, transport Retry,
management progress and target confirmation, persistent terminal views,
in-layout notices, usage labels and
working-directory copy. The macOS icon was replaced in all ten asset slots.

- `make check build` passed on the integrated UI revision. The existing selector
  focus PTY fixture still reports its sandbox skip.
- The AppKit surface-deck smoke performs 120 selections plus reordering,
  reconnect and close. It observed three initial native views, no remounts during
  selection/reordering, one renderer replacement on reconnect and only the closed
  renderer dismantled on close. It uses inert NSViews, never a real terminal.
- Workspace and inspector fixtures were also rendered at 680 and 1000 points.
  These use actual production layout views with a synthetic terminal and a
  placeholder usage footer. Structural notices and the compact bottom inspector
  were visually checked; this is not a real Metal rendering test.
- The full pinned Ghostty/SwiftUI target passed typechecking with the current
  overlay, including the final terminal background that follows Ghostty theme
  and dynamic color updates. Existing upstream warnings remain.
- The built-in image tool produced the icon; the corrected master and all ten
  resized slots have real RGBA transparency. Dimensions and alpha extrema were
  checked, and 32/128-pixel outputs were visually inspected.
- Current production Sidebar, QuickSwitcher and management views were rendered
  with synthetic sessions and a minimal Store double, then visually inspected in
  light/dark and compact layouts. This confirms layout only. Offscreen SwiftUI
  did not materialize Hidden Sessions List rows, so that artifact covers only the
  sheet shell/header/search. No actual Home data or terminal was used.
- Independent review of the final persistent-view, focus, notice and inspector
  changes completed with no remaining material findings.
- The user-run native build and ZIP round trip passed for this UI baseline,
  as recorded above.

## Catalog and usage follow-up

The subsequent 2026-09-08 pass addresses native grouped-view attachment counts,
alias-first A–Z ordering, alias/hide/restore consistency, and weekly usage.
These source changes are newer than the successfully built UI baseline above.

- `make check build` passed on the integrated attachment/usage revision,
  including Go tests/race/vet, shell checks, isolated tmux integration, native
  smoke checks and configured Go cross-builds. The selector focus PTY fixture
  still explicitly skips under the execution sandbox.
- An isolated tmux 3.5a fixture confirmed that an original session can report
  zero directly attached clients while its hidden native view has one; both
  correctly report one group-attached client. Catalog tests cover the group
  count, ungrouped fallback and malformed values.
- Usage tests cover pool-only and partial responses, separation from API-key
  allowances/reset times, explicit missing windows versus observed 100% left,
  alias-only account labels, empty-email transport, and Claude 429 backoff.
  Follow-up cases cover account changes during backoff and Retry-After from
  OAuth-recovery errors, including the 24-hour cap.
- `test-catalog-store.sh` embeds 17 production Store methods verbatim in an
  isolated shell with a fake backend and no terminal surfaces. Eight scenarios
  passed: rapid alias/hide restoration under input deferral, failed second
  writes, deferred external edits, recovery after three failed confirmations,
  rejection of old/equal confirmations, authoritative alias clearing on
  rollback, and recycled identities with stable row objects. A negative
  control using the previous Store compiled and failed the rapid-alias case.
  Ordering/timestamp helper tests are separate from these Store behavior tests.
- Full Swift/Ghostty target typechecking passed after the final Store fixes.
  Existing upstream Sendable/Sparkle warnings remain. Independent reviews
  identified deferred-mutation and quota/backoff defects; those findings were
  corrected with focused regression coverage.
- On the final integrated source, `make fmt-check` and
  `make test race vet shellcheck native-smoke build` passed. The earlier full
  integration/shfmt results still apply; subsequent changes were confined to
  the Swift catalog Store, its regression harness, and provider backoff state.
- A bounded read-only Home usage check confirmed that codex-lb supplied a pool
  weekly remaining value and six account rows with configured aliases and
  weekly windows. No raw email was serialized. Claude's upstream returned 429
  with Retry-After; no successful live Claude quota response was claimed.
- Production sidebar/quick-switcher views rendered with synthetic sessions in
  light/dark modes, including long aliases, natural numeric order, attached
  counts and hidden exclusion. Production usage cards also rendered with eight
  synthetic account rows. These are layout checks, not live interaction tests.
- Full native build was attempted again. Default execution failed with
  SwiftPM `sandbox_apply: Operation not permitted`; the authorized escalation
  was rejected because automatic approval review lost its upstream connection.
  After final verification and build-script scope checks, one further bounded
  retry received the same automatic-review connection failure.
  The user then ran `build.sh && package.sh` successfully (exit 0, 57.75
  seconds), including the source-digest check, arm64 bundle, ad-hoc signature
  and extracted ZIP round trip. That follow-up ZIP SHA-256 is
  `81154551fe90fe604b5f95624c328feb0239ce749586db75d21c95228e4b6d4b`.
  This is a successful local build/package, not an installation or publication.
  The user subsequently reported input and alias-sheet issues from that app;
  later corrections require another native build.

## Tab input and alias editor follow-up

The next 2026-09-08 correction hides inactive retained hosting views in AppKit,
limits window event monitors to the selected visible surface, verifies actual
first-responder assignment, and fixes hit-test coordinates for flipped content
views. Tab background geometry now includes its allocated width, and the alias
sheet closes on the matching Home write acknowledgement while catalog
confirmation continues independently.

- The catalog Store harness now passes nine scenarios, including saving while
  optimistic and stream projections arrive, successful acknowledgement without
  catalog confirmation, and a failed write retaining an editable sheet.
- The AppKit input harness verifies selected-host visibility/hit testing,
  key input through a synthetic window, environment propagation, command-key-up
  guards, and nonzero-origin coordinates. It reproduces the flipped-coordinate
  failure with the old expression. Mouse-monitor branches are mirrored with an
  injected fixture window identity, and drag delivery is manual; this is not a
  real Ghostty event-monitor or tmux interaction test.
- Four production tab items were rendered in light and dark appearances with
  short, long and Korean labels. Their visible boxes and gaps are consistent.
- Independent review found no remaining material input/alias defects after
  correcting the window hit-test coordinate conversion.
- `make check build` passed on the integrated correction, including Go
  tests/race/vet, shell checks, isolated tmux integration, all native smoke
  scripts and configured Go cross-builds. The selector focus PTY case explicitly
  skipped because interactive fzf is blocked by the execution sandbox.
- The full pinned Swift/Ghostty target passed typechecking after all corrections.
  Existing upstream Sendable/Sparkle warnings remain.
- Usage regression tests cover retry deadlines during active/stale rate limits,
  account changes, success, expiry, wire precision and post-deadline activity
  ingestion. The last case validates the actual generated transport frame.
  Optional deadline decoding remains compatible with older snapshots, and
  codex-lb account alias/weekly quota behavior is preserved. No live provider
  calls or credential changes were made during this correction.
- The corrected native app build stopped before compilation with SwiftPM
  `sandbox_apply: Operation not permitted` (exit 74). The authorized escalation
  and one bounded retry were both rejected because automatic approval review
  returned HTTP 503 while its upstream bridge was cooling down. Build scripts
  were confirmed unchanged from the earlier user-successful build. The user
  then ran `build.sh && package.sh` successfully (exit 0, 49.53 seconds),
  including arm64 bundle signing and extracted ZIP signature validation.
  That input-correction ZIP SHA-256 is
  `f13fd3bcdda75c05780808df82f4ba9628f61905d9082178eb9b4106b2bd2749`.
  No app was installed or restarted. Later theme/telemetry changes need another
  native build; they are not contained in that ZIP.

## Flexoki Dark, icon and Home metrics follow-up

The next 2026-09-08 update pins Omarchy Flexoki Dark throughout native chrome,
terminal and compatibility UI, replaces the macOS icon, and adds shared Home
CPU/GPU/RAM beside provider usage. The version remains unpublished `0.1.42`.

- Go unit tests, race tests, vet and shell checks passed on the integrated tree.
  `make check build` exposed an obsolete Light-theme expectation in
  `tests/keybindings_test.sh`; after correcting that expectation,
  `make integration native-smoke build` passed. All configured macOS/Linux Go
  cross-builds completed. The selector focus PTY case explicitly skipped because
  interactive fzf is blocked by the execution sandbox.
- The full pinned Swift/Ghostty target passed typechecking, including the new
  metrics files. Existing upstream Sendable/Sparkle warnings remain. A final
  independent review found no material defects in source ownership, capability
  compatibility, bounded collection, failure handling or Swift freshness recovery.
- Metrics tests cover fixed bounded commands, second CPU samples, memory bounds,
  missing/invalid GPU, collection failure, capability opt-in, legacy omission,
  invalid optional telemetry preserving the catalog, stale/offline states and
  recovery after a future clock observation. Tab input/drag, retained surfaces,
  alias saving, catalog ordering and provider quota regressions continue to pass.
- Both terminal configs match the pinned 16-color palette. All ten icon slots
  have transparent corners and valid alpha; small icon renders were inspected.
  Production sidebar, quick-switcher, workspace and footer components rendered
  using synthetic data. Footer layouts at 600, 660 and 1000 points include
  100% CPU/GPU values and unavailable states without truncating core metrics.
  These are layout checks, not real Ghostty input or live provider tests.
- Independent installer review identified signal-exit and interrupted-swap
  recovery defects. After correction, isolated production-cleanup fixtures passed
  failed replacement rename, TERM before/after swap, installed validation failure
  and successful backup retention. No fixture wrote `/Applications` or launched
  an app. Source and package digests are checked before any local install.
- A bounded metrics-only live sample was unavailable under the execution sandbox:
  `top` execution and `sysctl hw.memsize` were denied, and IORegistry supplied no
  usable result. No live resource percentages are claimed; no tmux sessions or
  provider credentials were accessed for this sample.
- The native build stopped with SwiftPM `sandbox_apply: Operation not permitted`
  (exit 74). Two authorized escalation attempts were rejected because automatic
  approval review lost its upstream websocket connection. Build, preparation and
  packaging scripts were verified unchanged from the earlier user-successful
  build. The user subsequently ran the full build/package/install command successfully
  (exit 0, 60.0913 seconds). HMux 0.1.42 was installed and launched at
  `/Applications/HMux.app`, including signature and extracted-archive validation.
  The ZIP SHA-256 is
  `dc2eb30cba185bbd52cc54bcf6c8df964c746cae708dd9b8955c4adf9e919d09`.
  Existing upstream header, Sendable and symbol warnings remained nonfatal.
  Later conversation-view changes require a new build.

To finish from an ordinary local terminal at the repository root:

```bash
macos/HMux/scripts/build.sh && macos/HMux/scripts/package.sh && scripts/install-hmux-app.sh --local --system
```

## Conversation, compact chrome and cswap follow-up (2026-09-09)

This records the agent’s checks at the time of the change. The user took over
manual build/install afterward; a later installed bundle has not been verified
in this record.

- Integrated the exact-session Codex public-conversation reader, SSH/app bridge,
  retained-terminal reading view, stable native search field, and fixed tab geometry.
  Tabs moved into the compact toolbar; redundant title and tab glyph were removed.
  Footer control height is 22 pt with equal 5 pt top/bottom insets.
- Added read-only cswap roster/cache integration. Synthetic tests cover live
  identity matching, account switch, reused-slot rejection, stale/expired cache,
  passed quota reset, cswap config-path precedence, email labels, Codex email
  exclusion and ambiguous active flags. Native cache reads are throttled to 2 s.
  No real conversations, account files or provider credentials were dumped or used
  as test fixtures.
- Go tests, race tests and vet passed. Native smoke suite passed. Swift full-source
  typecheck passed with existing upstream warnings. Search smoke actually focuses
  the AppKit editor and checks stable split/window bounds with Korean/long input.
  Synthetic reader previews were inspected at 600 and 1000 pt widths.
  After removal of the tab glyph, all three selected-tab renders remained 588×42 pt.
- Integration and cross-platform Go builds passed on retry. The first tmux resize
  test observed an intermediate split size and failed; the focused retry and full
  integration retry passed. Interactive fzf focus remained a sandbox PTY skip.
- Conversation parser/transport received independent review. The JSON boundary
  now reserves envelope/newline space and rejects oversized complete envelopes.
  Swift poll lifecycle was source-reviewed; its smoke cancellation case is only
  a stub cancellation check, not live tab-switch acceptance.
- The agent’s native build attempt was blocked by nested SwiftPM sandbox restrictions.
  The authorized escalation was rejected because the automatic approval review
  stream disconnected before completion. Packaging and /Applications replacement
  were not run by the agent for these changes.
  Current native toolbar positioning and real-account UI need acceptance after
  the new app can be built and installed.

### Conversation toolbar follow-up

Reading and all-tabs controls now share one native toolbar item with a 2 pt
internal gap. The reader no longer rejects requests based on a cached runtime
label: selected-tab session identity is sent to Home and resolved against tmux
and the current Codex process. Conversation smoke and focused Go conversation
checks pass. This source change requires a new app build; installation was delegated to the
user and is not claimed as verified here.

## Separate acceptance

A local build/smoke does not prove real remote or physical-device behavior.
For a live release, record release/source digest, OS/tool versions, logical
host label, exact disposable resources, assertions and cleanup. Keep private
addresses, users, key paths, credentials and pane content out of reports.

Remote SSH handoff, grouped-window sizing on actual devices, interactive
native create/reconnect/restore/focus transitions, and physical iPhone Termius
import/Vault/attach require separate live acceptance. In particular, source
predicate tests and code review do not replace visible UI acceptance of rejected
catalogs, retained last-valid content and the subsequent Offline transition.
State Termius's achieved level explicitly.

The selector focus test can report a sandbox PTY skip; record that as skipped
rather than fully verified. Optional staticcheck/govulncheck/security tooling
must likewise be reported as not run if unavailable.

## Shared tmux/provider binding (2026-09-09)

- The original tmux session’s active window/pane is authoritative. Tabs retain
  existing tmux attachment behavior; the proposed per-surface token design was
  removed before delivery.
- Catalog and conversation now share provider/process/record selection and safe
  opening. Synthetic coverage includes main + subagent descriptors, multiple
  mains, malformed/partial headers, header/filename mismatch, custom roots,
  symlinks, competing providers, cswap profiles and duplicate Claude registries.
- Read-only live acceptance (`HMUX_RUN_SESSION_BINDING_READ_TEST=1 go test
  ./internal/catalog -run TestReadOnlyLiveSessionBindings -v`) observed 22 active
  tmux panes: 19 Codex associations ready and 1 unavailable. No Claude provider
  was present in those active panes. Only aggregate counts were reported; no
  attach, rename, detach or kill operation ran.
- A follow-up read-only run also confirmed 19 stable Home-only resume references
  using the two-pass shared resolver. Counts remained 22 active panes, 19 Codex
  ready and 1 unavailable; no active Claude provider was available.
- Branch-depth and truncated-tree regressions now fail closed. Catalog metadata
  uses the safe opener too; bounded lsof exit-1 partial results do not discard
  healthy processes when another PID vanishes.
- Go tests/race/vet, format/shell checks and cross-platform builds passed before
  the recovery extension. Recovery-specific final verification is recorded below.


## Home reboot recovery (2026-09-09)

Automatic empty-server startup follow-up: recovery/client/agent race tests passed,
including existing-session preservation, catalog read failures and detached shell
creation. The isolated live test now also checks that same-boot deletion stays
empty and a later reboot creates only a default shell. This updated live test
has not run: automatic approval review failed with a disconnected upstream
connection. The live results and native package results below predate this
follow-up; they do not validate the new fallback.

- Shared Home checkpoints are used by both local app catalogs and remote agent
  streams. The original tmux identity remains the tab authority; only verified
  Home recovery lineage can reconnect a new lifetime.
- `go test ./internal/recovery` passed with synthetic boot transitions, refreshed
  provider IDs, fixed resume argv, metadata/lineage, topology limits, malformed
  layouts, duplicate identities and private-file checks.
- Opt-in `HMUX_RUN_RECOVERY_TMUX_TEST=1 go test ./internal/recovery -run
  '^TestRecoveryWithIsolatedTmuxAndFakeProviders$' -v -count=1` passed in 2.79s.
  It used only a dedicated `hmux-e2e-*` socket and fake Codex/Claude executables.
  Verified two windows/three panes, active window, both explicit resume commands,
  alias, old-to-new identity, same-boot idempotence and intentional delete-all.
  No pre-existing sessions or actual provider processes were changed; Home was
  not rebooted. Actual provider login/trust and physical reboot acceptance remain
  unverified.
- Swift workspace and catalog-store smoke tests passed, including reused IDs,
  ambiguous lineage, persisted selection, and retry after a blocked reconnect
  without a new catalog. Native app building is validated separately below.
- Final `go test ./...`, `go test -race ./...`, and `make fmt-check vet
  shellcheck shfmt-check build` passed. The earlier unchanged bundled usage
  dependency tests/race had passed in the shared-binding gate.
- Native smoke scripts passed: app config, models, mutations, catalog store,
  workspace, surface deck/input, search, conversation, catalog stream, usage,
  host metrics, file staging, backend lifecycle and restart readiness. The WSS
  catalog smoke needed an unsandboxed retry for loopback access; it then passed
  with a fake empty tmux catalog.
- `macos/HMux/scripts/build.sh` passed and the completed `HMux.app` passed
  `codesign --verify --deep --strict`. Xcode required an unsandboxed build for its
  nested sandbox/package tooling. This is a local ad-hoc build, not notarization
  or installation. The installed app and Home agent have not been replaced.

- The recovery acceptance was rerun after review fixes and passed in 2.88s. It
  now injects a process-death equivalent immediately after tmux creates a session
  but before the identity is returned/persisted; the next sync recovers the owned
  temporary session and resumes each fake provider once. Repeated manual restores
  preserve lineage. A unit test rejects a pane replaced at the same position.
- Construction now uses a durable private intent and temporary session name,
  gates all panes until topology/identity commit, and verifies exact pane IDs
  before release and final checkpoint. A matching name alone is never adopted.
- After the final review fixes, targeted race tests for recovery/catalog/client/
  agent and the agent CLI, `make fmt-check vet build`, and the native build passed
  again. `macos/HMux/scripts/package.sh` produced the local 0.1.42 arm64 archive;
  source digest, archive round-trip and ad-hoc signature validation passed.

## Shared native/web workspace (2026-09-09)

The shared tab implementation is source version 0.1.43. `internal/sharedworkspace`
is the Home authority for both clients; selected tabs remain per-device.

Verified in this workspace:

- Full Go suite and `go vet ./...` passed.
- Race checks passed for web gateway, client, shared workspace and recovery.
  Opt-in web socket/logout and isolated `hmux-e2e-*` tmux PTY tests passed.
- Shared-store tests cover concurrent opens/closes/reorders, lost-response replay,
  bounded retry history, rejected stale/missing changes, private/symlink state,
  recycled IDs and recovery rebasing. Recovery tests include A→B→C without an
  intermediate client sync. Read-only polls do not create revisions or refresh
  authentication idle expiry.
- Web type/format checks, production build and eight tests passed; dependency audit
  reported zero known vulnerabilities at the time of the check.
- Swift workspace wire/state tests and the exact production synchronization
  methods passed: remote closes/order, local focus ownership, missing references,
  lost-response retry with a newer local edit, and no polling feedback writes.
- Native model, mutation/store, surface lifetime/input, search geometry,
  conversation, usage, metrics, staging, helper lifecycle and restart smoke checks
  passed. The aggregate native smoke run stopped at its Swift WSS loopback test
  with `NSURLErrorDomain -1004`; remaining independent smoke checks ran separately.

Limits: full Xcode build failed at package sandbox initialization. The requested
permission escalation was rejected because automatic approval review returned
503 Service Unavailable. Real browser execution and permanent Home installation
were also blocked by the approval infrastructure. These are not successful native
build/install or browser/device acceptance results. Existing native app archives
predate this change and must not be described as containing shared tabs.

The independent shared-workspace review found no remaining material blocker. Its
browser-storage boot finding was corrected with guarded preference access and
blocked-storage/quota regression tests.

The Linux gateway was then installed with a dedicated unprivileged systemd user,
loopback-only listener and a separate Nginx TLS site. Public HTTPS/static assets
returned 200, unauthenticated state returned 401, invalid login Origin returned
403, tokenless connector access returned 403, and HTTP redirected to HTTPS.
The service was active with zero restarts at verification. This verifies the
public gateway, not an authenticated real Home terminal session: permanent Home
connector and updated native app installation are still pending local execution.
Certificate renewal dry-run could not acquire Certbot's lock because the existing
scheduled renewal service was already active. Its timer was enabled and the
issued certificate was valid; no existing Certbot process was interrupted.


## Current web verification (2026-09-09)

[Web HMux](../WEB.md) owns current behavior/operations; [iOS input](../IOS_INPUT.md)
describes the accepted input path and its limits. Older checkpoints are retained
in [iOS input history](IOS_INPUT_HISTORY_2026-09-09.md) and
[web UI history](WEB_UI_HISTORY_2026-09-09.md), not as current instructions.

### User-confirmed behavior

- iPhone direct Korean input and native deletion work well enough for current use.
  The user accepted the space-to-echo transition as clean.
- Native long-press Paste works. Its first menu appearance is slightly slow; the
  user accepted this and requested no further tuning. The exact cause of that
  first-use delay was not measured.
- Selecting terminal text dismisses the iPhone keyboard. Selection/Copy with the
  keyboard continuously open was **not achieved**. The user accepted using the
  current behavior and stopped that work; it is not a pending acceptance request.
- Android Chrome PWA full-input-row long-press Paste appears to work per the
  user's initial feedback; the user chose to keep the current behavior. The earlier
  cursor-sized target was ineffective. This does not establish all-device coverage.
- Android tab/keyboard anchor, iOS font/top-safe-area fixes and Android Chrome PWA
  installation/colors were confirmed. Samsung Internet's special token backgrounds
  after mitigation remain unconfirmed; Chrome is the verified Android path.

These are user observations, not an exhaustive mobile compatibility certification.
Do not reintroduce rejected IME adapters or clipboard dialogs to pursue keyboard-open
selection without a new user request.

### Latest deployed frontend and checks

Release `20260909T140447Z` is the current frontend. Its 66 automated tests,
format/type checks and production build passed. Public app and diagnostic HTML
and all six referenced JS/CSS assets matched the build; gateway health, PWA CSP
and unauthenticated API 401 were verified. Deployment preserved a timestamped
rollback reference and did not restart the gateway or modify original tmux sessions.
The Android Paste correction exposes the existing focused input across its row
only while the keyboard is visible, retaining the pinned keyboard-opening anchor.
Shared native gesture protection preserves standard input/composition/paste events.
Tests cover geometry, boundary clamping, event ownership and disposal. The earlier
cursor-sized target (`20260909T135115Z`) was ineffective; the full-width 44 px band
received initial positive feedback on Chrome PWA. No browser/device run is claimed.
The subsequent documentation cleanup changed no runtime code or deployed assets.
The current release then added a 30-second web API deadline (including body reads)
and cancellation/ownership protection for workspace requests on account disposal.
Three new request tests cover stalled body timeout/retry, cancellation between
accounts, pre-cancelled callers and deadline cleanup after success. Existing shared
workspace Go tests passed (`go test ./internal/sharedworkspace`), covering merge,
explicit close, stale reorder and operation replay. The same-account tab invariant
is documented; a real multi-device open/close/order run was not performed.

Coverage includes the supplied iPhone event sequence, native editing, preview/echo
handoff, boundary/lifecycle handling and native gesture ownership. Simulated events
do not prove OS menu behavior; physical results are recorded separately above.
The latest targeted Go/gateway validation is recorded in the earlier dated sections.

### Remaining limits

Playwright/Chrome execution was unavailable due to browser connection and automatic
approval-review infrastructure failures. No automated browser/device run is claimed.
The Home disk sampler and used/total UI were built and the gateway schema deployed,
but the running Home connector update remains unverified after local process
inspection was blocked. Do not report live disk collection based on the web build.

## Seven-day web login (2026-09-10 KST)

Gateway release `20260909T153919Z` sets the login cookie and server session to
seven days from authentication, removing the former 30-minute idle expiry.
Logout and per-account eight-login eviction still revoke sessions; activity does
not extend the absolute deadline. In-memory sessions do not survive gateway restart.
`go test ./internal/webgateway` passed, including inactivity through the seven-day
boundary, expiry notification and secure cookie lifetime checks. Linux amd64 build
passed. Deployment preserved a timestamped rollback reference, restarted the gateway,
and verified active service, public HTTP 200 and anonymous API 401. Frontend assets
are unchanged from `20260909T140447Z`. No real seven-day device run is claimed.

## 2026-09-10 — desktop Safari native Hangul replacement fix

- Physical evidence: supplied Safari 18.6/macOS trace, baseline xterm 6.0.0,
  capped at 2,000 records. Native input precedes keydown229 with isComposing=false;
  selected insertReplacementText changes syllables without composition events.
  Terminal seq1065 sends ㄱ early and seq1073 replacement 겨 is not sent;
  overlapping keys also lose insertText events.
- Added a desktop Safari mode to the existing native-input bridge. It starts at
  beforeinput's selection, retains the browser-edited run and inline preview,
  accepts replacement/no-op edits, and suppresses the corresponding xterm229 diff.
  Boundaries flush once; modifiers do not. Standard composition remains stock.
  No mobile textarea geometry or desktop gesture interception is installed.
  Diagnostic desktop baseline remains stock for comparison.
- Regression coverage includes overlapping keydowns, ㄱ→겨, 민→미+나,
  잠→자+모, 값→갑→가→ㄱ→empty, paste, modifiers, standard composition and
  disabled-view isolation. Existing iPhone/Android checks remain passing.
- Validation: npm run check, all 69 tests, npm run build passed. Tests are event
  regressions, not physical Mac/Safari acceptance. After deployment on 2026-09-10,
  the user confirmed "잘된다. 이정도면 괜찮음." Current Mac Safari Hangul input
  is user-accepted; no further acceptance request remains for this fix.
- Frontend deployed as release 20260910T080741Z, preserving prior release and assets.
  No gateway restart. Public application and diagnostic HTML plus six referenced
  assets match local build bytes; CSP manifest-src/worker-src self and anonymous
  /api/state HTTP401 verified. Service active. Gateway binary remains the prior
  seven-day-login build.

## 2026-09-10 — web workspace shortcuts

- Sidebar toggle changed from Alt+Shift+B to Alt+Shift+L. Alt+1–9 selects
  the corresponding tab in current displayed order; absent numbers do nothing.
  Existing Alt+Shift+Left/Right navigation remains. Physical KeyboardEvent.code
  handles Option-generated characters on Mac; composition and extra modifiers
  remain excluded. Updated settings guide, tooltip and WEB.md.
- Type/format check, all 70 tests and production build passed. Frontend release
  20260910T081519Z deployed with timestamped previous release and no gateway restart.
  Both public HTML pages and six assets match local bytes; CSP and anonymous401
  verified. Browser keyboard interaction was not independently automated.

## 2026-09-10 — usage cards, tab outlines and simplified shortcuts

- Sidebar is now Alt+L; Alt+W invokes the existing active-tab close action and
  Alt+Q invokes the existing logout action. Repeated keydown does not repeat these
  actions; composition/extra-modifier exclusions remain. Guides/tooltips updated.
- Account usage now has provider summary cards, active-account badges and paired
  weekly/5-hour remaining-capacity meters, including accessible numeric meter
  values. Unknown/expired data stays unknown, not full capacity. Threshold colors
  distinguish low capacity; percentage labels remain visible. Inactive tabs now
  have a subtle #3a3935 border while active blue emphasis is preserved.
- Type/format checks, 70 tests and build passed. No independent browser visual
  acceptance claimed. Frontend release 20260910T082002Z deployed without gateway
  restart, preserving prior releases. Both HTML pages and six assets match local
  bytes; CSP and anonymous401 verified; service active.

## 2026-09-12 — persistent login sessions, desktop selection and URL actions

- Login sessions now survive gateway restarts until their original seven-day
  deadline. Private atomic session storage contains cookie hashes, independent
  public IDs, credential fingerprints and bounded login metadata; bearer cookies
  are never persisted. Password/TOTP/account changes invalidate that account on
  restart; OTP replay-counter changes do not. Revocation/expiry/eviction persist
  and cancel live connections; writes fail closed. Stable token-derived CSRF
  permits already-open clients to continue after a gateway restart.
- Settings lists/revokes only the current user's browser sessions, including
  current-browser indicator, browser/OS, login IP, approximate location and
  login/activity/expiry timestamps. Dialog disposal aborts in-flight requests;
  successful revocation remains successful if the following refresh fails.
- Owner explicitly authorized external IP geolocation. The fixed HTTPS ipwho.is
  endpoint receives public login IP only. Bounded request concurrency/time/cache,
  same-IP in-flight coalescing, cancellation recovery and special-use address
  exclusions are tested. Location is optional and not an authentication factor.
- Alt+Backquote aliases Alt+L using the physical code, including the Korean ₩
  labeling. No character-based IME interception was added.
- Desktop drag/copy: tmux DEC mouse tracking disables ordinary xterm selection.
  Desktop-only primary gestures defer clicks; dragging enters native xterm forced
  selection without sending a partial mouse drag remotely. Click and wheel remain
  PTY mouse reports. macOS Cmd+C copies selection; Ctrl+C still sends ETX.
- Official addon-web-links 0.12.0 plus OSC8 handler shows a URL popover with an
  explicit new-window anchor. HTTP(S) only, no embedded credentials, textContent,
  noopener/noreferrer. Wrapped URLs retain their complete address; selecting a
  link does not activate it. Popover/disposable lifecycle tied to original tab.
- Independent Sol reviews found/fixed in-flight geo caching, cancellation retry,
  revoke/refresh status, reserved-IP filtering, Mac Ctrl+C behavior, fresh-source
  login-rate-limit eviction and ignored auth-touch failures before mutations.
- Automated: 79 web tests, type/format check and build passed. Go webgateway tests,
  full race suite (before final narrow auth checks), final focused race suite and
  vet validate persistence/restart, ownership/revocation, failure paths and geo.
- Browser: isolated Playwright Chrome fixture, no production tmux operations.
  390px settings/session cards visually inspected; remote revoke and Alt+Backquote
  exercised. Real xterm with DECSET1002/1006: drag selection/copy payload matched,
  Cmd+C was handled with no wire data, macOS Ctrl+C emitted ETX, ordinary click and
  wheel emitted mouse reports, wrapped URL popup matched, explicit Open created
  a separate Example Domain tab. User acceptance on physical Safari remains
  separate from these browser/event checks. Mobile accepted IME paths unchanged.
- Deployed server+frontend release `20260912T090412Z`; previous release preserved,
  gateway restarted and active. Public app/diagnostic HTML and six assets match
  local bytes, gateway SHA-256 matches built binary, CSP manifest/worker self and
  unauthenticated state/session-list HTTP401 verified. No production login or
  existing tmux session was used as a test. Initial memory-only logins require one
  fresh sign-in; subsequent sessions persist through restart.


## 2026-09-12 — mobile terminal URL actions

- Extracted the shared HTTP(S)/OSC8 popover and added mobile short-tap activation.
  Native editable/Paste targets, native range copy, long presses, movement and
  multitouch retain their gesture ownership. Popup actions have 44 px targets and
  are constrained to the visual viewport. Tab/dialog/scroll/disposal dismiss it.
- TypeScript/Prettier check, 82 web tests and production build passed.
- Real Chromium with CDP touch input at 390×844 passed 19 assertions: ordinary and
  repeated URL taps; wrapped links after Korean text; named OSC8 links; unsafe
  scheme rejection; viewport bounds and safe explicit new-window action; no PTY
  bytes for URL taps in DEC1003 all-motion mode; no keyboard focus on URL taps;
  long-press/drag rejection; native selection blocks activation and native copy
  keeps browser serialization; scrolling still emits tmux wheel reports;
  same-cell URL updates use fresh output; lifecycle cancellation and disposal.
  Native browser selection dismissal after a separate tap remains browser-owned.
- Desktop Chromium confirmed drag selection without PTY input, macOS Control+C
  interrupt, URL popover, no PTY input for link activation and Escape dismissal.
- Screenshot: `output/playwright/mobile-links-popup.png`. These are automated
  Chromium checks, not physical iPhone/Safari or Android device acceptance.
- Independent Sol review found no material blocker and independently reran the
  check and all 82 tests. Minor edge: a second finger used entirely outside the
  terminal host may not cancel the original tap; the maximum effect is showing
  the popover, never opening a URL or emitting terminal input.
- Frontend release `20260912T101845Z` deployed without a gateway restart. All 30
  staged files matched local SHA-256. Public app/diagnostic HTML and their six
  referenced assets matched local bytes; anonymous state/session APIs returned
  401; PWA manifest/worker CSP remained intact. Prior release retained for rollback.


## 2026-09-12 — web image/file attachments and three-hour retention

- Added a paperclip to the existing terminal toolbar and keyboard-visible mobile
  auxiliary row. Desktop file drags show a temporary drop overlay; transfer/pending
  status is one compact row and is hidden when idle. Native picker accepts images
  and ordinary files (1–16, nonempty, 32 MiB/file, 128 MiB total).
- Authenticated `/api/upload` uses same-origin cookie plus first-frame CSRF,
  256 KiB binary chunks with acknowledgements, five-minute/30-second timeouts,
  two global/one-login transfer bounds, and a pinned Home connector generation.
  Original filenames stay in the browser. Gateway verifies exact session/response
  identity, sanitized generated paths, sizes and SHA-256 before returning paths.
- Home reuses private filestage storage. Web expiry is completion + three hours;
  startup/every-minute cleanup continues during connector WSS reconnects. Cleanup
  requires the foreground Home process; no daemon or LaunchAgent was added.
- Exact original tab instance/identity, connection generation and selection epoch
  guard automatic POSIX-quoted paste without Enter. Focus/tab/dialog changes defer
  insertion to the original tab's explicit action. Close/cancel/logout aborts.
  UI review found a delayed-logout cancellation gap; fixed cancellation at logout
  intent and blocked new attachments while logout is pending. Current-browser
  revoke uses the same immediate cancellation hook.
- Web formatting/typecheck, 86 tests and production build passed. Full Go tests,
  race for webgateway/filestage/client, and full Go vet passed. Followup actual
  `runHomeUpload`/Receive integration tested two-file binary boundaries, real disk
  contents, hashes, identity checks, ~three-hour expiry, and joined partial cleanup
  using a temp spool/fake verifier; ten runs and race passed. Full Go tests and
  webgateway race passed again after those test additions. No pre-existing tmux
  sessions or production credentials were used for testing.
- Chromium full-app mock checked picker upload, filename privacy, quoted path
  insertion without newline, tab-switch suppression and original-tab manual insert,
  cancel/late response, desktop drop and transient overlay, mobile keyboard toolbar,
  and immediate cancellation/no paste during a delayed failed logout. Screenshots:
  `output/playwright/attachments-mobile-idle.png`, `attachments-mobile-progress.png`,
  `attachments-mobile-pending.png`, `attachments-desktop-drop.png` and
  `attachments-mobile-keyboard-toolbar.png`. These automated viewport/flow tests
  are separate from physical iPhone/Android picker acceptance.
- Deployed gateway/frontend release `20260912T104601Z`, preserving the previous
  release. Updated the foreground Home connector with a timestamped binary backup
  and unchanged configuration; installed SHA-256 matches the Darwin build, and
  the new process has an established TCP connection. Gateway service is active.
  Public app/diagnostic HTML and six referenced assets match the local production
  build; CSP manifest/worker self remains intact. Anonymous state, session-list
  and upload APIs return 401. Authenticated production upload was not exercised;
  transfer validation used the isolated browser fixture and real Home receiver tests.

## 2026-09-12 — tmux redraw control

- Added a redraw icon immediately left of Attach in the top toolbar and the
  keyboard-visible mobile row. Fits/repaints xterm and sends a dedicated refresh
  frame on the existing socket; no page reload, terminal reconnect or input bytes.
- Gateway supplies its own view ID and discards browser-supplied target/data.
  Home resolves only that grouped view's attached client PID/TTY and executes
  `refresh-client` with argument arrays. Two-second command deadline and one-second
  per-view rate limit; redraw failure does not terminate a working view.
- Full Go tests/vet, web check, 86 tests and build passed. Isolated tmux test
  refreshed twice without releasing its disposable view; scoped-client targeting
  and authenticated socket forwarding tests passed, including targeted race runs.
  Existing user tmux sessions were not used for tests.
- Chromium at 1200px/390px verified button order, mobile keyboard-row visibility,
  three refresh frames and unchanged connection/page with no application input.
  Screenshots: `output/playwright/terminal-refresh-1200.png` and
  `output/playwright/terminal-refresh-390.png`. Physical device acceptance remains
  separate from these browser checks.
- Independent read-only review found no blocker. Deployed release
  `20260912T121829Z` and updated Home with a timestamped binary backup and unchanged
  configuration. Gateway is active; Home binary matches the build and maintains
  an established TCP connection. Public HTML and six assets match local bytes;
  PWA CSP and anonymous API 401 checks passed. Prior server release retained.

## 2026-09-12 — redraw correction after desktop feedback

- User reported no visible effect and a distorted icon. The prior test verified
  tmux command success/connection preservation, but did not require the program
  inside the pane to repaint. A real isolated TUI with a SIGWINCH counter reproduced
  the failure: `refresh-client` replays tmux's cached content without increasing
  the application's redraw counter.
- Refresh now always resynchronizes PTY dimensions and discovers the foreground
  group of the owned client's active pane using bounded, fixed-argument `ps`.
  PID/TTY/pane identity is validated and rechecked before SIGWINCH, then the same
  tmux client is repainted. No input, temporary shared-window resize or reconnect.
  Darwin cannot use TIOCGPGRP on another session's controlling terminal; ps avoids
  that limitation. Generic success/failure frames now reach the originating browser;
  success briefly highlights the icon and failure is visible. Replaced the malformed
  arrow path with a circular 18px vector.
- The previously failing real TUI test now passes, including three repetitions and
  race. Foreign/stale pane targeting and sanitized socket failure responses are
  covered. Full Go tests/vet, 86 web tests, web check/build passed. Chromium checked
  unchanged socket/page, no program input, size resync, desktop/mobile button
  placement, visible failure and square icon dimensions. Screenshot:
  `output/playwright/terminal-redraw-fixed-desktop.png`. Physical Safari acceptance
  of the user's original distorted application screen remains separate.
- Independent review found no blocker and independently passed the real TUI test,
  client/webgateway tests and vet. Deployed release `20260912T123018Z` and Home
  connector with timestamped backups. Gateway active; Home build hash and live
  TCP connection verified. Public HTML/six assets match production build; CSP and
  anonymous 401 checks passed. Rapid repeats inside the one-second rate window
  remain ignored; the first eligible refresh reports success/failure.

## 2026-09-12 — per-account TOTP login setting

- Settings → Account security now exposes a TOTP switch. Changing either direction
  requires the current password and an unused code from the retained authenticator.
  Current browser and original expiry survive, other logins of that account are
  revoked, and other accounts remain unchanged. Disabled accounts use password-only
  login; enabling restores the existing authenticator requirement.
- Legacy credentials default to enabled and retain identical fingerprints. Login
  reveals a TOTP challenge only after password verification, without a cookie/session
  or consumed code. Bounded hashing, per-source login throttling and per-account
  setting throttling remain enforced. Toggles serialize and recheck live session,
  expiry, credential fingerprint and replay counter after hashing. Private bounded
  timestamped backups stay outside the extra-account directory; persistence failure
  rejects access. New state and account isolation survive restart.
- Settings clears passwords/codes on completion, cancellation and disposal; late
  replies cannot affect a later dialog/account. Uncertain mutations re-read policy
  before retrying. UI review found mutable login identity during an in-flight
  response; fixed by locking login inputs through challenge/success/failure, and
  verified with delayed browser responses for both challenge and password-only login.
- Full Go tests and vet passed; webgateway race passed. Contract tests cover legacy
  fingerprint compatibility, challenge cookie/counter absence, request validation,
  reauthentication/replay, no-op changes, primary/additional account persistence,
  revocation, storage failure and concurrent login/toggle/revocation. Web formatting,
  typecheck, 90 tests and production build passed.
- Isolated Chromium flow passed password → code → workspace, incorrect reauth without
  logout, disable → password-only login → enable → code required, and login-session
  list refresh. Desktop/mobile screenshots inspected:
  `output/playwright/hmux-totp-confirm-desktop.png` and
  `output/playwright/hmux-totp-settings-mobile.png`. Real account settings and
  production credentials were not changed for testing.
- Independent final backend/UI review found no remaining blocker. Deployed
  gateway/frontend release `20260912T125559Z`; previous release retained. Service
  is active and the existing Home connector re-established its connection. Public
  app/diagnostic HTML and six assets match local bytes; PWA CSP remains intact.
  Anonymous state, sessions, uploads and account-security requests return 401.
  Deployment leaves every existing account's TOTP policy unchanged.


## 2026-09-13 — hide Codex handoffs in conversation reader

- Confirmed the reported handoff is recorded as an assistant `message` with
  `phase: final_answer`, no summary channel, and `content_item_kinds: [unknown]`.
  That metadata also appears on ordinary replies, so it cannot classify handoffs.
- Home now excludes the narrow `## Task and constraints` → `Workspace:` envelope
  on assistant messages and automatic model-continuation envelopes before sending
  reader responses. Source records remain unchanged. Regression fixtures cover
  CRLF/whitespace, normal final answers with the same metadata, user-authored task
  specifications and quoted/reported examples. Other summary formats are not claimed
  to be exhaustively detected.
- `go test ./...`, `go vet ./...`, and `go test -race ./internal/catalog` passed.
  Built the Home connector and replaced the installed binary with a timestamped
  backup, preserving its foreground configuration. Installed bytes match the build;
  one connector is running with an established TCP connection. Gateway/frontend
  assets did not require changes. No pre-existing tmux sessions were used for tests.
- Reopen conversation view to fetch the filtered response. This verifies parser
  behavior and connector deployment; a production authenticated UI check was not run.


## 2026-09-13 — hide injected goal context

- Added `codex_internal_context` opening/closing markers to the existing injected
  user-content filter, hiding internal goal reminders in conversation view.
- Catalog tests passed, including complete/incomplete goal wrappers and preservation
  of a separate public user content part in the same message. Home build passed.
- Installed the connector with a timestamped backup and unchanged foreground
  configuration. Installed bytes match the build; one connector has an established
  TCP connection. Reopen the reader to refresh. No live tmux tests were performed.


## 2026-09-13 — identify internal summaries from compaction records

- Read-only format audit of 35 recent rollout files found 248 compaction envelopes
  whose extracted summaries exactly matched the preceding assistant message. Only
  aggregate counts, wrapper metadata and headings were inspected; no transcript
  bodies were copied into repository fixtures. Titles vary widely, so broad heading
  filters were not added.
- The bounded conversation tail is indexed for exact hashes of summaries from
  complete `compacted` records before applying the public message count limit.
  Compaction envelopes above the public-message line limit can be inspected within
  the same 4 MiB tail; replacement history is neither decoded into messages nor
  returned. Partial/malformed/unconfirmed records cannot classify public answers.
- Regression tests cover varied headings, preservation of identical user text,
  absent/partial/malformed compaction, large compaction envelopes and keeping all
  200 public messages when a hidden summary follows. Full Go tests, vet and catalog
  race tests passed. Existing injected-context and narrow handoff filters remain.
- Home build installed with a timestamped backup and unchanged foreground settings.
  Installed bytes match the build and one connector has an established TCP connection.
  No existing tmux sessions were used for tests. Reopen conversation view to refresh;
  production authenticated UI acceptance was not performed. Confirmation outside the
  bounded tail and unfinished compaction remain limitations.


## 2026-09-13 — pending input follows terminal cell colors

- The shared macOS Safari/iPhone native-input preview now reads the cursor cell's
  public xterm color attributes instead of always using the CSS base background.
  Supports default/theme ANSI, extended palette, RGB and inverse colors. The
  retained echo copy inherits these colors and refreshes its anchor cell on render.
  Browser-owned Hangul editing, flush boundaries and wire ordering are unchanged.
- Web check, all 92 tests and production build passed. New tests cover black/gray,
  custom palettes/defaults, muted diff colors and inverse video.
- Isolated headed Chromium with real xterm 6 verified black RGB, indexed gray and
  iPhone-bridge RGB gray pending previews, no premature data, redraw color changes,
  and matching echo color with a single boundary handoff. Screenshot inspected:
  `output/playwright/input-preview-backgrounds.png`. This is synthetic bridge/render
  verification, not new physical Safari/iPhone IME acceptance. No production tmux
  sessions or credentials were used for tests; fixture browser/server were closed.
- Published web release `20260913T130659Z`, preserving the existing gateway binary
  and previous release. Thirty staged web files matched local hashes; service active.
  Public app/diagnostic HTML and their asset references match the local build,
  PWA CSP is intact and anonymous protected APIs return 401. Home needs no update.
