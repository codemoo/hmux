# HMux

**Low memory, web terminal for AI agents.**

HMux keeps long-running terminal sessions on a host you control and makes them
available through a lightweight web interface on desktop, phone and tablet.
Close a browser, switch devices or reconnect later: the work stays on the host.
The web/PWA app is the only user interface.

Low memory overhead is a core design requirement. HMux favors native binaries,
shared collectors and bounded terminal buffers. Agent CLIs, tmux, authentication
and working directories stay on the host; Docker is not required. Installation
should be simple, with as few runtime dependencies as possible.

## Memory

In an isolated Linux comparison of retained deployed artifacts, the Rust + Protobuf
gateway/Home pair used **55–57% less memory** than Go + JSON (median PSS, three runs each):

| Gateway + Home | Go | Rust |
| --- | ---: | ---: |
| Connected idle | 24.88 MiB | 11.08 MiB |
| After 10,000 terminal echoes | 25.65 MiB | 11.07 MiB |

This measures HMux processes with synthetic host tools; browser, tmux and agent CLI
memory is excluded. See [workload, artifacts and limits](bench/hmux/README.md#deployed-artifact-memory-comparison-2026-09-25).

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
an arbitrary host OS has the same support just because a binary compiles there.

## Features

- Persistent tmux sessions with desktop/mobile tabs, search and reconnects.
- Password login, optional per-account TOTP, persistent logins and session revocation.
- Responsive web/PWA interface, Korean input, selection/copy and explicit link opening.
- File attachments with a three-hour retention window on the host.
- Provider usage, host metrics and a filtered Codex/Claude conversation reader.
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
An optional [native Home service](docs/OPERATIONS.md#automatic-home-startup-macos-and-linux)
starts the connector at login and restarts it after exit, so no terminal window
needs to stay open. macOS uses launchd; Linux uses a systemd user service.

The native Rust runtime is implemented and runs the maintained Gateway/Home/helper
trials. The default setup above still builds Go while release acceptance continues.
For current status and the isolated Rust build/test entry point, start with
[Rust migration](docs/RUST_MIGRATION.md).

## Repository map

| Location | Responsibility |
| --- | --- |
| `web/` | Desktop/mobile web and PWA interface |
| `cmd/hmux-web/`, `internal/webgateway/`, `deploy/web/` | Gateway, host connector and deployment templates |
| `internal/home/`, `internal/agent/`, `cmd/hmux-agent/` | Local tmux services, administration and workflow hooks |
| `internal/catalog/`, `internal/recovery/` | Session identity, provider binding and reboot recovery |
| `third_party/` | Licensed in-tree usage collector |
| `crates/`, `proto/`, `tests/RUST.md` | Rust native runtime, versioned Home protocol and isolated verification |
| `docs/` | Architecture, setup, security and validation references |

## Development and releases

Read [CONTRIBUTING.md](CONTRIBUTING.md) for checks and [AGENTS.md](AGENTS.md) for
change rules. CI checks Go/Rust runtimes and compatibility, native lifecycle fixtures,
the web client, generated protocol types and Rust dependency policy.
Source and deployment procedures are tracked in [RELEASING.md](docs/RELEASING.md).
Third-party notices are preserved in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Contributors

- [Kim Ji Yu (@Banal972)](https://github.com/Banal972) — Linux Home metrics,
  provider setup and web session lifecycle improvements.

The first published commits import the existing project by component. They do
not recreate its earlier development history. See the [documentation map](docs/README.md)
for detailed behavior and [validation evidence](docs/VALIDATION.md) for limitations.

## License

The HMux source-license choice is pending; public visibility alone is not a reuse
license. Included third-party components retain their own licenses and notices.
See [release policy](docs/RELEASING.md#source-license).
