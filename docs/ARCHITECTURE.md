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

Both Go and Rust implement the native runtime during migration; the web/PWA
remains TypeScript. The maintained Gateway/Home/helper installation runs Rust
trials, while default build commands still produce Go. The
[Rust migration control](RUST_MIGRATION.md) owns remaining acceptance and Go
retirement; [validation](VALIDATION.md) records actual deployments. The boundaries
below apply to both implementations.

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
| `internal/webgateway/` | Authentication, account profiles, revocation, diagnostics, push and bounded transport |
| `internal/home/` | Local catalog/recovery stream, workspace operations and terminal PTYs |
| `internal/agent/`, `internal/catalog/` | Validated profile creation, session metadata and exact provider bindings |
| `internal/recovery/`, `internal/sharedworkspace/` | Private checkpoints and verified tab continuity |
| `internal/filestage/` | Bounded, private uploads with exact session checks |
| `third_party/token-terrier-server/` | Source-separated provider usage collection on Home |
| `cmd/hmux-agent/` | Headless administration, recovery and optional workflow hooks |
| `deploy/web/` | Gateway service/proxy templates and web/Home build bundle |

Rust maps these boundaries to `crates/hmux-gateway`, `hmux-home`, `hmux-usage`,
`hmux-web` and `hmux-agent`. `hmux-model`, `hmux-core` and `hmux-platform` hold
shared models, bounded process/storage primitives and OS access; `hmux-service`
and `hmux-install` own native lifecycle and installation. `hmux-protocol` implements
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

## Go source boundaries

- `cmd/hmux-web/main.go` parses/dispatches CLI arguments; `init.go`, `serve.go`
  and `connect.go` own credential enrollment, gateway startup and Home startup.
- `cmd/hmux-agent/` separates create, metadata, workflow, setup and diagnostic
  handlers from command dispatch, retaining the existing public commands.
- `internal/webgateway/server.go` owns server lifetime and outer HTTP security
  headers/Host checks. `http_api.go` keeps Origin/login/session/CSRF gates together;
  `http_auth.go`, `http_action.go` and `browser_terminal.go` own their handlers.
- `protocol.go` owns wire messages and bounded peer I/O; `hub.go` owns shared Home
  state, pending requests and fan-out. `home.go` owns connector request/view lifetime;
  `home_collectors.go`, `home_upload.go` and `home_action.go` isolate the shared
  collectors, file transfer and allowlisted local operations.
- `internal/homeservice/` separates OS manager operations (`service.go`), command
  dispatch (`command.go`), installation/adoption (`install.go`) and environment
  validation (`environment.go`).
- `internal/workflow/` separates bounded persistence (`store.go`), hook/report
  intake (`hooks.go`), catalog projection (`catalog.go`) and state validation/pruning
  (`state.go`). File splits do not introduce services or change persistence formats.

Security gates, transactional recovery and private-file validators remain local
to their owners. Avoid introducing a generic storage or CLI framework merely to
reduce repetition where those contracts differ.
