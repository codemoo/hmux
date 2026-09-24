# Rust migration — acceptance status

Updated 2026-09-25. This is the **only current control for the Rust migration**.
The native Rust candidate is implemented. The maintained Linux Gateway and macOS
Home/helper now run user-authorized Rust trials. Default builds remain Go. See
[deployment evidence](VALIDATION.md#rust-transition). This source checkpoint retains
Go for default builds, compatibility checks and rollback; Go retirement is pending.

## Scope and completion

**Low memory, web terminal for ai agents.** Replace the Gateway, Home, embedded
usage collector, administration/workflow helpers, service and installer code with
Rust. Keep the TypeScript/xterm.js web/PWA, host tmux and provider CLIs. Home uses
Protobuf v2 with rolling JSON v1 compatibility; no sidecar or Docker is required.

Full completion includes release acceptance, verified Gateway/Home/helper cutover,
Rust-only build/check/docs and Go retirement with retained rollback artifacts.
[Contracts](RUST_CONTRACTS.md) retain every compatibility, security and release gate;
[the fixture inventory](../tests/fixtures/contracts.json) maps their test owners.

## Implemented and accepted locally

| Area | Accepted candidate scope |
| --- | --- |
| Gateway and Home | Authentication/TOTP/revocation, terminal/session lifecycle, uploads, push, diagnostics, Codex/Claude conversations, shared workspace and recovery |
| Usage | Shared in-process OAuth, codex-lb, cswap/account sources and bounded activity collection |
| Transport | Typed snapshots, terminal/file bytes and all 12 action/reply operations; `controls1` negotiation, contextual v1 replies and bounded retained ownership |
| CLI and packaging | Native web/helper commands, installer, rendered launchd/systemd units, native macOS/Linux bundles and mixed-version helper/install checks |
| Resource checks | Native view limits/cleanup, Linux lifecycle churn, scoped sustained comparisons and activity/terminal workloads; whole-product budgets remain unaccepted |

Latest transport acceptance: **57 protocol + 443 Home/Gateway checks on macOS**,
with 15 opt-in skips; Linux has 57 protocol, 22 hub and 47 focused Home checks,
with one opt-in skip. Four changed native role/codec pairs pass per OS. Strict
Clippy, schema/format checks and independent runtime review passed. These counts
are scoped, distinct checks combined across recorded runs, not every release gate.
See [protocol evidence](archive/RUST_PROTOCOL_CHECKPOINTS_2026-09-24.md) for hashes,
failures/reruns and the macOS Go-core/Linux non-race oracle limitations.

The latest tested release executables include typed controls. The maintained trials
use the exact verified Linux/macOS executables with refreshed notices and unchanged
web assets. Other distribution bundles and earlier frozen soaks predate typed controls:
refresh and verify them before a new acceptance run.

## Remaining acceptance work

| Deliverable | What remains | Environment or result needed |
| --- | --- | --- |
| Exact release bundle | Gateway and macOS Home/helper trials packaged and deployed; finish remaining distribution package/pair checks | Native macOS and Linux; use the artifact that will actually be trialled |
| Resource and fault behavior | Remaining concurrent KDF/upload/typing and whole-Home workloads, storage/failure behavior and measured budget decisions | Separate Gateway/Home metrics, matched successful workloads; no inferred memory claims |
| Service and rollback | Full fixed-name service CLI, current-state rollback, physical login/reboot | Isolated OS account with a real launchd GUI session or systemd user manager |
| Browser/device use | Desktop Safari/Chrome, physical iOS/PWA and Android input, background/resume and reconnect | Isolated Rust test instance and the device checklist in the verification guide |
| Long-running acceptance | Investigate the interrupted macOS candidate run, complete valid 24h runs, then freeze the RC for 72h | Fresh progress and final success records bound to exact binary hashes |
| Release | Independent release review, remaining CI/publication checks and first-party license decision | Coordinated cutover, deployed hashes/services, current-state rollback artifacts, then Go retirement |

**Next step:** observe the deployed Gateway/Home/helper trials and complete remaining
authenticated/device acceptance. Follow the
[test entry point](../tests/RUST.md#test-entry-point) and
[device acceptance checklist](../tests/RUST.md#browser-and-device-acceptance).
Reuse accepted checks when source/artifacts are unchanged. Fix material failures;
do not add feature scope or rerun unchanged suites solely to rebuild a count.

## Soak and environment limits

- Linux 24h candidate: fresh progress observed on 2026-09-24 at about three hours;
  the elapsed-time gate has not passed.
- macOS 24h candidate: **interrupted, not passed**. Progress stopped near 2h25m,
  the log ends with an incomplete traceback, and the runner/guard/Gateway/Home
  PIDs are absent. The stored `running` label is stale; the cause is unconfirmed.
  Preserve the evidence and investigate before scheduling a replacement run.
- The final 72h RC soak has not started. Existing candidate runs use older frozen
  binaries; preserve their logs and never relabel them as final-RC acceptance.

Temporary HOME/XDG paths do not isolate fixed service names or real-UID process
scans. A suitable isolated service-test account has not been verified on either OS;
do not run the fixed-name service CLI against the production account. Browser
trials also need separate state, credentials, workspaces and disposable tmux sockets.
Existing user tmux/provider sessions must remain untouched.

## References

- [Verification](../tests/RUST.md): runnable checks and device acceptance procedure.
- [Contracts](RUST_CONTRACTS.md): durable requirements; [benchmarks](../bench/hmux/README.md): measurements and claim limits.
- [Implementation history](archive/RUST_MIGRATION_HISTORY_2026-09-24.md): preserved detailed checkpoints; [protocol history](archive/RUST_PROTOCOL_CHECKPOINTS_2026-09-24.md): typed transport evidence.
- [Deployment validation](VALIDATION.md): maintained Rust Gateway/Home/helper trials; it does not own the Rust queue.

Private logs, source snapshots and recovery records stay outside tracked docs.
Update current status here; retain dated evidence with its existing owner.
