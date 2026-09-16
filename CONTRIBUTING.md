# Contributing

HMux is a self-hosted terminal project. Please read [AGENTS.md](AGENTS.md) and the
[architecture](docs/ARCHITECTURE.md) before changing authentication, SSH, tmux
lifecycle or terminal input. Report vulnerabilities through [SECURITY.md](SECURITY.md).

## Development setup

Use Go 1.24 or newer and Node.js 22 or newer. Native UI work needs an Apple Silicon
Mac and the pinned build prerequisites in [macos/HMux/README.md](macos/HMux/README.md).
Release verification requires OpenSSL 3 (`brew install openssl@3 jq shellcheck`
on macOS). Put `$(brew --prefix openssl@3)/bin` before the system LibreSSL in PATH;
LibreSSL does not support the Ed25519 `pkeyutl -rawin` verification used here.
Examples under `config/` and `deploy/web/` are templates, not production settings.

```sh
go mod download
npm ci --prefix web
go test ./...
go vet ./...
(cd third_party/token-terrier-server && go test ./...)
npm run check --prefix web
npm test --prefix web
npm run build --prefix web
```

For native work, run `make native-smoke`. `make check` runs core/race/web checks. `make legacy-check` runs ShellCheck,
shfmt and archived isolated integration checks and requires their tools. Do not enable a
live-test environment variable against a personal tmux server. Existing user
sessions must remain untouched.

## Pull requests

Describe the problem, changed behavior and checks performed. Keep unrelated
formatting or refactors separate. Preserve user settings and rollback paths.
Include regression tests for behavior or security changes, and update the owning
reference document. Screenshots must use synthetic data and no real credentials,
host names or conversations. Never commit build outputs or dependency caches.

The initial repository history is a component-based import of the existing
application. Future changes should be incremental commits with descriptive titles.
