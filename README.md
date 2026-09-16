# HMux

**One host for Codex, Claude Code and shells. Pick up your sessions from anywhere.**

HMux keeps long-running terminal sessions on a host you control and makes them
available through a lightweight web interface on desktop, phone and tablet.
Close a browser, switch devices or reconnect later: the work stays on the host.
The primary client is the web/PWA app. A separate macOS app is retained as an
optional client; the former standalone terminal UI is archived.

## How it works

```text
Desktop / phone / tablet browser
            │ HTTPS / WSS
            ▼
       Web gateway
            ▲
            │ outbound WSS
            │
        Your host
     tmux ─┬─ Codex
           ├─ Claude Code
           └─ Shell
```

The host owns terminal processes, provider authentication and files. The gateway
handles web authentication and connections; it does not run your agent workloads.
The current tested host setup is macOS with a Linux HTTPS gateway. Do not assume
an arbitrary host OS has the same support just because a Go binary compiles there.

## Features

- Persistent tmux sessions with desktop/mobile tabs, search and reconnects.
- Password login, optional per-account TOTP, persistent logins and session revocation.
- Responsive web/PWA interface, Korean input, selection/copy and explicit link opening.
- File attachments with a three-hour retention window on the host.
- Provider usage, host metrics and a filtered Codex conversation reader.
- Shared host state and recovery of verified tmux/provider sessions after a reboot.

A web account grants terminal access to the connected host. Multiple accounts are
for trusted collaborators: they share host files and shell authority. HMux is not
an isolation layer for untrusted tenants. See [SECURITY.md](SECURITY.md).

## Get started

```sh
git clone https://github.com/codemoo/hmux.git
cd hmux
```

Follow the [web setup guide](docs/WEB.md) to configure your own host, gateway,
domain and credentials. No hosted service, private configuration or maintainer
infrastructure access is included. Build prerequisites are Go 1.24+, Node.js 22+
and tmux on the host. HTTPS is required for remote web access.

```sh
npm ci --prefix web
make web-build
```

This creates the web assets and the currently supported Linux gateway/macOS host
binaries under `dist/`. Building does not deploy or alter existing sessions.
Native macOS source builds have additional Xcode/Ghostty prerequisites; see
[macos/README.md](macos/README.md). Notarized public app downloads are not yet provided.

## Repository map

| Location | Responsibility |
| --- | --- |
| `web/` | Primary desktop/mobile web and PWA client |
| `cmd/hmux-web/`, `internal/webgateway/`, `deploy/web/` | Gateway, host connector and deployment templates |
| `internal/`, `cmd/hmux-agent/`, `cmd/hmux-control/` | Shared host, SSH, tmux, identity, recovery and administration |
| `macos/`, `cmd/hmux/` | Optional native app and its Go bridge/compatibility entrypoint |
| `archive/terminal/` | Retired fzf/framed terminal UI, configuration and regression tests |
| `third_party/` | Licensed in-tree usage collector |
| `docs/` | Architecture, setup, security and validation references |

The archived UI still has a small number of compatibility imports from shared
client/agent entrypoints. It is kept buildable; it is not the primary product.

## Development and releases

Read [CONTRIBUTING.md](CONTRIBUTING.md) for checks and [AGENTS.md](AGENTS.md) for
change rules. CI checks Go, native contracts and the web client. Public-source and
binary-release readiness are tracked separately in [RELEASING.md](docs/RELEASING.md).
Third-party notices are preserved in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

The first published commits import the existing project by component. They do
not recreate its earlier development history. See the [documentation map](docs/README.md)
for detailed behavior and [validation evidence](docs/VALIDATION.md) for limitations.

## License

The HMux source-license choice is pending; public visibility alone is not a reuse
license. Included third-party components retain their own licenses and notices.
See [release policy](docs/RELEASING.md#source-license).
