# Rust candidate verification

Runnable checks and their scope. Commands run from the repository root; temporary
fixtures must never target production services or existing tmux sessions.
[Migration status](../docs/RUST_MIGRATION.md) owns accepted results and next work;
this reference describes how to run checks, not which release gates have passed.

## Test entry point

Use an isolated candidate before changing a running installation. Start here when
preparing a new artifact; accepted checks on unchanged artifacts remain valid.
Build prerequisites are the pinned Rust toolchain, Node.js 22+, Python 3 and the
native build tools. Mixed-version harnesses additionally require Go 1.24+.
Real tmux tests are separately opt-in; the native matrix uses synthetic host tools.
`make rust-check` defaults to two test workers to limit subprocess contention;
override `RUST_TEST_THREADS` when appropriate. Production timeouts are unchanged.

```sh
make rust-package
hmux_candidate_bundle="$(pwd -P)/dist/rust-web-$(rustc -vV | sed -n 's/^host: //p')"
HMUX_RUST_BUNDLE="$hmux_candidate_bundle" make rust-bundle-check
HMUX_RUST_WEB_BIN="$hmux_candidate_bundle/hmux-web" make rust-native-matrix
```

Run this on each native target host. It builds both binaries and web assets,
checks the bundle, then uses its web binary in private Go/Rust network and
auth-state rollback fixtures. It does not install the production service or open
an interactive browser test site. Preserve `RELEASE`, `SHA256SUMS`, tool versions
and results with the candidate. An older `dist/` directory is not proof that the
latest source was packaged.

| Check | When to use it |
| --- | --- |
| `make rust-check` | Fresh checkout or changed Rust code; formatting, strict Clippy and workspace tests |
| `make rust-compat` | Changed Go/Rust configuration, storage or protocol contracts |
| `make rust-proto-check` | Changed schema/generator; requires protoc 35.1 |
| `make rust-native-cli` / `make rust-native-installed-pairs` | Changed commands, installer or mixed-version helper behavior |
| Native workloads below | Resource, capacity, collector, failure and soak evidence; each result has its own limits |
| Browser/device acceptance below | Actual input, presentation and lifecycle behavior against an isolated candidate |

Do not start duplicate long-running tests to refresh a status label. Inspect the
existing runner's recent progress, final status and frozen binary hashes first.
An old `status: running` value alone does not establish a live or successful run.

## Browser and device acceptance

Prepare a separate test Gateway/Home with its own endpoint, authentication state,
connector token, workspace, staging root and disposable `hmux-e2e-*` tmux socket.
Verify those explicit paths before connecting. Use synthetic conversations and
test provider sessions; do not point a second connector at production state or
reuse existing tmux sessions. Real-device PWA/push checks require a secure origin
reachable from the device. Temporary HOME alone does not isolate a system service.

Record the candidate hash, browser/OS/device, action, observed result and relevant
redacted diagnostics. A browser emulator does not count as a physical-device run.

| Area | Acceptance scenario |
| --- | --- |
| Desktop Safari/Chrome | Login, restore tabs, select/copy, explicit URL opening, attachments and web shortcuts; input reaches the intended tab once |
| Korean and multiline input | Compose, replace, erase and wrap text in Codex/Claude/shell; check input overlay bounds and cursor residue |
| Physical iOS/PWA and Android | Auxiliary keys keep the keyboard usable; rotate, background/resume and reconnect without duplicate input or lost session identity |
| Connections and output | High output plus typing, switching tabs, a slow/background view and temporary network loss; healthy views remain usable and original processes survive |
| Accounts and conversations | Test-account login persistence, TOTP, revocation/isolation, source-specific usage and filtered Codex/Claude Markdown/tables |
| Files and notifications | Desktop drop/mobile attachment insertion, three-hour staging retention, permitted push delivery and the correct destination tab |
| Service lifecycle | In an isolated OS account, restart/login/reboot and current-state binary rollback; retained logins survive and revoked logins stay revoked |

Use [web behavior](../docs/WEB.md), [accepted input behavior](../docs/BROWSER_INPUT.md)
and [iOS constraints](../docs/IOS_INPUT.md) as the expected contracts. This checklist
does not change them. Report missing devices or accounts as unverified; release
acceptance remains governed by [the migration contracts](../docs/RUST_CONTRACTS.md).

## Build and package

Rust migration candidates additionally use the pinned `rust-toolchain.toml`:
`make rust-check`, `make rust-compat` and `make rust-build`. These do not replace
default Go build outputs. See [the migration control](../docs/RUST_MIGRATION.md) and
[synthetic measurements](../bench/hmux/README.md). `Cargo.lock` is tracked; Rust crates
are not published as packages during migration.
Schema edits also require `make rust-proto-check` with protoc 35.1. This regenerates
and compares checked-in Protobuf types; normal binaries do not need protoc.
Typed-control regressions are in the protocol action tests, Gateway hub/HTTP tests
and Home peer tests. They cover operation/result matching, sessionless actions,
late v1 replies, reply ownership across reconnect, and the legacy JSON boundary.
Native mixed-peer checks must also pass after changing the negotiated protocol.
`make rust-build` builds the candidate workspace, including native `hmux-web` and
`hmux-agent`. `make rust-native-cli` runs CLI, service-manager simulation and
installer interruption checks; `make rust-native-e2e` connects the native web
entrypoints to isolated Home/gateway fixtures. These checks do not register the
production user service or use existing tmux sessions. `make rust-package` creates separate
`dist/rust-web-<target>` bundles. `make rust-bundle-check` verifies the native host
bundle's manifest, fresh installation, repeated updates, configuration preservation
and Python compatibility entrypoint in disposable Homes. Set `HMUX_RUST_BUNDLE`
to select another runnable bundle. OS service/release acceptance remains a migration
gate; ordinary `make build` continues producing Go bundles.

## Native pairing and workloads

`make rust-native-matrix` runs all four Go/Rust gateway × Home combinations
and Rust Protobuf v2 plus forced JSON fallback over a synthetic loopback TLS proxy.
Linux uses native binaries for both roles. On macOS, Go Home uses the actual
connector core in an owned test subprocess with an explicit synthetic CA because
the native Go verifier ignores `SSL_CERT_FILE`; all other roles use native binaries.
That lane proves Go-core interoperability, not native Go CLI WSS trust.
It verifies catalog/usage/actions, PTY flow, reconnect, logout and shutdown. A
browser withholding a full render-credit window must not block another browser
or a control request, and resuming it must preserve every output byte. Set
`HMUX_RUST_WEB_BIN` to an absolute packaged binary to test that artifact; otherwise
it builds the debug candidate. The Go oracle uses the race detector on native
builds; cross-compiled test executables without it are not race-test evidence.
The matrix also starts Rust → Go → Rust native gateways sequentially against
the same synthetic authentication files: retained logins must survive and sessions
revoked on either implementation must stay revoked. This is auth-state binary
rollback, not a complete deployment/service rollback rehearsal.
`make rust-native-installed-pairs` copies actual Go/Rust web/helper executables
into a private installation path, changes one file per step, and checks all four
pairs through upgrade and rollback. It verifies installed web CLI startup plus
helper config/workspace/workflow continuity; network pairing and service activation
remain the separate checks above. No old state snapshot is restored.
`HMUX_STRESS_OUTPUT=/tmp/hmux-stress-new make rust-native-stress` repeats native
Rust view open/input/close/cleanup 100 times per codec. The output directory must
not exist; set `HMUX_STRESS_CYCLES=10000` for the lifecycle churn gate. It records
gateway/Home RSS checkpoints separately, plus Linux PSS/thread/FD counts and
binary hashes. These are synthetic-tool process checks, not CPU/latency budgets,
peak memory, long soaks or browser/device acceptance. The harness never registers
a user service and only uses its disposable private HOME and tmux fixtures.
`HMUX_STRESS_OUTPUT=/tmp/hmux-capacity-new make rust-native-capacity` checks eight
active native views, ninth-view rejection, surviving echo/ACK, replacement after
one closes and final cleanup in both codecs. It requires explicit acceptance
markers and all four per-process resource checkpoints; a skipped or older oracle
cannot silently count as a pass.
`HMUX_PERF_OUTPUT=/tmp/hmux-perf-new make rust-native-perf` runs five native
Go/Go and Rust/Rust pairs on Linux, with common JSON v1 and a separate Rust
Protobuf lane. It alternates pair order, warms one persistent view, records ten
seconds of connected idle and 1,000 sequential 64-byte echoes. The prior synthetic
slow-view burst is part of the precondition. RSS/PSS/CPU/FD/thread checkpoints
identify gateway/Home separately; RTT ends at socket receipt, not xterm rendering.
The oracle is built without race instrumentation for measurements. This is a
measurement, not a performance-budget verdict. Counts are bounded; use the Python
runner directly for explicit Go GC tuning or workload settings. Results require
a new private directory and include binary/dependency hashes and raw RTT samples.
For sustained comparison, the Python runner defaults to 250,000 echoes and 60
seconds idle; the short Make target keeps the prior 1,000/10-second workload.
Use `--gogc 50 --gomemlimit 32MiB` to add a separate tuned-Go lane alongside the
unchanged Go default. Each lane gets equal input counts; raw samples are saved
once per run. Counts are capped at 500,000 per run and ten million per invocation.
`--go-tuning-only` compares just default/tuned Go for targeted tuning checks;
the oracle and synthetic tools clear Go tuning variables. Custom harness controls
forward tuning only into the native gateway/Home, keeping driver settings fixed.
`CARGO_HOME=/tmp/hmux-cargo cargo run --locked --release -p hmux-home --example
activity_bench -- 512` exercises the production JSONL reader against a new private
synthetic tree. The optional argument is files per provider (8–2048). It checks
bootstrap totals, ten unchanged polls with zero content bytes read, incremental
bursts, inode replacement and no replay. JSON output records raw sample times and
diagnostics; fixture creation is outside those times and cleanup is automatic.
This is reader-level wall time, not native Home memory/CPU, provider polling or a
Go comparison. Use release mode to avoid drawing conclusions from debug builds.
`HMUX_ACTIVITY_OUTPUT=/tmp/hmux-activity-new make rust-native-activity` instead
uses the actual native Home collector and gateway with 512 synthetic JSONL files
per provider. Both codecs must publish exact backfill/append/inode-replacement
totals through authenticated state, retain them across quiet scans and continue
terminal echoes. It records bounded raw socket RTTs and per-process RSS (Linux:
also PSS/CPU/FDs/threads). Use `HMUX_RUST_WEB_BIN` for a packaged release binary.
These are collector/terminal checks and resource checkpoints, not peak memory,
Go comparison, parallel file-write throughput or physical-device validation.
`HMUX_SOAK_OUTPUT=/tmp/hmux-soak-new make rust-native-soak` runs 24 hours by
default. Set `HMUX_SOAK_SECONDS=12` for smoke, `259200` for a frozen 72-hour
candidate, or `HMUX_SOAK_CODEC=json` for legacy-wire acceptance. The runner copies
the native executables into a new private output directory, records their hashes,
and writes atomic progress to `summary.json`. It uses one persistent browser view,
authenticated echo/catalog checks roughly once per second, and a transient view
roughly once per minute, failing a check gap over five seconds. Reconnect,
revocation and shutdown must still pass after the full elapsed duration. Normal
signals to the Python runner clean its owned process group and verify native/PTY
fixture exit; it never registers a service or changes sleep/login policy. Native
macOS tests need boot-identity, PTY and process-inspection access. Synthetic soak
does not replace real providers, full workloads or physical-device acceptance.

## Dependency and notice checks

The Rust lockfile advisory gate uses `cargo audit` 0.22.2 or newer (older versions
cannot parse current CVSS 4 advisories). Record the advisory database revision and
report; do not treat an unavailable database as a pass. This gate is separate from
dependency-license acceptance. Install pinned `cargo-audit 0.22.2` and
`cargo-deny 0.20.2`, then run `HMUX_ADVISORY_DB=/path/to/fresh/advisory-db make
rust-dependencies`. `deny.toml` checks allowed license expressions and registry
sources. `make rust-notices-check` verifies transitive notice collection; Rust
bundles retain normal/build dependency notices from the host/target closures,
ring's per-source copyrights and Rust standard-library notices. The build/check
first fetches the locked graph (including metadata for dev-only packages); notice
collection then reads cached sources offline and fails on absent license files. Bundle
tests verify every indexed notice/hash; GitHub CI runs the dependency policy and
notice collector. Tool installation and fetching the advisory database are
development/release checks, not HMux runtime dependencies.

## OS and lower-level compatibility checks

`HMUX_RUN_NATIVE_MANAGER_TEST=1 cargo test -p hmux-service --test native_manager -- --ignored`
explicitly tests rendered units with the real launchd/systemd user manager using
unique `hmux-e2e-*` names and removes them afterward. Linux links are runtime-only;
macOS uses a temporary plist. It checks argv/environment, working directory,
restart PID and shutdown; this is not login/reboot or production service-CLI proof.
`make rust-gateway-candidate` separately builds `target/release/examples/auth_gateway`,
an explicitly auth-only loopback example; it is not a production `hmux-web` bundle.
`python3 tests/rust_auth_candidate.py` runs it with disposable synthetic state and
checks startup, process restart, revocation and SIGTERM shutdown.
`make rust-full-gateway-candidate` builds a separate `gateway_candidate` example
with the gateway owners assembled. `make rust-gateway-e2e` runs it against the
actual Go Home workers and an owned PTY with synthetic tmux/process tools, private
temporary state, a synthetic trust root and loopback-only networking. It also
reads the OS boot identity for Go recovery; constrained macOS sandboxes may block
that read. This is protocol/process integration, not real tmux or device acceptance.
`rust-compat` also builds a temporary Go flock helper and runs the explicitly
ignored cross-process Rust test. It hands current Rust authentication state to Go,
logs out a retained synthetic account there, then checks Rust cannot revive it.
Ordinary `cargo test` skips tests requiring those cross-language handoffs and the
separately opt-in native system trust check.
`make rust-home-peer-compat` (also part of `rust-compat`) connects the candidate
Rust Home peer to the actual Go connector endpoint and hub, built with the Go
race detector. Temporary inventory and fake tmux commands exercise catalog,
profiles, streamed binary uploads (Go manifest/hash validation, private modes,
expiry and bidirectional spool-lock exclusion), an owned shell PTY
(Korean I/O, resize, refresh failure and close),
explicit unsupported-operation responses and disconnect cleanup.
It offers Protobuf v2 and verifies same-socket legacy v1 selection when the Go
server omits the subprotocol. It does not test WSS, reconnect, real tmux or the
production Home service.
`make rust-home-candidate` builds the separate `home_candidate` example. It
requires `--experimental-home`, an explicit WSS endpoint, private token and
Home config; it does not install a service or replace `hmux-web connect`.
`make rust-home-candidate-e2e` runs that executable with synthetic CA/config,
loopback WSS and a fake tmux executable. It checks both codecs, reconnect,
singleton exclusion, terminal startup, binary upload commit and signal-driven
cleanup of partial uploads. These are
process checks; native service installation and physical devices are separate.
The candidate resolves tmux from bounded PATH or an explicit absolute `--tmux`;
`--tmux-socket` selects an absolute disposable test socket. Never test with a
production token, state directory or existing user tmux session. Candidate uploads
require `--staging-root /absolute/private/hmux/staged-files-v1`; no real user cache
is selected implicitly. The spool engine/peer tests use private temporary roots,
verify three-hour expiry, strict quota and identity checks, flock/queue pressure
and cancellation cleanup. The connector sweeper is joined before singleton release.
The separate Home WSS unit tests use generated certificates and private loopback
listeners to exercise native TLS verification, shared DNS/socket admission,
cancellation and deadlines. `native_trust_initialization_is_opt_in` reads the
host OS trust store without dialing; sandbox restrictions can prevent it even
when synthetic tests pass. Run/report this check explicitly on supported hosts.
Home library proxy checks use synthetic HTTP/HTTPS tunnels and injected policy
inputs, never the operator's proxy credentials. They cover CONNECT boundaries,
TLS/credential separation, routing exceptions, cancellation and admission.
`make rust-home-lock-compat` runs Rust's lifetime connector lock against the
actual Go `homeservice.LockConnector`, built with the race detector. It checks
exclusion in both directions and release after normal and forced process exit,
using a private temporary directory. Ordinary Home tests cover directory trust,
unsafe lock files, inode preservation and simultaneous callers. This does not
prove a system service's lifetime behavior. Separate connector tests exercise
sequential three-second retries, cancelled startup/dial/peer owners, caller-abort
cleanup and lock retention using injected dial results and the read-only peer.

`make rust-home-state-compat` checks current session metadata and visibility
against a temporary Go `-race` test binary. Both stores read each other's writes,
reject stale lifetimes and exclude each other on the existing persistent lock
inodes. The fixture is private and synthetic; no tmux is involved. This target
also runs under `rust-compat`.

`make rust-home-continuity-compat` exchanges current recovery checkpoints and
workflow records between actual Go and Rust owners. Synthetic fixtures cover
empty and pending recovery state, exact restored lifetimes, mutual file locking,
concurrent workflow helpers and matching human-readable workflow output. It is
also part of `rust-compat`; its example binaries are test bridges, not installed
commands, and never use existing tmux sessions or provider records.

`make rust-home-recovery-e2e HMUX_TEST_TMUX="$(command -v tmux)"` additionally
creates disposable tmux sockets with synthetic Codex/Claude executables. It checks
interrupted restore/retry, persisted identity before provider launch, exact resume
arguments/configuration, surviving shells and no repeated launch on same-boot sync.
It never connects to the default tmux server and removes its own fixtures.

Session creation peer tests exercise both codecs, literal argv, unique child
workspaces, profile metadata, aliases/visibility, admission and cancellation.
Candidate session actions capture explicit HOME/PATH/SHELL inputs; they do not
change the configured workspace base. The checked-in Unicode 15 category table
preserves Go's workspace slugs without a runtime dependency; the migration-only
`go run ./tools/hmux-unicode-oracle` regenerates it for comparison after rustfmt.

Home PTY unit tests spawn only owned synthetic shells. The opt-in
`make rust-home-terminal HMUX_TEST_TMUX="$(command -v tmux)"` additionally starts
private tmux servers with disposable `hmux-e2e-*` original sessions. It checks
nonce-guarded view cleanup, name collisions, Korean I/O, client resizing, refresh and
preservation of the original process and another test-owned client after close.
It also tests candidate-created Codex/Claude sessions with fake providers:
exit 0/7/130, Ctrl+C termination and handled Ctrl+C retain an interactive shell.
Closing the peer keeps those test-owned sessions alive until fixture cleanup.
No default tmux socket or existing user session is accessed. macOS sandboxes may
block the process query required for real refresh; report the sandbox failure
separately from an authorized isolated rerun. Synthetic Home terminal tests
exercise both codecs, finite queues/credit and startup-load isolation. Packaged
OS, WSS/service and browser/device acceptance remain separate migration gates.
