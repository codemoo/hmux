# Verification

Only the web/PWA UI and its Home/gateway services are supported. Tests for retired
selectors, terminal frames, desktop app bridges/installers and SSH provisioning
have been removed alongside those implementations.

| Check | Scope |
| --- | --- |
| `make check` | Go formatting/unit/race/vet, vendored collector, ShellCheck, TypeScript and frontend tests |
| `make build` | Production web assets, Linux gateway and macOS Home/helper binaries |
| `make integration` | Safe Home installation, managed workflow hooks, isolated profile creation and browser PTY lifecycle |
| `npm test --prefix web` | Browser logic, keyboard/input, clipboard, connection recovery, settings and rendering contracts |
| `internal/config` | Profile-only inventory, legacy Home config compatibility, precedence and fail-closed validation |
| `internal/home` | Identity checks around grouped-view creation, cleanup, redraw and catalog observation |
| `internal/webgateway` | Authentication/account scope, revocation, transport flow/closure, uploads, notifications and diagnostics |
| `internal/recovery` | Checkpoints, boot identity, safe construction and verified tab remapping |

`make integration` requires tmux, jq and Python 3. Tests use private sockets and
`hmux-e2e-*` session names. The PTY test checks original session options/process
survival, view resize/close/cancellation and stale identities. Hook installation
runs only against a temporary HOME. A missing tool is a skipped check, not a pass.

Additional recovery integration, using fake providers on an isolated socket:

```sh
HMUX_RUN_RECOVERY_TMUX_TEST=1 go test ./internal/recovery -run TestRecoveryWithIsolatedTmuxAndFakeProviders -count=1
```

Never enable live tests against a personal tmux server. Read-only tests requiring
real installed providers are not general CI gates. Browser emulation and synthetic
input traces do not replace physical iOS/Android/Safari acceptance; follow
[web/AGENTS.md](../web/AGENTS.md). Record dated results in [VALIDATION.md](../docs/VALIDATION.md).
