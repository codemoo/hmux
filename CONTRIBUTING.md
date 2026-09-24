# Contributing

HMux provides web/PWA access to a trusted Home host. Read [AGENTS.md](AGENTS.md)
and [architecture](docs/ARCHITECTURE.md) before changing authentication, tmux
lifecycle or input. Report vulnerabilities through [SECURITY.md](SECURITY.md).

## Development

Use the pinned Rust toolchain, Node.js 22+, Python 3, ShellCheck and jq. tmux is required on
Home and for isolated integration tests. Xcode, desktop app SDKs and SSH provisioning
are not build prerequisites. Configuration examples contain no deployment credentials.

```sh
cargo fetch --locked
npm ci --prefix web
make check
make integration
make build
```

`make check` runs Rust formatting, Clippy, locked Rust tests, ShellCheck and the
TypeScript/frontend checks. `make build` packages production web assets and the
native Gateway/Home/helper bundle for the build host. Set `HMUX_RUST_TARGETS` only
when the required Rust targets, linker and platform SDK are installed. `make integration`
checks the actual native pair, WSS runtime, CLI/hooks and isolated tmux lifecycle in
private fixtures; it never targets an existing user's tmux server. The hook test uses
a temporary HOME. `make bundle-check` verifies a built bundle and requires
`HMUX_RUST_BUNDLE`.

`make shfmt-check` optionally checks shell formatting. It installs the pinned shfmt
3.13.1 release into ignored `.tools/`, verifies its SHA-256 before use and reuses the
verified binary. This tool needs curl and a SHA-256 utility; override `SHFMT_DIR` to
keep it in another developer-owned directory.

Use the tracked `Cargo.lock` and [Rust verification](tests/RUST.md) for detailed
native package, workload and OS checks. Historical Go/Rust comparisons remain useful
reference evidence but are not contributor dependencies. [Rust runtime status](docs/RUST_MIGRATION.md)
distinguishes automated coverage from pending device and soak acceptance.

See [tests/README.md](tests/README.md) for coverage and opt-in recovery checks.
Report browser emulation separately from physical device acceptance. Preserve the
accepted IME/clipboard behavior described in [web/AGENTS.md](web/AGENTS.md).

## Pull requests

Describe the concrete problem, resulting behavior and checks performed. Keep
unrelated work separate, preserve private state and rollback paths, and update
the owning reference. Add meaningful regression coverage for behavior/security
changes. Never commit build outputs, caches, credentials, private topology or
transcripts. Use synthetic data in screenshots and fixtures.
