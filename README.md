# HMux

**One host for Codex, Claude Code and shells. Pick up your sessions from anywhere.**

HMux keeps long-running terminal sessions on a host you control and makes them
available through a lightweight web interface on desktop, phone and tablet.
Close a browser, switch devices or reconnect later: the work stays on the host.
The web/PWA app is the only user interface.

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
Install the Home connector and optional administration helper using
[Operations](docs/OPERATIONS.md). macOS here refers to the host running tmux.

## Repository map

| Location | Responsibility |
| --- | --- |
| `web/` | Desktop/mobile web and PWA interface |
| `cmd/hmux-web/`, `internal/webgateway/`, `deploy/web/` | Gateway, host connector and deployment templates |
| `internal/home/`, `internal/agent/`, `cmd/hmux-agent/` | Local tmux services, administration and workflow hooks |
| `internal/catalog/`, `internal/recovery/` | Session identity, provider binding and reboot recovery |
| `third_party/` | Licensed in-tree usage collector |
| `docs/` | Architecture, setup, security and validation references |

## Development and releases

Read [CONTRIBUTING.md](CONTRIBUTING.md) for checks and [AGENTS.md](AGENTS.md) for
change rules. CI checks the Go host/gateway, isolated tmux lifecycles and the web client.
Source and deployment procedures are tracked in [RELEASING.md](docs/RELEASING.md).
Third-party notices are preserved in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

The first published commits import the existing project by component. They do
not recreate its earlier development history. See the [documentation map](docs/README.md)
for detailed behavior and [validation evidence](docs/VALIDATION.md) for limitations.

## License

The HMux source-license choice is pending; public visibility alone is not a reuse
license. Included third-party components retain their own licenses and notices.
See [release policy](docs/RELEASING.md#source-license).
