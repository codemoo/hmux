# Verification

Only the web/PWA UI and its Home/gateway services are supported. Tests for retired
selectors, terminal frames, desktop app bridges/installers and SSH provisioning
have been removed alongside those implementations.

| Check | Scope |
| --- | --- |
| `make check` | Go formatting/unit/race/vet, vendored collector, ShellCheck, TypeScript and frontend tests |
| `make build` | Production web assets, Linux gateway and macOS Home/helper binaries |
| `make integration` | Safe Home installation, managed workflow hooks, isolated profile creation and browser PTY lifecycle |
| `make rust-native-matrix` | Candidate Go/Rust native pairs, both wire codecs, slow-view isolation and reconnect/revocation |
| `make rust-native-stress` | Opt-in native Rust view churn and separate gateway/Home resource checkpoints; see required output path in tests/RUST.md |
| `make rust-native-capacity` | Native eight-view capacity, ninth rejection, admitted-view survival and released-slot reuse; both wire codecs, synthetic host fixtures |
| `make rust-native-activity` | Actual native Home collector plus terminal echoes: synthetic JSONL backfill/append/replacement, exact state totals and separate process checkpoints |
| `make rust-native-perf` | Linux paired native gateway/Home resource and socket-latency measurements; distinct from browser rendering and budget acceptance |
| `cargo run --locked --release -p hmux-home --example activity_bench -- 512` | Opt-in production JSONL reader workload: synthetic backfill, zero-content quiet polls, append and inode replacement; reader-level timing only |
| `make rust-native-soak` | Bounded 24h/72h synthetic native runs with frozen binaries, progress, cadence and final lifecycle assertions; no device acceptance claim |
| `make rust-dependencies` / `make rust-notices-check` | Locked Rust advisory/license/source policy, transitive native notices and standard-library attribution; see tool/database setup in tests/RUST.md |
| `npm test --prefix web` | Browser logic, keyboard/input, clipboard, connection recovery, settings and rendering contracts |
| `internal/config` | Profile-only inventory, legacy Home config compatibility, precedence and fail-closed validation |
| `internal/home` | Identity checks around grouped-view creation, cleanup, redraw and catalog observation |
| `internal/homeservice` | User-service definitions/manager commands, environment allowlist, verified process adoption, singleton locking, bounded logs and safe backups |
| `internal/webgateway` | Authentication/account scope, revocation, transport flow/closure, uploads, notifications and diagnostics |
| `internal/recovery` | Checkpoints, boot identity, safe construction and verified tab remapping |

`make integration` requires tmux, jq and Python 3. Tests use private sockets and
`hmux-e2e-*` session names. The PTY test checks original session options/process
survival, view resize/close/cancellation and stale identities. Hook installation
runs only against a temporary HOME. A missing tool is a skipped check, not a pass.

Rust candidate command details and opt-in OS checks are in
[Rust verification](RUST.md); the [migration control](../docs/RUST_MIGRATION.md)
records their current acceptance. Default build outputs remain Go while the
maintained Rust trials and remaining release gates are evaluated.

Additional recovery integration, using fake providers on an isolated socket:

```sh
HMUX_RUN_RECOVERY_TMUX_TEST=1 go test ./internal/recovery -run TestRecoveryWithIsolatedTmuxAndFakeProviders -count=1
```

Never enable live tests against a personal tmux server. Read-only tests requiring
real installed providers are not general CI gates. Browser emulation and synthetic
input traces do not replace physical iOS/Android/Safari acceptance; follow
[web/AGENTS.md](../web/AGENTS.md). Record dated results in [VALIDATION.md](../docs/VALIDATION.md).
