# Rust runtime status

Updated 2026-09-26. HMux now uses Rust for its native Gateway, Home connector,
usage collection, administration helper, installation and service lifecycle. The
web/PWA remains TypeScript. Rust-only source, contributor checks, packaging and
installation are the current product paths; Go source and tooling are not active
runtime or build dependencies.

This cutover does **not** turn earlier trials into passed release gates. Keep the
actual deployment records and measurements in [validation](VALIDATION.md), and keep
durable rules in [runtime contracts](RUST_CONTRACTS.md) and transition evidence in
the [archive](archive/README.md).
Neither document authorizes a deployment.

## Current product path

- `make check` runs Rust formatting, Clippy, locked Rust tests, ShellCheck and web
  checks. `make integration` runs the native pair, runtime WSS, CLI/hooks and
  isolated tmux checks. `make build` packages the native host bundle.
- Bundles are `dist/web-<platform>/` and `dist/hmux-web-<platform>.tar.gz`; they
  contain `hmux-web`, `hmux-agent`, web assets, service/proxy templates, notices and
  a SHA-256 manifest. Non-host builds require an explicit `HMUX_RUST_TARGETS` value
  plus each target's Rust support, linker and platform SDK.
- New installations start with the built bundle's `hmux-web install` guide for
  Gateway, Home or both, locally or over SSH. `install-home` remains the lower-level
  Home installation/upgrade command. The installer preserves configuration, state
  and original tmux/provider processes; service management remains opt-in and uses
  the existing account and explicit PATH. See [Operations](OPERATIONS.md).
- Versioned JSON v1 compatibility and Protobuf v2 remain supported Home transport
  contracts. Configuration, state, authentication and `{id, created_at}` session
  identity contracts remain authoritative.
- Git history and private, timestamped rollback artifacts may retain earlier
  executables and evidence. A rollback must retain current credentials, account
  policy, configuration and session state.

## Follow-up acceptance limits

The remaining work is acceptance and measurement, not a reason to restore a Go
runtime or toolchain:

| Area | Current limit | Needed evidence |
| --- | --- | --- |
| Device/browser use | Desktop and mobile browser/device checks remain scoped to recorded evidence | Fresh isolated Safari/Chrome/iOS/Android/PWA input, reconnect and background/resume checks |
| Long-running behavior | Existing macOS trial was interrupted; the recorded Linux progress was incomplete | New frozen-artifact 24-hour runs and the planned 72-hour RC run with progress and final assertions |
| Resource claims | Measurements are scoped synthetic/native observations | Matched successful Gateway/Home workloads with separate process metrics; no inferred whole-product budget |
| Service lifecycle | Fixed-name service CLI needs isolated real-account acceptance | launchd GUI-session and systemd-user-manager checks that preserve original tmux/provider processes |
| Release review | Review applies only to the recorded frozen artifact | Repeat manifest, hash, notice, deployment and current-state rollback review for each release |

Use [Rust verification](../tests/RUST.md) for runnable checks. Reuse recorded results
only when the source and exact artifact are unchanged. Record failures, skips and
physical-device results separately from automated test coverage.
