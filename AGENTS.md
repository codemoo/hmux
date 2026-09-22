# Working on HMux

## Scope and references

HMux runs Codex, Claude and shells on one Home host. Its only user interface is
web/PWA. macOS remains a supported Home host; browser-native input is part of the web UI.

- `CODEX_HMUX_IMPLEMENTATION_PROMPT_KO.md`: product and security requirements.
- `docs/README.md`: documentation map; `docs/ARCHITECTURE.md`: system boundaries.
- `web/AGENTS.md`: browser/input rules; `docs/WEB.md`: current web behavior.
- `CONTRIBUTING.md`: checks; `docs/OPERATIONS.md`: installation/administration.
- `docs/RELEASING.md`: publication; `docs/archive/`: non-current browser evidence.

## Change discipline

- Preserve unrelated changes and keep each commit focused and reviewable.
- Reuse Go host contracts; authorization belongs on the server.
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
- Do not introduce macOS LaunchAgents or background daemons.

## Verification

- `make check`: Go formatting/tests/race/vet, vendored collector, ShellCheck and web checks.
- `make integration`: hook installer and isolated session-create/web-PTY tests.
- `make build`: web assets, gateway and Home host binaries; no deployment.
- Follow `web/AGENTS.md` for browser checks; a typecheck is not device validation.
- Never attach to, rename, detach or kill pre-existing tmux sessions in tests.
  Live tests use disposable `hmux-e2e-*` resources and isolated sockets.
- Report failures/skipped checks and distinguish a local build from a deployment.
