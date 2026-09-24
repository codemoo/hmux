# Threat model

HMux grants trusted users interactive shell authority on one Home host. Account
profiles isolate authentication and tab layouts, not Home files, processes or
provider credentials. A compromised authenticated browser or gateway can exercise
that user's terminal authority. HMux is not a sandbox for untrusted tenants.

## Boundaries and defenses

- Public browser traffic uses HTTPS/WSS through a trusted reverse proxy. The Rust
  gateway binds loopback and enforces exact Host/origin, CSRF and WebSocket origins.
  Home connects outbound using a private token and normal TLS certificate checks.
- Password hashes, optional per-account TOTP, replay protection, rate limits and
  persistent hashed login tokens protect entry. Revocation is persisted before
  success and closes that login's connections. Session storage failures fail closed.
- Home operations use validated profiles and argument arrays. Exact tmux
  `{id, created_at}` identities and verified provider bindings prevent accidental
  access to recycled IDs or unrelated conversations.
- Temporary grouped terminal views own only their PTY/view. Disconnects, redraws
  and tests never terminate unrelated work. Recovery cleans only verified resources
  created by its own construction intent.
- Bounded frames, queues, deadlines, connection limits and output credits prevent
  an unresponsive browser from creating unlimited memory/connection growth.
- Uploads verify the destination session before and after transfer, enforce byte
  and file limits, use opaque private names and expire after three hours. Paths
  are quoted for paste; uploading never presses Enter or executes a file.
- Markdown uses allowlisted DOM construction and literal text. Raw HTML is inert;
  external links require explicit opening with noopener/noreferrer. Remote content
  cannot set application authority. No transcript/credential caching in the worker.
- Runtime state uses private owner-controlled files, symlink checks, locks and
  atomic replacement. Backups remain private and outside release assets.

## External services and residual risks

Provider usage uses the Home user's existing authentication. Push delivery uses
the browser's push provider with bounded subscription validation. Login location
lookup may send a public source IP to the configured external location service;
it does not send credentials. Provider/push/location availability is not guaranteed.

Home transcripts and terminal output can contain secrets; authenticated terminal
users can read them. Frontend diagnostics are bounded and sanitized but should
still be reviewed privately before sharing. No telemetry/log contains intentional
terminal-byte or credential collection. Private host state must never enter Git.

The current code targets the tested macOS Home/Linux gateway deployment. Unit,
race and browser tests reduce regression risk but do not establish device-wide
behavior or a complete security audit. Report vulnerabilities via [SECURITY.md](../SECURITY.md).
