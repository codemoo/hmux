# Architecture

HMux has one interface: a TypeScript/xterm.js web app, installable as a PWA.
A Home host owns tmux, Codex/Claude authentication, transcripts and files. A Go
gateway authenticates web users and routes bounded operations to one outbound
Home WebSocket connection. Nginx provides public HTTPS; Home needs no inbound port.

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

The tested deployment is a macOS Home and Linux gateway. macOS support refers to
host services, not a separate graphical application. `hmux-agent` is an admin/hook
helper; the connector calls local Go services directly and never starts a desktop
bridge, terminal selector or remote SSH client.

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
