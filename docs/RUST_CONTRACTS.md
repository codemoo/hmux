# Rust migration contracts and release gates

Scoped reference: full scope, invariants, source map and acceptance gates.
Read [the current migration control](RUST_MIGRATION.md) first; this file carries
no changing implementation status. Source baseline: `245d4e6839939839fd56b7632600d7398726e359`.

## Decision and scope

HMux's purpose remains **"low memory, web terminal for ai agents"**. The chosen
end state is Rust for all HMux native runtime code: gateway, Home connector,
embedded usage collection, administration and workflow helpers. This is a product
direction, not an experiment conditional on Rust beating Go. Measurements decide
optimizations and release readiness; a regression requires investigation rather
than changing the goal or claiming an unmeasured improvement.

The TypeScript/xterm.js web/PWA remains the sole UI. Rust does not replace the
browser with WASM, a desktop app or a terminal selector. Provider CLIs, tmux,
provider credentials and workspaces stay on the host. Nginx remains the public
HTTPS boundary. Docker and a separate resident supervisor are not required.

This is the durable migration contract. [RUST_MIGRATION.md](RUST_MIGRATION.md)
alone controls current work and next actions. Architecture/Operations describe the
running Go product until a verified replacement. Validation records deployment;
this reference is not evidence that a deployment has occurred.

### What “complete” means

- `hmux-web init|serve|connect|service` and every supported `hmux-agent` command
  are implemented in Rust, retaining executable names and external contracts.
- Usage collection runs in the Rust Home process; there is no Go sidecar, Go FFI
  library or Go subprocess helper. Maintained OS/PTY bindings and external provider
  commands remain allowed; they are not hidden Go implementations of HMux.
- Native release builds and normal contributor checks no longer require Go.
  Shell formatting uses a pinned, checksum-verified standalone shfmt installation;
  it must not reintroduce a Go toolchain requirement.
- First-party installation logic currently in Python moves into the Rust CLI;
  thin shell download/bootstrap scripts and test harnesses may remain. New binary
  installations require no Python or compiler. Keep the existing Python entrypoint
  as a compatibility wrapper during transition, then document its retirement.
- Go source, modules and vendored Go collector are removed from the active tree
  only after equivalent coverage and rollback evidence exist. Git history and
  immutable Go release artifacts retain the reference implementation.
- Third-party attribution/license obligations remain after a port. External
  provider CLIs and tmux are dependencies, not HMux code to rewrite.

## Changes from the supplied proposal

The supplied “HMux 서버 Rust 전환·저메모리 최적화 — Codex 실행 지시서” v1.0
(2026-09-22) is an input, not the current repository contract. Its review used
`a4fcecf`; refresh all source assumptions against the baseline above.

Keep its protocol/state compatibility, bounded resource accounting, fair
backpressure, cancellation safety, synthetic measurements and current-state
rollback requirements. Expand its optional Home migration into required work.
The later user decision adds Protobuf over the Home WebSocket as the target wire
format; retain JSON v1 only for the rolling migration. Do not retain references to a native macOS UI, desktop bridge, SSH client transport,
Xcode acceptance or `internal/client`. Use `internal/home` and current tests.
The root product/security specification remains an active reference.

Gateway PSS ≤16 MiB (stretch ≤10 MiB), 30% below tuned Go and idle CPU ≤0.1%
are **proposed gateway targets**, not achieved results or Home budgets. Ratify
measurement conditions before adopting thresholds. Gateway changes do not alone
fix Home catalog/process/provider scanning delays; measure those independently.
External references and crate APIs from the proposal must be checked against
selected versions before implementation; no dependency selection is final here.

## Source and replacement map

| Existing owner | Required Rust responsibility |
| --- | --- |
| `cmd/hmux-web/` | CLI parsing, enrollment, serve/connect entrypoints and errors |
| `internal/webgateway/{server,http_api,http_auth,http_action,browser_terminal}.go` | HTTP/static assets, security gates, accounts and terminal admission |
| `internal/webgateway/{protocol,hub,terminal_flow}.go` | Wire v1, peer generations, pending requests, flow control and fan-out |
| Other gateway auth/upload/push/diagnostics files | Full account/security/storage and API parity, not relay-only MVP |
| `internal/webgateway/home*.go`, `internal/home/` | Outbound connector, shared collectors, request handling and disposable PTYs |
| `internal/catalog/`, `internal/catalogstream/` | tmux catalog, exact provider binding, Codex/Claude conversations and completion |
| `internal/agent/`, `internal/config/`, `internal/model/`, `internal/sessionstate/` | Profile launch, typed contracts, configuration and metadata |
| `internal/providers/`, `internal/webgateway/providers.go` | Existing Codex/Claude/Gemini status, install/login/update jobs, key management and allowlisted auth URLs |
| `internal/sessionlaunch/` | Provider argv, signal handling and return to an interactive shell |
| `internal/recovery/`, `internal/sharedworkspace/` | Verified restore/checkpoint and workspace continuity |
| `internal/filestage/` | Private bounded attachment staging and three-hour cleanup |
| `internal/homeservice/`, `internal/hostmetrics/` | launchd/systemd lifecycle, process identity, OS metrics |
| `internal/workflow/`, `cmd/hmux-agent/` | Hooks, workflow records and all headless administrative commands |
| `internal/filelock/`, `internal/safeexec/`, `internal/timing/` | Locking, bounded process output and safe timings |
| `third_party/token-terrier-server/stream` and reachable collectors | In-process source-separated usage, auth readers and bounded polling |
| `deploy/web/`, `scripts/`, CI/Makefile | Packaging, install/update, service definitions, notices and checks |

The vendored collector also contains unused upstream daemon/SSH support. Audit
reachable imports and public HMux behavior; do not recreate that unused runtime.
Capture CLI/route/state details in fixtures instead of assuming this map is exhaustive.

## Proposed Rust layout

Use a root Cargo workspace alongside Go during migration. Start with the smallest
useful crate set and add boundaries when the corresponding phase starts:

```text
crates/hmux-model/      shared validated domain model
crates/hmux-core/       shared private storage and native transaction locks
crates/hmux-protocol/   v1 adapter, Protobuf v2 codecs and validated limits
crates/hmux-gateway/    gateway library
crates/hmux-home/       host services, tmux/PTY, recovery and collectors
crates/hmux-usage/      embedded provider usage library
crates/hmux-web/        hmux-web binary: init, serve, connect, service
crates/hmux-agent/      hmux-agent binary: administration and hooks
bench/hmux/            synthetic drivers, budgets and result schemas
```

Crates are compilation boundaries, not additional services. Avoid a generic
framework or one crate per Go package. Share explicit wire types while keeping
security-sensitive storage validation and transaction ownership with their owners.

Start with stable Rust, a pinned toolchain and Cargo.lock. Evaluate Tokio/Axum,
Serde, bytes and maintained HTTP/WebSocket/crypto/PTY libraries with license and
advisory checks. Use verified crypto implementations. Default to system allocator;
compare runtime worker counts and allocators only after profiling. Bind all
blocking process/file/KDF work to bounded admission and cancellation semantics.
Runtime crates forbid unsafe Rust. The small `hmux-platform` crate isolates the
one macOS `sysctl` binding that maintained safe wrappers do not expose with exact
argv semantics. Its private adapter permits only read calls with owned bounded
buffers; review pointer/length validity, kernel size checks and before/after
process identity together. It adds no process or resident collector.

Home outbound WSS and provider/push HTTP require certificate and hostname
verification, usable trust roots and bounded DNS/connect/read behavior; test these
in packaged binaries. The loopback gateway continues to rely on Nginx for TLS.

## Non-negotiable compatibility gates

- Preserve CLI flags, exit behavior, JSON/stdout contracts, config precedence,
  decode-only legacy keys, unknown-field rejection and profile validation.
- Preserve base64 byte fields, omitted/null/false distinctions, timestamps,
  integer bounds, strict decoding, WS frame types, deadlines and close behavior.
- Preserve account scope, Host/Origin/CSRF/cookie policy, password strength, TOTP
  atomic replay protection, persistent login, revocation and active-work cancellation.
  KDF permits remain held until actual work ends, even after request cancellation.
- Preserve private modes, symlink defenses, per-store lock ordering, atomic
  persistence and auth-critical durability. One connector owns each Home; helpers
  and hooks may access its stores concurrently only through compatible transaction
  locks. Mixed-language lock semantics require explicit verification (see below).
- Require `{id, created_at}` and authoritative provider bindings. Preserve grouped
  view markers/names during rolling upgrades. Closing a browser or connector does
  not terminate original tmux/provider work; provider exit leaves the host shell.
  Recheck identity across grouped-view creation, preserve detached cleanup hooks
  and target redraw signals only at a verified foreground process group.
- Preserve configured workspace roots, safe child-folder naming and session names.
- Preserve source-specific quotas, plan labels, missing-window semantics, reset
  times, preferences and allowed labels; never forward raw credentials/responses.
  Retain the collector snapshot allowlist, bounded stream, worker shutdown and one
  shared collector per connected Home.
- Preserve provider setup jobs, credential write safeguards, auth URL allowlists
  and redaction after authorization-code input. Test with fake CLIs on isolated
  sockets; Gemini setup parity does not imply adding Gemini conversation support.
- Preserve Codex/Claude conversation parsing, internal-context filtering and exact
  session selection. Synthetic transcripts only in tests and committed evidence.
- Bound bytes as well as item counts, including producer waiters, library buffers,
  parser assembly, caches, upload chunks and pending requests. Slow views must not
  block the shared Home reader or cause terminal bytes to be silently dropped.
- Isolate caller cancellation from shared transport lifetime. A queued request
  can be canceled; once its frame starts, complete it within an independent bounded
  transport deadline. Dropping a Rust future must not leave a partial frame, lose
  the writer or disconnect unrelated tabs. Port the paused-write tests in
  `internal/webgateway/shared_connection_cancel_test.go`; verify stale-result discard
  and cancellation before admission, mid-write, during reply wait and on peer change.
- Preserve opt-in launchd/systemd user services, explicit PATH, singleton locks,
  verified process adoption and configuration backups. Check owner and process birth
  identity, not PID alone. Do not change sleep/login policy.
  Linux adoption pins a pidfd before rechecking identity and signaling; kernels
  without pidfd support must use explicit installation after manual connector stop.
  macOS retains the existing Go behavior: an immediate owner/birth/argv/environment
  recheck followed by positive-PID TERM. Those calls are not atomic; PID reuse in
  that narrow interval remains a limitation and must not be described as eliminated.
- Preserve browser input, IME, uploads, notifications/deep links, tab continuity
  and cache isolation. Native-backend replacement does not justify UI regressions.

## Execution sequence and exit gates

Each phase is a reviewable change series. Keep production on its verified release
while candidates run against isolated state. Do not remove Go owners prematurely.

| Phase | Work | Required exit evidence |
| --- | --- | --- |
| 0 — Contracts and baseline | Enumerate CLI/routes/wire/state/OS contracts; sanitized golden fixtures; synthetic Home/browser drivers; Go current/tuned benchmarks; map existing tests | Reproducible results or explicit unavailable-environment status; Go oracle, complete owner/test matrix and proposed machine-readable budgets |
| 1 — Rust foundation | Workspace/toolchain/CI, v1 adapter + Protobuf v2 schema/codecs/limits, CLI shell, security admission and bounded blocking primitives | Bidirectional Go/Rust fixtures; protected incomplete routes fail closed; isolated candidate builds, outside production artifact paths |
| 2 — Gateway parity | Auth/state/profiles, negotiated v1/v2 hub, terminal/ACK, uploads, usage/conversations forwarding, push and diagnostics | Complete route matrix; Go Home + Rust gateway E2E; revoke/fault/slow-view tests; current-state Go rollback |
| 3 — Home core | Config/catalog/process inspection, provider bindings/conversations, tmux/PTY, session creation, file staging, host metrics, provider setup jobs and shared collection | Rust Home + Go gateway and Rust gateway E2E; original sessions survive view/reconnect/upgrade; macOS/Linux OS tests |
| 4 — Usage and continuity | Port reachable usage collectors, recovery, shared workspace, completion and workflow persistence | All supported usage sources and provider parsing covered; fake-provider reboot recovery; preference and identity compatibility |
| 5 — CLI and installation | All hmux-agent commands, setup/hooks, service install/adoption/status/logs/update, existing Python installer transition | macOS launchd/Linux systemd isolated tests; paths with spaces, PATH, process reuse, mixed helper locks and interrupted two-binary install/rollback |
| 6 — Performance and release rehearsal | Allocation/runtime tuning, full-stack stress/soak, package both binaries/assets/notices, canary and rollback procedure | Functional/security parity; measured budgets; 24h and release-candidate 72h soak; actual device acceptance distinguished from emulation |
| 7 — Rust default and Go retirement | Coordinated service replacement, documentation/check/build switch, remove active Go sources/tooling after rollback window | Released gateway/Home/helper versions and hashes verified; no Go runtime dependency; clean Rust-only build; rollback artifact retained |

Phase 3 may use a small Rust-only test entrypoint until its connector is complete;
it must not expose an incomplete production command as working. Phase 4 completes
Home data behavior; Phase 5 completes its service/helper/installation contracts.
A Home release requires **Phases 3–5 plus the applicable Phase 6 release gates**.
Gateway-only canary can precede that once Phase 2 and its own release gates pass,
using the unchanged Go Home. It is not completion of the full migration.

`hmux-web` contains both serve and connect/service. Until all roles are ready,
package an experimental gateway only for its tested role in a separate candidate
directory; never overwrite the normal macOS Home bundle or advertise a universal
replacement. Keep existing production `make build` outputs on Go until an explicit
role cutover. Candidate Rust builds run in CI without becoming deployment inputs.

During mixed versions, test all four Go/Rust gateway × Go/Rust Home pairs against
wire v1, then Rust↔Rust v2 and negotiated fallback. Gateway and connector replacements are sequential with exclusive service
ownership; helper/hook concurrency is covered separately below. Binary rollback
uses **current** state, never automatically restores old sessions/credentials that
could revive revoked access. Gate every persisted schema on cross-version readers.
New schemas require explicit migration and downgrade decisions before release.

### State locks and executable handoff

- Home `home-connector.lock` is a lifetime `flock`; recovery/workspace/session/workflow
  stores have separate transaction locks. Preserve lock paths, `flock` semantics,
  acquisition deadlines, lock ordering and close-on-exec. Never unlink a live lock
  inode or substitute an unrelated lock API. Test Go connector + Rust helper and
  Rust connector + Go helper, with concurrent workflow hooks and recovery requests.
- The current Go gateway has process-local store mutexes, not a gateway-wide
  cross-process state lock. Isolated ports alone do not make shared-state canaries
  safe. Use separate synthetic state for staging; stop and verify exit of the old
  gateway before activating another against production state. A new Rust lock
  cannot constrain the old Go process unless Go also participates.
- The existing Home installer replaces two files sequentially. A crash can leave
  one new binary and one old helper; do not assume pairwise atomic activation.
  Test every transitional pair or introduce verified release-directory activation
  with recovery for interrupted installation. Retain old executables and service
  definitions before changing either file. Hooks continue to find `hmux-agent`.
- A role cutover must record version/hash, service PID/birth identity and original
  tmux identities; verify Home freshness, authenticated operations and public auth
  barriers. Reconnects are expected during replacement; zero downtime is not promised.
  Rollback changes binaries/service definitions while retaining current state.
- Start retirement only after the complete Rust stack passes the 72-hour soak,
  each OS acceptance gate and current-state rollback rehearsal, with no unresolved
  release-blocking defect. Keep the known-good Go artifacts and compatibility
  evidence available after source deletion; removal is a separate reviewed change.

### Protobuf target transport (user decision, 2026-09-24)

Use Protobuf binary messages over the existing Home↔gateway WebSocket. Do not add
gRPC, HTTP/2 infrastructure, a sidecar or another persistent connection. Browser
terminal bytes are already binary; keep that path and browser JSON HTTP APIs.
Persisted authentication/config formats remain unchanged for rollback.

Design the versioned `.proto` schema during the foundation phase, with generated
Rust types and schema/codegen drift checks. Internally use typed operations and
bounded byte buffers; JSON v1 is a compatibility adapter, not the permanent core.
The initial Protobuf envelope may carry explicitly named JSON control payloads
while typed control schemas are completed; never call that stage full Protobuf
parity. Terminal/input/upload byte fields must avoid base64 from the first version.

Require explicit WebSocket subprotocol selection before sending v2 frames. No
selection means legacy v1 where supported; auth/TLS failure never triggers a
weaker fallback. Never send an unsolicited server hello to an old Home. Document
mixed Go/Rust/protocol pairs, one connection generation per peer, frame direction,
operation allowlists, IDs, exact ACK units and all byte/count/time limits.

Protobuf is not a validation or authorization layer: reject unsupported operation
enums, absent/wrong oneofs, invalid identities and oversized fields before dispatch.
Bound the WebSocket message before decode, parser recursion, repeated fields,
unknown-field skipping and retained buffers. Preserve FIFO request/control order
and per-view fairness. Do not treat zero-copy slices retaining a large allocation
as free memory. Reserve retired field numbers/names and review schema evolution.

Compare JSON v1 and Protobuf under identical throughput/credit/connection limits:
encoded size, encode/decode CPU, allocation/retention, RSS/PSS, p99 and error rate.
Removing base64 reduces that payload's wire bytes by roughly 25% from its encoded
size; it does not prove an equivalent whole-product memory or latency improvement.
Catalog revision/lease and smaller windows remain separate subsequent experiments.

The goal is an entirely Rust native runtime with efficient Protobuf Home transport.
Schema and codecs can advance alongside Go parity work; production activation
still requires interoperability, security, load and rollback gates.

## Measurement and verification

Measure gateway, deployment stack, Home and provider/tmux costs separately. Linux
RSS/PSS/cgroup and macOS footprint are different metrics; do not add overlapping
figures. Exclude/report driver and profiling overhead. Record revision, binaries,
toolchain, dependencies, machine limits, workload seed and raw synthetic samples.

Port the proposal's S00–S14 workloads: readiness, connected idle, 1/2/4/8 views,
normal/slow output, concurrent KDF/upload/typing, 10,000 lifecycle churn,
reconnect/replacement, revocation, malformed input, storage failures, long soak,
capacity rejection and catalog limits. Add Home-specific catalog scan latency,
process/provider lookup, transcript growth, source polling and burst-to-idle tests.
Phase 0 freezes scenario seeds, warm-up, OS/machine definitions and byte/count/time
budgets before comparing candidates. Version budgets for both gateway and Home;
separate observed Go values, hard safety limits and proposed Rust targets. Until
measured budgets and required environment results exist, a phase may proceed in
implementation but cannot be marked release-ready. For interactive p99, start with
a regression allowance of max(10% of paired baseline, 1 ms); ratify it with noise
measurements and equal throughput/success rates. Do not silently relax failed gates.
Use paired runs and report throughput/errors with p50/p95/p99. Distinguish socket
receipt from the actual xterm write callback. Measure cold and warm readiness.

Existing Go tests are behavior evidence, not code to transliterate mechanically.
Retain current `make check`, `make integration` and `make build` during coexistence;
add Rust fmt/clippy/unit/integration and advisory/license checks incrementally.
Rust's type system and clippy do not replace Go race-test coverage: retain behavioral
concurrency tests for ACK races, cancellation, revocation, locks and worker shutdown.
Add bounded parser fuzzing/property tests and fault injection; task panics and disk
failures must produce a defined failed/closed state, not an apparently healthy hub.
Run host tests on macOS and Linux; cross-compilation alone cannot verify PTYs,
process birth identity, filesystem durability or service managers. Preserve the
current Linux-amd64 gateway/macOS-arm64 Home bundles; add Linux Home packaging and
native acceptance before advertising a complete supported Linux Home bundle.

Use only isolated tmux sockets and disposable `hmux-e2e-*` resources for tests.
Do not attach to, rename, detach or kill pre-existing sessions. Runtime inspection
and live rollout remain distinct from synthetic validation. Physical Safari/iOS
and Android input/background/resume acceptance remains required for release claims.
