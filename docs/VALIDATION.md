# Validation status

Evidence summary updated 2026-09-26. Deployment observations describe one maintained
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
