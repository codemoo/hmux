# Validation status

This is the current validation/deployment summary, updated 2026-09-25. It describes
one maintained installation; it does not establish the state of independent deployments.
Detailed dated evidence is preserved in [validation history](archive/VALIDATION_HISTORY_2026-09-24.md).

## Current verified baseline

- Linux Gateway: `rust-gateway-trial-20260924T151949Z`, activated 2026-09-24
  15:37 UTC. Rust binary SHA256 `7d355330907bed6501199544480227f2502f16252132f310971bb281ea7a364c`.
  Native macOS Home/helper also run the Rust trials recorded below. Web assets
  remain the `cleanup-20260923T154706Z` baseline (runtime commit `fe0c509`).
- Current release verified no-store/PWA CSP, five anonymous API 401 barriers and
  preservation of all original tmux identities during the Gateway trial. Browser-only disposable views
  were recreated on reconnect. Existing rollback releases and
  timestamped Home backups remain available.
- `make check`, `make integration` and `make build` passed for the performance
  baseline. Subsequent latency instrumentation passed focused Go tests/race/vet
  and both binary builds; provider-aware loading passed web check/build and 171 tests.
- Native service installation/update succeeded. Earlier historical entries saying
  Home activation was pending are superseded; physical reboot/login behavior was
  not newly tested. Linux unit verification belongs to Linux CI/device evidence.

## Browser and device evidence

- Synthetic Chrome checks: cached workspace before live state, no premature terminal
  connection, account isolation, 32 restored tabs with only one initial xterm,
  initialization on selection, and unchanged usage polling without DOM mutations.
- Claude loading card reviewed at 390×844 with a held synthetic response. Codex and
  neutral fallback labels have regression coverage. Provider attribution for returned
  messages remains server-owned. Live authenticated Claude conversation acceptance
  has not been recorded, despite deployment of the implementation.
- Device-confirmed input behavior and accepted limits remain in
  [browser input](BROWSER_INPUT.md#accepted-mobile-behavior) and [iOS input](IOS_INPUT.md).
  Synthetic responsive checks do not establish physical Safari/iOS/Android behavior.

## Performance evidence and unresolved work

The latest startup sample separated recovery synchronization (1,310 ms), catalog
collection (4,720 ms) and publication (2 ms). Nested spans overlap and must not be
summed. Earlier first-catalog observations were about 14.8 s and 7.4 s; these are
individual observations, not a controlled browser latency benchmark.

Remaining work is to attribute the slow catalog internals and capture intermittent
5-second requests with the new operation-tagged diagnostics. Recent workspace
requests were 61–171 ms with `send_ms=0`. Neither every latency source nor the
cause of every connection interruption is claimed resolved. No measured total
memory target is claimed.

The deployed-artifact [memory comparison](../bench/hmux/README.md#deployed-artifact-memory-comparison-2026-09-25)
records fresh isolated Go/Rust measurements and separate live Rust readings.
Rust used less PSS in the measured Linux fixture; no matched live Go sample or
whole-product memory-budget acceptance is claimed.

## Rust-only source retirement (2026-09-25)

The user authorized removing the Go source after the maintained Rust trials.
Default build/check/install paths and Linux/macOS CI now use Rust; no `.go`,
`go.mod` or `go.sum` remains in the active source tree. Prior Go sources and
migration oracles remain at checkpoint `c061f28fe7ea8e865578ac1189240447d0ebaa6f`;
frozen synthetic compatibility vectors and required attribution are retained.

- A complete local `make check` passed: Rust formatting/strict Clippy, 635 passing
  Rust tests with 20 opt-in skips, ShellCheck, TypeScript and 171 frontend tests.
- `make integration` passed the production Rust Gateway/Home pair, both-codec
  Home WSS, native CLI, workflow hooks and isolated real-tmux lifecycle tests.
  The new pair fixture initially omitted required pane/recovery data; completing
  that synthetic fixture fixed startup, and the final runs passed.
- `make build` produced the macOS ARM64 native bundle; `make bundle-check` passed
  all five manifest/notices/install/upgrade/rollback checks in temporary Homes.
  Native notices cover 158 locked crates and the Rust standard library.
- Shell formatting, local documentation links, contract-owner paths and the
  independent retirement review passed. Normal contributor/CI commands need no
  Go toolchain; optional historical comparisons require explicit external artifacts.
- No service was installed, restarted or deployed during this cleanup. This source
  decision does not complete physical-device, real-account service, 24h/72h soak
  or whole-product resource acceptance. Those limits remain in
  [Rust runtime status](RUST_MIGRATION.md).

## Source publication checks (2026-09-25)

- `make check`, `make integration`, `make build`, `make shfmt-check` and the final
  formatting, strict workspace Clippy and Protobuf-generation checks passed locally.
- Rust workspace coverage completed with 635 distinct passing tests and 19 opt-in
  skips across the full run and corrected installer-test rerun. The initial run
  timed out waiting for a synthetic process to start; the focused retry passed,
  and the full rerun used two test workers without relaxing runtime deadlines.
  That rerun exposed a concurrent timestamp collision in an installer fixture;
  a per-process sequence fixed it, and both installer tests passed afterward.
  The failed runs are retained; no single uninterrupted full-green run is claimed.
- Independent publication/documentation review and local Markdown link checks
  completed. Locked notices were verified for 158 crates and the Rust standard
  library. Upstream license bytes remain intact, including the declared CRLF file.
- Runtime source matches the deployed candidate; the later correction changes
  only a test fixture. This publication does not redeploy services, switch default
  builds or establish the remaining device/soak gates. GitHub CI results belong
  to the published commit's Checks run, separately from these local results.

## Rust transition

The maintained Linux Gateway is running a user-authorized Rust trial under its
existing systemd unit. That rollout left Go Home/helper, provider processes and
browser assets unchanged; the helper-only and Home trials below followed separately.
Default builds remain Go; full migration acceptance is tracked in
[Rust migration](RUST_MIGRATION.md).

- Exact-artifact synthetic Rust → deployed Go → Rust checks passed for retained
  login and both revocations, using private fixtures with umask 0077. Rust Gateway
  with the deployed Go executable as Home passed catalog, profiles, workspace,
  PTY input/ACK/resize, close/reconnect/logout and graceful shutdown checks.
- The staged manifest verified 377 files, including notices for 141 crates and
  the Rust standard library. All 47 existing public assets were byte-identical;
  HTTPS hashes, five anonymous API 401 barriers and no-store/PWA CSP passed.
- The old process exited successfully before the release switch. The Rust PID
  and executable hash remained stable with zero automatic restarts in the initial
  observation. Unchanged Go Home reconnected and published a fresh catalog.
  Credentials, connector token and persisted login file remained byte-identical.
- Two initial attempts returned to Go because of rollout-probe defects: checking
  the executable before systemd's child completed exec, then using a loopback Host
  that both runtimes correctly rejected with 421. The script now waits for the
  expected executable and probes normal public HTTPS; it passed against Go before
  the successful retry. Neither attempt restored older authentication state.
- The previous Go release and private deployment/rollback receipts are retained.
  Rollback changes the release only and keeps current authentication state, as
  required by [Rollback](ROLLBACK.md). Private state backups are recovery evidence,
  not an automatic rollback input.

Actual authenticated browser/TOTP acceptance was not completed: the available
Chrome session was signed out, and the subsequent browser check was blocked by
another extension UI. Public HTTP checks and Home publication do not establish
that acceptance. Long soaks, physical-device acceptance and a matched production
memory comparison remain pending. This section records the Gateway-only rollout;
later Home/helper replacement does not establish Go retirement.

### macOS helper-only trial

On 2026-09-24 16:01 UTC, the maintained macOS `hmux-agent` was replaced with the
verified Rust executable, SHA256
`88a532322c37c2283d38cc05d0bee12b2e963518444861e88d7187b68197865c`.
The Go Home executable and its launchd process, run count and plist stayed unchanged.
No Home restart or configuration update was performed.

- Exact installed Go/Rust mixed-pair and helper handoff checks passed, including
  current workflow state preservation and concurrent helper writes in synthetic fixtures.
- Before publication, the native pair installer was rehearsed separately through
  Rust installation and Go rollback in a disposable directory. The actual deployment
  replaced only the helper, under the native installation lock with atomic rename.
- Private deployment checks verified helper readiness and preservation of existing
  sessions. The prior Go helper remains available in timestamped rollback storage;
  rollback must retain current state. Host-specific receipts remain private.

This produced Rust Gateway + Go Home + Rust helper before the Home trial below.
The helper-only checks do not establish long-running/device acceptance.

### macOS Home trial

On 2026-09-24 16:09 UTC, the maintained macOS Home connector was replaced with
the verified Rust executable, SHA256
`c5c771a40e35d14a2f606205bb1331e2ef5924f0c9da6d5c3aeda773096a412a`.
The already installed Rust helper was retained. The current maintained combination
is **Rust Gateway + Rust Home + Rust helper**; default builds still remain Go.

- The exact candidate manifest verified 357 files, including refreshed notices
  for 158 crates and the Rust standard library. Previously accepted mixed-pair,
  helper-handoff and isolated native installer/rollback checks were reused after
  verifying unchanged source and artifacts.
- The old connector was stopped and its singleton lock released before native
  `install-home --binaries-only` installation. Private checks verified unchanged
  configuration, original-session preservation, helper readiness and initial
  authenticated workspace/terminal connectivity without a restart loop.
  Downstream checks supplement Home's `Connected` log, which alone establishes
  only the WebSocket handshake. Host-specific state, counts and logs stay private.
- Private timestamped artifacts retain Go Home plus the current Rust helper.
  Home rollback reinstalls that pair under the existing plist while retaining
  current configuration, authentication and runtime state.

This establishes the maintained runtime replacement and initial live connectivity.
Full browser/input/TOTP acceptance, physical reboot/login, long soaks and a matched
production memory comparison remain separate release gates.

## Ongoing maintenance

Documentation and Go cleanup completed locally on 2026-09-24. Current references
were separated from dated evidence, and Go CLI/gateway/collector/service/workflow
responsibilities were split into focused files without intended behavior changes.
Independent documentation and Go reviews reported no material findings.

`make check` passed (including full Go/collector race/vet, ShellCheck and 171 web
tests). Installer, hooks and isolated session-create integration passed. Isolated
PTY integration initially failed because the sandbox denied `/bin/ps`; its targeted
rerun in the normal host environment passed. Local Markdown paths/headings and
archive discovery checks passed. `make build` passed for web assets, Linux gateway
and macOS Home/helper binaries. The cleanup was subsequently deployed as the
release above. Linux CI exposed a scalar WorkingDirectory quoting error and a
disposable-process test assumption about birth-time resolution; both were fixed
in `fe0c509`. GitHub Actions run `35883961196` passed all three jobs for that runtime commit:
macOS Go/unit/race/vet/integration, Linux native service verification, and web
check/test/build.
