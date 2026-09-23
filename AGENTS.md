# Working on HMux

## Scope and references

The product purpose is **"low memory, web terminal for ai agents"**. Low memory
overhead and simple installation are core requirements for architecture and features.

HMux runs Codex, Claude and shells on one Home host. Its only user interface is
web/PWA. macOS remains a supported Home host; browser-native input is part of the web UI.

- `CODEX_HMUX_IMPLEMENTATION_PROMPT_KO.md`: product and security requirements.
- `docs/README.md`: documentation map; `docs/ARCHITECTURE.md`: system boundaries.
- `web/AGENTS.md`: browser/input rules; `docs/WEB.md`: current web behavior.
- `CONTRIBUTING.md`: checks; `docs/OPERATIONS.md`: installation/administration.
- `docs/RELEASING.md`: publication; `docs/VALIDATION.md`: current checks; archive preserves historical evidence.

## Resource and deployment principles

- Prefer native binaries and minimal runtime dependencies; do not require Docker
  or a VM. Provider CLIs, tmux, authentication and workspaces stay on the host.
- Bound buffers, queues, caches and retained state. Share collectors/connections
  where possible; avoid extra processes or polling loops per browser or tab.
- Assess memory impact for new resident services and dependencies. Substantiate
  memory claims with measurements, separating HMux gateway/Home, browser and
  agent/tmux usage; do not present an unmeasured target as an achieved result.
- Go is the current implementation. A future partial or full Rust migration must
  preserve CLI, configuration, protocol, security and session-lifecycle contracts;
  evaluate it against measured resource use and maintainability.

## Change discipline

- Preserve unrelated changes and keep each commit focused and reviewable.
- Reuse existing host contracts; authorization belongs on the server.
- Keep `{id, created_at}` identity checks and authoritative provider bindings.
  Closing a browser/tab/view must not end original tmux or provider work.
- `internal/home` serves the local web connector. Do not restore a desktop bridge,
  standalone terminal selector, SSH client transport or separate app installer.
- Retain the existing grouped-view tmux marker/name for rolling-upgrade safety.
- Accept retired Home configuration keys only through decode-only compatibility;
  never execute them. New examples contain local Home paths and launch profiles.
- Update owning documentation; distinguish implemented behavior, automated checks
  and actual device validation. Preserve accepted browser-input behavior.

## Security and privacy

- Never commit credentials, real deployment addresses, private-key paths, personal
  configuration, conversation dumps or runtime state. Use synthetic fixtures.
- Allowlist operations and use `exec.Command` argument arrays. Validate any
  arguments that cross an administrative SSH remote-shell boundary.
- Preserve SSH host-key checks; never enable agent forwarding.
- Do not access private databases or application storage belonging to other apps.
- Preserve account-scoped authentication/session revocation. Accounts sharing a
  Home are trusted users of the same machine, not isolated tenants.
- Back up existing user configuration with timestamps; use managed includes.
- Home auto-start is opt-in: a per-user macOS LaunchAgent or Linux systemd user
  service runs the native connector. Use the existing account and an explicit PATH;
  preserve configuration, singleton ownership and original tmux/provider processes.
  Do not add a separate resident supervisor or change system sleep/login policy.

## Verification

- `make check`: Go formatting/tests/race/vet, vendored collector, ShellCheck and web checks.
- `make integration`: hook installer and isolated session-create/web-PTY tests.
- `make build`: web assets, gateway and Home host binaries; no deployment.
- Follow `web/AGENTS.md` for browser checks; a typecheck is not device validation.
- Never attach to, rename, detach or kill pre-existing tmux sessions in tests.
  Live tests use disposable `hmux-e2e-*` resources and isolated sockets.
- Report failures/skipped checks and distinguish a local build from a deployment.
