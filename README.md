# HMux

[English](README.md) | [한국어](README.ko.md)

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
- English and Korean interface and installer; English is the default.
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

Follow the [installation guide](docs/OPERATIONS.md) to configure your own host, gateway,
domain and credentials. The current distribution starts from source; building needs
the pinned Rust toolchain, Node.js 22+ and Python 3. Once built, the native installer
needs none of those tools. The Home host needs tmux and your chosen provider CLIs.
Remote access requires your own HTTPS Gateway and connector token.

```sh
make build
```

This packages the native host bundle under `dist/web-<platform>/`, such as
`dist/web-darwin-arm64/` or `dist/web-linux-amd64/`. It does not deploy or alter
existing sessions. Build another supported target only when its Rust target, linker
and platform SDK are available, for example through `HMUX_RUST_TARGETS`; see
[Operations](docs/OPERATIONS.md#build-and-install).

Run the installer from the bundle for your platform:

```sh
./dist/web-darwin-arm64/hmux-web install
```

Choose **Gateway**, **Home**, or **both**, then **this machine** or an **SSH server**.
Gateway requires Linux/systemd; Home supports macOS and Linux. Remote setup checks
the target OS/CPU and transfers a matching native bundle. Gateway HTTPS can use
managed Nginx/Let's Encrypt or your existing reverse proxy. Create the first account
and configure TOTP in the browser using the private one-time setup token.

Choose the web language on the login screen or in **Settings → Terminal → Language**.
The choice is saved on this browser and changes the interface without reconnecting
terminals. The installer offers a language choice; use `--lang en` or `--lang ko`
to choose explicitly (`HMUX_LANG` is also supported).

Same-host setup passes connection details automatically. Split-host setup uses a
private connection file. Home asks for a workspace (default `~/.hmux`) and optional
automatic startup; existing paths and agent sessions are preserved. See
[installation options and prerequisites](docs/OPERATIONS.md#build-and-install).

macOS here refers to the host running tmux. An optional
[native Home service](docs/OPERATIONS.md#automatic-home-startup-macos-and-linux)
starts the connector at login and restarts it after exit, so no terminal window
needs to stay open. macOS uses launchd; Linux uses a systemd user service.

HMux now ships a Rust Gateway, Home connector and helper. Historical Go/Rust trial
results remain evidence with their stated limits; they do not establish outstanding
soak or physical-device acceptance. See [Rust runtime status](docs/RUST_MIGRATION.md).

## Repository map

| Location | Responsibility |
| --- | --- |
| `web/` | Desktop/mobile web and PWA interface |
| `crates/hmux-gateway/`, `crates/hmux-home/`, `crates/hmux-web/` | Gateway, Home connector and native entrypoints |
| `crates/hmux-agent/`, `crates/hmux-service/`, `crates/hmux-install/` | Administration, service lifecycle and durable installation |
| `crates/hmux-core/`, `crates/hmux-model/`, `crates/hmux-usage/` | Shared contracts, bounded native primitives and Home usage collection |
| `proto/`, `tests/RUST.md` | Versioned Home protocol and native verification |
| `third_party/` | Retained third-party attribution and licenses |
| `docs/` | Architecture, setup, security and validation references |

## Development and releases

Read [CONTRIBUTING.md](CONTRIBUTING.md) for checks and [AGENTS.md](AGENTS.md) for
change rules. CI checks the Rust runtime, native lifecycle fixtures, the web client,
generated protocol types and Rust dependency policy.
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
