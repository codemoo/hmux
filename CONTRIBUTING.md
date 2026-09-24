# Contributing

HMux provides web/PWA access to a trusted Home host. Read [AGENTS.md](AGENTS.md)
and [architecture](docs/ARCHITECTURE.md) before changing authentication, tmux
lifecycle or input. Report vulnerabilities through [SECURITY.md](SECURITY.md).

## Development

Use Go 1.24+, Node.js 22+, ShellCheck and jq. tmux is required on Home and for
isolated integration tests. Xcode, desktop app SDKs and SSH provisioning are not
build prerequisites. Configuration examples contain no deployment credentials.

```sh
go mod download
npm ci --prefix web
make check
make integration
make build
```

`make check` runs Go formatting, all unit/race/vet checks, the vendored usage
collector's checks, ShellCheck, TypeScript and frontend tests. `make build` also
builds production assets and Linux gateway/macOS Home binaries. `make integration`
checks the Home installer in a temporary directory and uses a private tmux socket for session creation and browser-view lifecycle tests;
it never targets an existing user's tmux server. The hook test uses temporary HOME.
`make shfmt-check` optionally checks shell formatting. It installs the pinned
shfmt 3.13.1 release into ignored `.tools/`, verifies its SHA-256 before use and
reuses the verified binary. This tool needs curl and a SHA-256 utility, not Go;
override `SHFMT_DIR` to keep it in another developer-owned directory.

The native runtime is being migrated to Rust. Use the pinned
`rust-toolchain.toml` and tracked `Cargo.lock`; start with `make rust-check`,
`make rust-compat` and `make rust-build`. Default build outputs remain Go;
the maintained Rust deployment trials do not change those commands.
[Current migration status](docs/RUST_MIGRATION.md) owns the execution queue;
[Rust verification](tests/RUST.md) holds detailed commands, prerequisites and
claim limits for native packages, compatibility, workloads and OS checks.

See [tests/README.md](tests/README.md) for coverage and opt-in recovery checks.
Report browser emulation separately from physical device acceptance. Preserve the
accepted IME/clipboard behavior described in [web/AGENTS.md](web/AGENTS.md).

## Pull requests

Describe the concrete problem, resulting behavior and checks performed. Keep
unrelated work separate, preserve private state and rollback paths, and update
the owning reference. Add meaningful regression coverage for behavior/security
changes. Never commit build outputs, caches, credentials, private topology or
transcripts. Use synthetic data in screenshots and fixtures.
