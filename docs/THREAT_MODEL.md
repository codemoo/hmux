# Threat model

| Threat | Defense | Limit |
| --- | --- | --- |
| Compromised native SSH DMZ or jump | No Home private key; ProxyJump; Home host-key verification; no forwarding | Native SSH relay can deny traffic and observe connection metadata |
| Compromised web gateway | Dedicated unprivileged service, loopback backend, HTTPS, bounded authenticated protocol, no Home SSH key | Gateway terminates TLS/auth and can control Home terminal input; it is a trusted execution principal, unlike the native TCP relay |
| Web login/CSRF/session theft | Password + TOTP, persisted replay prevention, per-source throttling, hash concurrency cap, Secure/HttpOnly/Strict cookies, exact Origin/Host and CSRF checks | A stolen live session or compromised gateway grants interactive terminal access; upstream network DoS remains possible |
| Malicious DMZ artifact | Role-bound Ed25519 manifest, size/SHA-256 checks, immutable cache and rollback | Compromised signer remains trusted |
| Shell installer source compromise | Trusted Home SCP connection, archive/checksum and bundle validation | Archive and checksum share Home trust; this path does not verify a DMZ manifest |
| Unsafe native ZIP | Bounded paths/types/extraction, contained symlinks, bundle/CPU/version and deep signature checks | Private build is ad-hoc signed, not Developer ID notarized |
| SSH command injection | Allowlisted alias/path/arguments, protected stable ID, exec argument arrays | OpenSSH still uses the remote login shell; every new argument needs review |
| Stale native session identity | ID plus creation time, expected-identity mutation checks | tmux creation time has second precision; CLI legacy state has narrower guarantees |
| Hostile metadata | Protocol/count/size limits, text sanitization, validated runtime/workflow fields | Project paths can be sensitive on screen |
| WSS impersonation/replay | IPv4 loopback only, exact ephemeral TLS leaf pin, one-use token, Origin rejection | Compromise of the app process defeats in-memory isolation |
| Stale or orphaned streams | Sequence/heartbeat lease checks, bounded reconnect, parent-bound foreground helpers | Connectivity loss still interrupts terminal connections |
| File-drop leak/injection | Generic Home names, private bounded spool, identity/surface/epoch checks, POSIX quoting without Enter | Uploaded bytes remain on Home until opportunistic expiry |
| Local state corruption | Owner/mode/symlink checks, locks, bounded atomic writes and directory fsync | A compromised Home user can access their own state |
| Provider credential exposure | Read-only collection on Home, no credential refresh/transfer, bounded usage fields, owner-requested codex-lb aliases and cswap email labels | Home credential compromise is outside transport protection |
| Wrong conversation exposed | Exact tmux session identity, process/open-file association, rechecks, bounded public-message filtering | Missing or ambiguous associations cannot be displayed; Home-user compromise remains outside this boundary |
| UI terminates work accidentally | Tab close/hide separated from confirmed identity-checked termination | Explicit termination ends processes |
| Native shared-window resize | Grouped-view behavior documented; preserve other clients | Native tabs share tmux window sizing; no global handoff guarantee |
| Termius data loss | No private storage access, no automatic deletion, manual import gate | Import/Vault behavior depends on supported app UI |
| Inventory secret commit | Placeholder examples and private inventory storage | Free-text values still require review |

## Execution and configuration boundaries

Go owns SSH/tmux execution. Swift invokes fixed bridge operations.
No eval, unchecked remote script pipe, host-key bypass, agent forwarding,
Mac launch agent is required. The owner-authorized web client additionally uses
Linux Nginx public HTTPS/WSS with a loopback-only Go backend; see [web security](WEB.md).

Original tmux session options and bindings are not used for product UI state.
Native Go cleanup removes only its own temporary grouped view. CLI frames use
isolated disposable servers. Old target-UI cleanup is an explicit migration,
never an automatic test against user sessions.

Native, workflow and file-stage commands reject stale identities. Legacy
ID-only CLI last/tab state does not yet carry creation time end-to-end; do not
claim the native identity guarantee for every compatibility command.

Release signing keys stay on a trusted build host. The DMZ stores only public
keys, signatures and artifacts. The local install/update directory must be
owner-controlled. Failed validation preserves the selected good release.

## Privacy

Workflow persistence excludes prompt/response text, transcripts, cwd, full
argv, pane contents, tool inputs/results and raw provider IDs. Ordinary
catalog metadata does include workspace paths. Usage credentials stay on
Home and are never serialized remotely. Diagnostic output and screenshots
must omit private topology and account data.

Explicit conversation reading returns public Codex user/assistant messages to the
owner’s app through the existing authenticated boundary. It excludes internal
reasoning and tool records and never puts bodies in catalog/workflow storage,
diagnostics or disk cache. Clipboard copy is an explicit user action.

Claude cswap emails are authorized display labels; Codex uses codex-lb aliases
without email fallback. Native cswap reads existing account metadata and usage
cache, not credential backups, and matches cache identity before showing quota.
No account-switch command, token refresh or new background service is introduced.

## Dependencies

Direct Go dependencies are pinned in `go.mod`/`go.sum`: TOML, WebSocket,
PTY, x/sys and x/term, plus the vendored Token Terrier module and its own pins.
Native Ghostty, Zig and SwiftPM artifacts are described in
`macos/HMux/Dependencies.lock.json`. The app's third-party notice is shipped
with the bundle; complete public-distribution notices/SBOM and notarization
remain separate gates.

Use `go version -m` on a built helper and optionally `govulncheck ./...` for
dependency verification. Tool absence is a not-run result, never a pass.
