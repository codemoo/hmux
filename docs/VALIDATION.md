# Validation status

Evidence summary updated 2026-09-25. Deployment observations describe one maintained
installation, not every HMux deployment. [Rust runtime status](RUST_MIGRATION.md)
owns the remaining acceptance queue; [Rust verification](../tests/RUST.md) owns
runnable checks. Earlier rollout details and failed attempts remain in the
[dated validation history](archive/VALIDATION_HISTORY_2026-09-25.md).

## Source and automated checks

Source checkpoint `9211fb95edc78bbacb0d265b37b31708515a0b6a` retires Go from the
active tree. Gateway, Home, usage, helpers, service management and installation
use Rust; the web/PWA remains TypeScript. Default builds, contributor checks and
CI need no Go toolchain. Historical sources remain at
`c061f28fe7ea8e865578ac1189240447d0ebaa6f`; frozen synthetic compatibility vectors
and required third-party attribution are retained.

Local results for the retirement checkpoint:

| Check | Recorded result |
| --- | --- |
| `make check` | Passed Rust formatting, strict Clippy, 635 Rust tests (20 opt-in tests ignored), ShellCheck, TypeScript and 171 frontend tests |
| `make integration` | Passed actual Rust Gateway/Home pair, Home WSS with both codecs, native CLI, workflow hooks and isolated real-tmux lifecycle tests |
| `make build` | Built the macOS ARM64 native bundle; no cross-platform build claim |
| `make bundle-check` with that bundle | Five manifest/notices/install/upgrade/rollback checks passed in temporary Homes |
| Publication review | Shell formatting, local Markdown links, contract-owner paths and independent retirement review passed; notices cover 158 locked crates and the Rust standard library |

The new native-pair fixture initially omitted required pane/recovery data. Completing
the synthetic fixture fixed startup; the final local runs passed. Earlier failed
publication attempts remain historical evidence, not successful checks.

[GitHub Actions for the retirement checkpoint](https://github.com/codemoo/hmux/actions/runs/36032474956)
is separate from local evidence. Its Linux job failed ShellCheck on an `&&`/`||`
conditional in the bundle script after Rust tests passed. Web, Protobuf schema, dependency-policy and macOS native jobs passed.
The overall retirement CI result was failure; local success is a separate result.

The [preceding checkpoint's CI](https://github.com/codemoo/hmux/actions/runs/36029654175)
failed after five Linux mixed/native runtime-and-codec pairs passed: the historical
Go/Rust current-state rollback oracle hit an HTTP client deadline at initial Rust
login. That rollback gate did not pass. Retiring Go source does not erase the result.

## Connection stability update

Runtime checkpoint `b81c076` follows the retirement checkpoint. An incident review
confirmed remote Home operation errors behind HTTP 502 responses and separate
Home disconnects caused by catalog-query failure. The earlier Gateway diagnostic
mapped every nonempty remote error to `invalid`; it cannot establish the exact
remote cause retroactively. No process restart was observed in either case.

Catalog/metadata read failures now retry without disconnecting live views or
renewing stale catalog timestamps. Background inspection reserves capacity for
interactive conversation reads. Home adds bounded fixed failure/latency categories
and first-publication readiness; Gateway distinguishes remote-operation errors,
admission pressure, invalid responses and action deadlines.

Validation of this change:

- `make check`: 639 Rust tests passed, 20 opt-in tests ignored; formatting, strict
  Clippy, ShellCheck, TypeScript and all 171 web tests passed. The final publication
  log addition passed Clippy and the live-view recovery regression again.
- Native pair login/catalog/terminal/reconnect, Home WSS with both codecs,
  CLI/workflow hooks and isolated tmux lifecycle checks passed. The first integration
  attempt was blocked by sandboxed macOS boot-time access. With normal access, the
  final shell-return check exposed a test race between file creation and its write;
  it passed after waiting for the complete marker and rerunning that target.
- macOS ARM64 and Linux AMD64 release bundles built; all five bundle checks passed
  on each. The initial Linux staging/check attempt had group-writable directories;
  owner-controlled staging and a private umask passed the same check.
- Independent runtime/rollout review completed. Readiness was strengthened from a
  WSS connection to first catalog publication; rollback uses a retained known-good
  installer. Shell formatting and documentation link/anchor checks passed.

The metadata-overlay failure path and a combined live-view recovery through the
40-second freshness expiry were not separately exercised by the new regression.
This update does not establish long-running or physical-device acceptance.

## Rust quality review

A subsequent source review found and corrected three bounded failure paths:

- Failure to configure one accepted socket now rejects only that connection;
  it cannot exit the Gateway HTTP listener through the socket-option error path.
- Failed disposable-view cleanup now emits a fixed, privacy-safe diagnostic and
  a non-success terminal exit. Its admission slot remains quarantined; unrelated
  views and requests remain live. Reporting belongs to the cleanup owner so it
  also runs after the requesting task is dropped.
- Native installation creates all new parent directories with mode 0700, including
  when the invoking shell uses umask 0002. Existing permissions and trust checks
  are preserved. The old installer failure was reproduced in a temporary Home.

[CI for `73bdfe7`](https://github.com/codemoo/hmux/actions/runs/36037687994)
failed separately: the macOS job exposed two key-file tests racing a process-wide
admission slot, and Linux ShellCheck rejected three hook-script conditionals.
Those tests now serialize around the existing admission contract, and the shell
guards use explicit conditionals. Production admission limits are unchanged.

The regression checks exercise child-specific umask, both terminal codecs with
cleanup failure and a surviving view, and injected socket setup failure followed
by a successful HTTP request. The quarantine test runs in its own child process
so intentionally lost permits cannot change other tests' capacity. Independent
review found no material defects in this change. An actual OS socket-option
failure and cleanup diagnostics during cancelled startup are not separately
reproduced. These findings are not established causes of the earlier live incident.

Local verification of the integrated changes passed:

- `make check`: formatting, strict Clippy, 642 Rust tests (20 opt-in tests ignored),
  ShellCheck, TypeScript and 171 web tests.
- `make integration`: actual native Gateway/Home login, terminal and reconnect;
  Home WSS with both codecs, CLI/hooks and isolated real-tmux lifecycle checks.
- `make build` and all five bundle checks on macOS ARM64; `make shfmt-check`.

Source checks alone do not establish deployment. Native deployment validation
is summarized below; identifying receipts remain private.

### Usage collector recovery

A reported missing-usage symptom prompted a further review. A synthetic regression
confirmed that a short source `retry_at` could expire between quota refreshes:
heartbeat publication advanced `generated_at_utc`, failed the future-deadline
validation and permanently cancelled both providers' collection. Publication now
clears elapsed retry deadlines when it advances the publication timestamp, without
changing quota observation time. Invalid data is withdrawn while existing bounded
source workers continue; the next valid result recovers without a Home restart.
Fixed-category diagnostics record first publication, validation failure and recovery.
The prior runtime lacked these diagnostics, so this reproduction alone does not
prove the exact trigger of the live missing-usage incident.

The focused publication/recovery regressions, all 288 Home tests (8 opt-in tests
ignored), workspace strict Clippy, `make integration`, macOS ARM64 release build
and all five bundle checks passed. Independent review found no material defects.
These checks cover collector recovery; a valid snapshot pair alone does not prove
that each provider returned current quota data.

[CI for `a6e1588`](https://github.com/codemoo/hmux/actions/runs/36044120812) passed
macOS, web, Protobuf and dependency checks. Linux passed Rust/lint checks and the
native pair, then failed the workflow-hook installer: GNU `stat -f` printed
filesystem information before the BSD-to-GNU fallback. The installer and its
permission assertions now choose flags explicitly by OS; isolated hook checks
passed on both Linux and macOS.

[CI for `b581a96`](https://github.com/codemoo/hmux/actions/runs/36047586025) passed
Linux, web, Protobuf and dependency checks. macOS exposed an action/catalog test
ordering race: the test discarded an updated catalog received before the action
reply, then waited less than the unchanged-catalog renewal interval. The harness
now retains both observations in either order without extending its deadline;
all five enabled session-peer tests passed locally (one real-tmux test ignored).

A bounded source check covered a full refresh interval for the supported usage
collectors. Health assertions passed; account data and live quota values are not
part of the public evidence.

### macOS CPU and disk sampling

The reported partial metrics display led to a reproduced failure cascade: `top`
exhausting the shared three-second command budget omitted CPU and skipped the
subsequent filesystem sample. The regression failed against the previous code.
CPU now uses the second interval sample from the built-in CPU-only `iostat`
report, and command timeouts no longer suppress the separate `statfs` read.
The existing single-worker limit, child cancellation/reaping and fresh-field
semantics remain in place; no resident process or dependency was added.

All 16 focused sampler/parser tests passed, including timeout isolation,
cancellation and initial catalog readiness with both codecs. Strict Home Clippy
passed. An opt-in native check returned three fresh CPU/RAM/disk samples on macOS.
A three-run command comparison measured `top` at 2.005–2.139 seconds wall time
and 992–1,130 ms child CPU time, versus `iostat` at 1.006–1.016 seconds wall time
and 4–7 ms child CPU time. This is a local collector-command measurement, not a
whole-application benchmark. Authenticated browser rendering remains unverified.

## Unified installer and web enrollment

The unified guide separates Gateway, Home and combined roles, plus local or SSH
installation. Native fixtures cover role/argument boundaries, private pairing,
Home startup opt-out and installer cancellation. Browser enrollment fixtures cover
TOTP and non-TOTP account creation, same-origin/setup-token rejection, enrollment
replacement, normal login and setup retirement across restart. Synthetic Chromium
checks exercised account creation, TOTP verification and return to normal login;
a mobile-sized viewport was reviewed. This is not physical-device acceptance.

Release-mode Rust formatting, workspace Clippy and the non-opt-in native suites
passed, as did 176 web tests, 12 CLI/PTY checks and five native bundle checks.
The synthetic production Gateway/Home pair passed login, catalog, terminal ACK,
disconnect and reconnect. Its first sandboxed run could not start the Home child;
the same isolated test passed with normal host process permissions.

Gateway provisioning is checked through isolated file/HTTP fixtures and source
review, including bounded manifest verification, swapped-directory rejection,
private release staging, managed-file rollback and readiness probes. Live Linux
package installation, public ACME issuance, real SSH deployment
and OS service activation with the new unified installer remain unverified. No
maintained Gateway/Home service was reinstalled by these installer checks.

The full native suite also exposed an obsolete Go metric-oracle assertion for
Darwin `top` output. The historical fixture is retained; current compatibility
checks reject that retired input while native parser tests cover the replacement
`iostat` interval format. No collector behavior was changed for this test repair.

## English and Korean interface

The web/PWA and native installer default to English and support an explicit Korean
choice. The web preference is browser-local; the installer accepts `--lang` and
`HMUX_LANG`, including explicit forwarding through SSH and privilege elevation.
Both README editions link to each other at the top.

Local validation covered:

- TypeScript, formatting and all 184 web tests, including blocked preference
  storage, localized DOM ownership, stale bindings, service-worker language
  fallback and notification identity checks.
- Strict workspace release Clippy, 24 native entry-point unit tests, three native
  integration tests, 15 CLI/PTY checks and five macOS ARM64 bundle checks.
- Synthetic Chromium login, settings and first-account/TOTP forms at desktop and
  390×844 widths. Switching retained mounted inputs, setup drafts and TOTP state;
  a mounted xterm stayed the same instance and no new WebSocket was created.
  Language persisted across reload and the active worker URL matched the choice.
  English/Korean screenshots were reviewed for layout and horizontal overflow.

Independent review caught a non-UTF-8 installer path regression; the locale parser
now preserves Unix path bytes, with a regression assertion. Review also found a
pre-existing PWA install-button callback retention issue. Install events now query
mounted controls; a synthetic browser prompt worked and the retired button was
garbage-collected after its view was removed. These checks used synthetic accounts
and local assets. They did not deploy maintained services,
register OS startup services or establish physical-device acceptance.

## Provider authentication clarity

Settings → AI tools separates existing Home CLI authentication from optional API
key changes. A signed-in CLI without a launch profile can be registered without
another login. Gemini key removal also updates its selected authentication method;
an unrelated custom method is preserved.

The Home library suite passed 215 tests (three opt-in checks ignored). The final
11 focused provider checks include JSON and Protobuf registration, credential-byte
preservation, refusal during an active setup job, custom-profile preservation and
Gemini key/account transitions. Strict Home Clippy passed. All 188 web tests,
TypeScript, formatting and the web build passed.

Synthetic Chromium checks at desktop and 390×844 widths verified collapsed API
key entry for existing CLI logins, registration without login/key mutations,
explicit key removal before account sign-in, and language switching without losing
an API-key draft. English and Korean layouts were reviewed. This used mocked
browser responses and isolated native fixtures; real credentials and maintained
services were unchanged. These results do not establish physical-device acceptance
or a deployment.

## Maintained deployment

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
