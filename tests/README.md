# Verification

HMux is a Rust native Gateway/Home/helper with a TypeScript web/PWA UI.
No Go toolchain is required for normal builds, checks or installation.

| Check | Scope |
| --- | --- |
| `make check` | Rust format/Clippy/unit and contract tests, ShellCheck, TypeScript/frontend tests |
| `make integration` | Native Gateway/Home pair, Home WSS/codecs, CLI enrollment, managed hooks and isolated tmux views/session creation |
| `make build` | Both native binaries, web assets, manifest and dependency notices for this host |
| `HMUX_RUST_BUNDLE=/absolute/bundle make bundle-check` | Manifest, licenses, native install/upgrade/rollback and wrapper behavior |
| `make rust-proto-check` | Checked-in schema regeneration; requires protoc 35.1 |
| `make shfmt-check` | Pinned, checksum-verified shell formatter |

[Native verification](RUST.md) documents prerequisites, optional retained baseline
artifacts and device checks. [Contract fixtures](fixtures/README.md) retain frozen
synthetic prior-version outputs; their names record provenance, not Go dependencies.
Use disposable `hmux-e2e-*` resources and private sockets. Never attach to, rename,
detach or kill original tmux/provider sessions. Temporary HOME does not isolate
fixed-name service registration; service lifecycle tests require an isolated OS account.

A green typecheck or synthetic terminal test is not physical iOS/Android or
Safari/Chrome acceptance. Report device, restart and soak evidence separately in
[validation](../docs/VALIDATION.md).
