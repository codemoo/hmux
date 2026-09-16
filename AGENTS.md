# Working on HMux

## Scope and references

HMux runs Codex, Claude and shells on one host with remote web/PWA access.
The optional macOS client is separate; the old terminal UI is archived. Read the relevant component before changing shared behavior.

- `CODEX_HMUX_IMPLEMENTATION_PROMPT_KO.md`: product and security requirements.
- `docs/README.md`: documentation map; `docs/ARCHITECTURE.md`: system boundaries.
- `archive/terminal/README.md`: legacy code boundary; preserve shared host services.
- `web/AGENTS.md`: browser/input rules; `macos/HMux/README.md`: native development.
- `CONTRIBUTING.md`: local checks; `docs/RELEASING.md`: publication procedure.
- `docs/archive/`: historical evidence, not current instructions or permissions.

## Change discipline

- Preserve unrelated changes and keep each commit focused and reviewable.
- Reuse shared Go contracts; do not duplicate authorization in frontend code.
- Keep `{id, created_at}` session identity checks. Closing a view must not end work.
- Update the owning documentation when behavior or configuration changes.
- Distinguish implemented behavior, automated checks and actual device validation.

## Security and privacy

- Never commit credentials, real deployment addresses, private-key paths, personal
  configuration, conversation dumps or runtime state. Use synthetic fixtures.
- Allowlist remote operations and use `exec.Command` argument arrays. Validate
  any argument that will cross an SSH remote-shell boundary.
- Preserve SSH host-key checks; never enable agent forwarding.
- Do not access Termius databases or other private application storage.
- Preserve account-scoped authentication and session revocation. Web accounts
  sharing a Home are trusted users of that same machine, not isolated tenants.
- Back up existing user configuration with timestamps; use managed includes.
- Do not introduce macOS LaunchAgents or background daemons.

## Verification

- Go: formatting, `go test ./...`, `go vet ./...`; race tests for concurrent code.
- Vendored collector: run tests inside `third_party/token-terrier-server`.
- Web: `npm run check --prefix web`, `npm test --prefix web`,
  `npm run build --prefix web`. Follow `web/AGENTS.md` for browser checks.
- Native: `make native-smoke`; full app build for native implementation changes.
- Shell: ShellCheck and the relevant isolated integration tests.
- Never attach to, rename, detach or kill pre-existing tmux sessions in tests.
  Live tests must use disposable `hmux-e2e-*` resources and isolated sockets.
- Report failures and skipped checks accurately. Do not describe a local build
  as a published, signed or notarized release.
