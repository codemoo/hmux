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

## Maintained deployment

The stability update replaced the maintained Linux Gateway and macOS Home/helper
on 2026-09-24 UTC (2026-09-25 KST). The maintained combination is **Rust Gateway +
Rust Home + Rust helper**. The earlier source retirement alone did not deploy.

| Component | Recorded activation (UTC) | Executable SHA-256 |
| --- | --- | --- |
| Linux Gateway | 2026-09-24 17:47 | `e48eeca9313cec1b2f699889ae9b02cbd85a59128c7e64017afd2fde49445eea` |
| macOS helper | 2026-09-24 17:51 | `d5165c04f19dcfd26044b05181e3489abd6877d910757175285a656c94c99af5` |
| macOS Home | 2026-09-24 17:51 | `e2217a5a606277d532b2fa654d8149411f67c12a42df423ad990449411ceed61` |

Gateway release: `rust-stability-20260924T173000Z`. The existing web assets were
retained byte-for-byte. Public asset hashes, no-store/PWA CSP and five anonymous
API 401 barriers passed. Home first catalog publication, subsequent authenticated
workspace requests and original tmux preservation were verified. Detailed service,
configuration and session receipts remain private.

Timestamped rollback artifacts remain available. Binary rollback must keep current
configuration, credentials, account policy and session state; an older state snapshot
can revive revoked access. See [Rollback](ROLLBACK.md). Initial live connectivity
does not establish physical reboot/login, full browser/TOTP or long-running acceptance.

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
