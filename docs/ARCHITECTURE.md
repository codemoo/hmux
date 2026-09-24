# Architecture

HMux's purpose is **"low memory, web terminal for ai agents"**. Low memory
overhead and simple installation guide architecture and deployment decisions.

HMux has one interface: a TypeScript/xterm.js web app, installable as a PWA.
A Home host owns tmux, Codex/Claude authentication, transcripts and files. The
gateway authenticates web users and routes bounded operations to one outbound
Home WebSocket connection. Nginx provides public HTTPS; Home needs no inbound port.

Native binaries are the default deployment direction, with minimal runtime
dependencies and no required Docker engine or VM. Provider CLIs, tmux, credentials
and workspaces remain on the native host. Shared collection and bounded buffers,
queues and caches keep HMux overhead controlled as browsers and tabs increase.
Memory claims need measurements that distinguish gateway/Home overhead, browser
memory and the agent/tmux workloads; no numerical memory budget is established here.

The native runtime is Rust; the web/PWA remains TypeScript. The
[Rust runtime status](RUST_MIGRATION.md) records current limits and follow-up
acceptance work, while [validation](VALIDATION.md) preserves dated deployment evidence.
The boundaries below apply to the shipped runtime.

```text
Browser/PWA ── HTTPS/WSS ── Nginx ── loopback hmux-web serve
                                           ▲
                                           │ authenticated outbound WSS
                                      hmux-web connect
                                           │
                                   local Home services
                                           │
                                      tmux / providers
```

## Ownership

| Component | Responsibility |
| --- | --- |
| `web/` | Tabs, terminal, conversations, settings, input, uploads and notifications |
| `crates/hmux-gateway/` | Authentication, account profiles, revocation, diagnostics, push and bounded transport |
| `crates/hmux-home/` | Local catalog/recovery stream, workspace operations and terminal PTYs |
| `crates/hmux-agent/` | Validated profile creation, session metadata, recovery and optional workflow hooks |
| `crates/hmux-core/`, `crates/hmux-model/` | Private checkpoints, verified tab continuity, shared models and bounded process/storage primitives |
| `crates/hmux-usage/` | Source-separated bounded provider usage collection in Home |
| `crates/hmux-service/`, `crates/hmux-install/` | Native service lifecycle and durable installation |
| `deploy/web/` | Gateway service/proxy templates and native web/Home bundle |

`hmux-protocol` implements
the typed Home transport in [proto/README.md](../proto/README.md), with Protobuf v2
negotiation and rolling JSON v1 compatibility. Browser HTTP/WebSocket contracts
remain unchanged.

The tested deployment is a macOS Home and Linux gateway. macOS support refers to
host services, not a separate graphical application. `hmux-agent` is an admin/hook
helper; the connector calls local services directly and never starts a desktop
bridge, terminal selector or remote SSH client.

Home can run under an opt-in native user service: launchd on macOS or systemd on
Linux. The manager execs the same connector with a captured, allowlisted environment;
no separate resident supervisor is added. A state-directory lock admits one
connector, and service stop/restart targets only that process, preserving original
tmux/provider work. See [startup and login boundaries](OPERATIONS.md#automatic-home-startup-macos-and-linux).

## Session and connection lifecycle

Every session operation uses `{id, created_at}`. Names and recycled tmux IDs are
not sufficient authority. Provider bindings are resolved from verified Home state;
ambiguous or changing conversations are unavailable rather than guessed.

A browser terminal owns a disposable grouped tmux view sharing the original
session's windows. Its PTY can resize/redraw and close independently. Closing the
view or losing a connection removes only that view, never the original session.
The `@hmux_app_view` marker and `hmux-app-view-` name prefix are retained as an
internal compatibility detail so views created by earlier web connectors remain
hidden during upgrades. They do not enable a native client.

One terminal is live per visible browser, with a gateway-wide limit of eight.
Connection generations discard stale callbacks. Explicit deadlines, cancellation,
bounded queues and output credits prevent slow rendering from growing queues
without limit. Catalog and usage streams are shared across browser clients.
Completion detection observes every catalog fetch before unchanged snapshots are
suppressed; notifications carry the target tab identity.

Recovery checkpoints only verified sessions. On a changed boot it recreates
missing sessions and resumes exactly bound providers, then remaps saved tabs by
verified old-to-new identities. See [RECOVERY.md](RECOVERY.md).

## Authentication and storage

The gateway owns password/TOTP policy, persistent logins, session revocation and
account-scoped profiles. Secure cookies are HttpOnly and SameSite=Strict. Mutations
require CSRF protection and exact origins. Private files remain outside releases.
Accounts share Home shell authority and are for trusted collaborators, not hostile
tenants. See [THREAT_MODEL.md](THREAT_MODEL.md) and [WEB.md](WEB.md).

Home configuration uses `home.toml` plus an allowlisted profile inventory. The
loader accepts an existing `client.toml` when the new file is absent, and decodes
known retired keys without using them. Unknown fields still fail. No loader edits
existing files. See [MIGRATION.md](MIGRATION.md).

## Rust source boundaries

- `crates/hmux-web/` owns `hmux-web` command dispatch, enrollment, Gateway startup,
  Home startup and the built-bundle `install-home` command.
- `crates/hmux-gateway/` owns HTTP/static serving, authentication, account state,
  Home transport, bounded browser flow, uploads, push and diagnostics.
- `crates/hmux-home/` owns connector lifetime, catalog/usage collection, session and
  provider binding, recovery, workspace operations and disposable terminal PTYs.
- `crates/hmux-agent/` owns headless create, metadata, setup, recovery, workflow and
  diagnostic commands; `crates/hmux-service/` owns OS manager operations and verified
  adoption; `crates/hmux-install/` owns private atomic binary replacement.
- `crates/hmux-core/`, `crates/hmux-model/` and `crates/hmux-protocol/` hold bounded
  storage/process primitives, validated contracts and versioned peer I/O.

These source boundaries do not introduce extra resident services or change persistent
configuration, session identity or protocol contracts.

Security gates, transactional recovery and private-file validators remain local
to their owners. Avoid introducing a generic storage or CLI framework merely to
reduce repetition where those contracts differ.
