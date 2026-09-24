# Rust migration evidence through 2026-09-24

Historical snapshot, not current instructions or deployment status. Start with
[the current migration control](../RUST_MIGRATION.md). The original document
SHA-256 before this archive wrapper/link adjustment was `bc9dfb98950818739bdb92b7e622999e4c8f0af7e5aa8359e2a37bbac5ab7415`.
All implementation/checkpoint claims below retain their original scope and date.

---

# Full Rust migration plan

Status: implementation in progress; candidate gateway and Home foundations,
production remains Go. No comparative runtime performance result yet. Updated 2026-09-24.
Source baseline: `245d4e6839939839fd56b7632600d7398726e359`.

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

This is the sole current Rust migration plan. Architecture/Operations describe the
running Go product until each replacement is verified. Validation records actual
checks and deployment; this plan does not claim a deployment has occurred.

### What “complete” means

- `hmux-web init|serve|connect|service` and every supported `hmux-agent` command
  are implemented in Rust, retaining executable names and external contracts.
- Usage collection runs in the Rust Home process; there is no Go sidecar, Go FFI
  library or Go subprocess helper. Maintained OS/PTY bindings and external provider
  commands remain allowed; they are not hidden Go implementations of HMux.
- Native release builds and normal contributor checks no longer require Go.
  Replace the current Go-based shfmt invocation with a pinned tool installation.
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

## Initial implementation order

The implementation began with Phase 0, without replacing production:

1. Inventory every CLI/route/state contract with owner, fixture and pass/fail status.
2. Extract synthetic wire/auth/state/provider fixtures from current behavior and
   test cross-language-sensitive values (base64, time, omission and integer bounds).
3. Build a bounded synthetic Home/browser harness and baseline result schema;
   capture Go readiness, idle/view memory and Home collection spans separately.
4. Establish the Cargo workspace and codec-only compatibility check after those
   fixtures exist; do not introduce a second authentication implementation first.

The contract matrix must cover every native package, CLI command, route, stored
schema, OS branch and embedded collector dependency. Each row records owner,
fixture/test, baseline/candidate result and implemented/verified/skipped status.
Record intentionally strengthened behavior separately; do not reproduce an unsafe
Go behavior merely for parity. This first tranche changes no production service.

Track phase state here; put detailed compatibility fixtures beside their tests
and measured results under the benchmark artifacts. No redundant live worklog.
Current implementation checkpoint (2026-09-24):

- Phase 0 **partial**: native owner/CLI/route/state inventory in
  [contracts.json](../../tests/fixtures/contracts.json); Go-generated wire, auth and
  model/config fixtures. The S00/S01/S02 synthetic driver now starts either Go
  or the full Rust gateway candidate with sanitized environments and the same v1
  Home workload. Both roles passed macOS startup/login/RSS harness smoke runs,
  including 100 sessions and two quiet views; these are not a release baseline.
  S03–S14, paired/current-vs-tuned runs and Linux resource results remain pending.
- Phase 1 **partial**: Cargo workspace/toolchain/lock, model/config read-only
  crates, auth primitives/session DTOs, bounded v1 codec/output-credit state and
  Protobuf v2 schema/generated codec/preflight validation and JSON adapter.
  Shared WS I/O task has bounded admission, independent write deadlines and
  teardown tests, including detached cleanup without one task per send. The same
  task sends a nonce Ping every 15 seconds, requires its exact Pong within 10
  seconds and flushes peer Pongs without application writes. Paused-clock tests
  cover wrong/missing Pong, paused writes and shutdown. Private
  read/atomic-write/flock primitives and bounded password-work admission exist.
  HTTP/1 Host/Origin/cookie/CSRF boundaries and bounded upgrade lifetime are now
  exercised over real loopback sockets. Authenticated Home upgrade selects JSON
  v1 or Protobuf v2 on the same socket, with no fallback on failed authentication.
  Independent foundation reviews and integrated checks passed on macOS after
  correcting integer narrowing, object-shape decoding, output frame size, idle
  socket teardown and Home hard-link checks. The separate auth-only and full
  experimental gateway executables are described below; a complete Home
  executable and full native-runtime parity remain pending.
- Phase 2 **partial**: authentication transactions, private credential/session
  persistence, restart login, account-scoped revoke, TOTP replay/toggle and checked
  admission errors are integrated with the authentication HTTP routes. Synthetic
  HTTP and separate-process restart/logout tests pass on macOS. Current Rust state
  is accepted by Go and Go logout remains revoked on return to Rust. Disk/panic
  failures cancel access; admission pressure produces 503 instead of false expiry.
  The opt-in library Home route now joins its bounded hub and transport on
  shutdown. It rejects duplicate peers, isolates connection generations, bounds
  requests/views/uploads/completions and preserves FIFO output ACKs. Synthetic
  TCP tests cover both wire versions, reconnect/cache clearing and shutdown;
  duplex tests cover slow-view isolation and request/upload teardown. Hub
  completion notifications use a 64-event best-effort queue: a full queue or
  missing observer drops the new notification, matching Go. It never stalls
  or disconnects the shared Home reader. Requests and terminal output continue
  while notification delivery is slow or disabled. Before enqueue, IDs, exact
  identities and timestamps are validated (two minutes old/one minute future).
  A five-minute deduplication table holds at most 4,096 fixed-size IDs across
  reconnects; only successfully queued events enter it. No extra worker or timer
  is added. These metadata bounds are separate from the payload budget below.
  Hub payloads share an 8 MiB retained-allocation budget, including bytes returned to
  callers and old-generation snapshots. Credits follow the last clone/slice;
  long-lived decoded fields are copied out of their larger backing allocation.
  The opt-in library browser terminal route now forwards binary input/output
  with JSON controls, FIFO ACK and legacy write-completion pacing. Authentication
  cancellation and exact expiry cover pending opens and blocked writes; closing a
  browser drops only its disposable view. Synthetic HTTP/duplex tests cover
  account isolation, forged view IDs, invalid ACK/input, abandoned opens, Home
  replacement, expiry and shutdown during a blocked output frame. The assembled
  gateway now also passes the actual-Go Home integration described below;
  physical browser/device acceptance remains a separate gate.
  Independent review found and corrected a full-credit-window exit loss: data
  stays capped at 32 queued frames, while the 64-slot event queue also admits
  refresh/exit controls. A deterministic shared-reader barrier verifies 32 queued
  data frames, refresh and normal exit drain in order with all FIFO ACKs.
  Opt-in library usage settings now persist per account/profile with Go-compatible
  filenames, defaults and revisions. The cache holds at most nine accounts; two
  blocking-work slots stay held through actual completion after caller cancellation.
  Private target checks precede atomic replacement, and shutdown joins writes.
  Origin/CSRF, stale revisions, corrupt state, restart and current-state Go/Rust
  handoff have synthetic coverage. Independent review found queued writes could
  outlive revocation; workers now recheck cancellation/expiry after acquiring the
  mutex and immediately before beginning atomic persistence. Already-started
  atomic transactions may still complete. The `/api/state` library route borrows
  retained JSON without a decoded value tree, preserves offline/null semantics and
  omits temporarily unavailable settings. Review also caught a combined response
  cap smaller than accepted Home snapshots; the HTTP regression now exercises a
  near-4-MiB catalog plus two 1-MiB usage snapshots together.
- A bounded native command-runner foundation is integrated in `hmux-core`:
  immediate finite admission, argument arrays, stdout limit, 64 KiB stderr,
  explicit partial-exit acceptance and deadline/cancellation cleanup. Synthetic
  subprocess tests verify the owned direct child is killed/reaped before freeing
  admission. It does not kill process groups or descendants. It is not yet wired
  to service installation; Home collectors, actions and PTYs now use the runner.
  Returned output remains the caller's retention responsibility.
- Phase 3 **partial foundation**: the Home library reads basic tmux sessions and
  windows through two bounded argument-array commands. It preserves grouped-view
  hiding, attachment counts, identity, dimensions, sorting and basic shell labels.
  Parsing borrows row/field slices, without an intermediate row table. Synthetic
  fake-command tests and a Go-generated nine-case catalog oracle pass. Separate
  bounded provider inspection/public conversation owners and disposable PTY/view
  handling are integrated as described in the Home checkpoints below. Recovery,
  completion and production CLI wiring remain incomplete.
- Shared workspace now includes a **storage and API foundation**: bounded 32-tab
  delta merge, 64-operation replay history, exact lifetime and unique restore
  lineage match the actual Go oracle through real Rust file transactions. Missing
  tabs survive; stale closes preserve another device's unseen opens; selection
  stays device-local. Revision overflow and invalid resulting restore identities
  fail closed. One owner shares two blocking slots across all account scopes,
  uses a persistent Go-compatible flock with a three-second deadline and writes
  private atomic JSON of at most 32 KiB. Catalog acquisition precedes the lock;
  parsing keeps only identity/restore fields, skipping labels/process metadata.
  Cancellation checks run while waiting and immediately before persistence;
  actual workers retain admission after callers disappear, and shutdown joins
  them. No per-profile worker or resident workspace cache is added. Synthetic
  tests cover corruption/links/modes, contention, concurrent owners and restart.
  Account workspace HTTP routing isolates additional accounts and forwards the
  primary workspace to Home. Action forwarding reconstructs allowed fields,
  generates transport IDs and propagates logout/20-second timeout cancellation.
  Real Home recovery, production endpoints and OS/device acceptance remain pending.
- The library now has an opt-in **static asset foundation** via
  `Gateway::with_assets`: configured on-disk release files are streamed, not
  embedded or preloaded. Each request reopens the trusted configured root, so
  switching the deployment `current` symlink takes effect without restarting the
  gateway; an in-flight response keeps its opened file. Relative client paths are
  resolved with directory descriptors and no-follow opens. Public files do not
  inherit private-state ownership requirements. GET/HEAD, index redirects, MIME
  types used by the web build, modification-time conditions and single/multipart
  ranges match a 63-case actual Go gateway oracle (the platform-dependent Markdown
  MIME header is normalized to the candidate plain-text notice policy). Synthetic
  tests also cover a 3 MB real HTTP response, disconnects, deployment switches, truncation/growth,
  descriptor admission and retained small byte slices. One asset owner admits
  at most 16 streams and four blocking reads/opens. Reads occur on demand in
  32 KiB chunks with 64 shared backing-allocation credits (2 MiB); there is no
  preload cache or resident per-client task. Cancelled callers retain worker
  admission through completion, and shutdown joins workers. These bounds exclude
  HTTP/kernel buffering, MIME metadata and allocator overhead; they are not RSS
  measurements. The candidate deliberately rejects directory listings, dotfiles,
  traversal, descendant symlinks/special files, non-ASCII published paths and
  encoded reserved API paths. It serves Markdown notices as plain text and uses
  octet-stream for unknown extensions, preserves no-store with generic static
  errors, and ignores more than 32 ranges.
  These are explicit differences from Go's general-purpose FileServer, not claims
  of identical behavior on arbitrary directories. CLI/release wiring and Linux/
  device acceptance are still pending; the auth-only executable remains unchanged.
- The opt-in **browser diagnostics foundation** via `Gateway::with_diagnostics`
  accepts only bounded numeric metrics and allowlisted labels; no messages,
  URLs, stacks or terminal contents are retained. Account/profile exports omit
  private owner keys. Cookie/Origin/CSRF checks and reauthorization after a slow
  body preserve revocation. One ring retains at most 2,048 events, 256 per owner,
  for seven days by server receipt time; 1,024 login rate buckets allow six
  batches/minute each. Source metadata is shared across records without a
  separate retained interning cache. The 16 KiB request and 2 MiB private-file
  limits remain separate. One ten-second save task serializes bounded snapshots
  outside the state lock, preserves appends made during a save, retries transient
  errors and joins the real blocking final save on shutdown. Corrupt/unsafe
  startup files disable ingestion without overwriting originals; unexpected
  file changes observed before a save also stop replacement. This is a
  single-owner state file: digest-check then rename is not an atomic compare-and-
  swap against concurrent external editors. Strict duplicate/object parsing and the
  private no-link policy remain candidate safety differences. Synthetic tests
  cover limits, exact receipt TTL, isolation, restart, revoked slow requests,
  corrupt/changed files and shutdown. A Go-generated oracle covers 41 decode
  cases and 140 state transitions, using compact event templates with actual
  record hashes. Executable wiring, Linux execution and full-process resource
  measurements remain pending.
- The opt-in Gateway library now includes a **browser upload foundation**. The
  gateway checks the start CSRF/session metadata, hashes streamed chunks across
  file boundaries and forwards each browser ACK only after Home acknowledges
  the exact cumulative byte count. It validates completion identity, file sizes,
  SHA-256 values and canonical staged paths before returning the typed result.
  It never writes staging files or collects whole bodies. Limits remain 16
  files, 32 MiB/file, 128 MiB total, 256 KiB/chunk, 16 KiB start/finish controls
  and 64 KiB completion metadata. Two fixed limiter slots enforce global and
  per-login admission without a growing map. Unlike Go, a slot is reserved before
  constructing a data-sized WebSocket, including the initial frame wait; denied
  sockets use a 16 KiB frame ceiling. The first frame gets ten seconds, transfer
  idle phases thirty seconds and the overall transfer five minutes. Admission
  stays held through browser transport cleanup. Existing bounded transport
  heartbeat/write limits also apply; these safety bounds are not RSS claims.
  Account cancellation, exact expiry and periodic auth checks cover blocked
  operations; abandoned transfers drop their generation-bound Hub lease and
  never reroute onto a replacement Home. Strict object/duplicate decoding is a
  documented candidate difference. A synthetic Go oracle contains 17 start and
  19 completion cases. A real Go peer/`runHomeUpload`/filestage receiver has also
  accepted Rust Gateway traffic and committed the expected binary bytes, hashes,
  0600 files and three-hour expiry in an isolated spool. Only the tmux verifier
  and spool path are substituted; no real session is inspected. Full Go Home
  connector startup and Rust Home staging/sweeping have later candidate evidence
  below; complete Rust gateway/Home/browser and device acceptance remain gates.
- The push storage library now owns private Go v1 VAPID/subscription persistence
  with a lifetime flock, 2 MiB input/output cap and at most 256 subscriptions.
  It uses two startup slots and two blocking transaction slots; cancelled callers
  retain admission until the actual worker exits. Shutdown joins transactions
  and releases the lock even with live handles or a poisoned mutex. Subscription
  transfer/pruning requires an authoritative nonblocking auth callback under the
  store lock; lookup errors abort the transaction. Cancellation, expiry and auth
  are rechecked immediately before persistence; an already-started atomic write
  may still finish. File changes or write errors fail the owner closed, preserving
  malformed/private-path failures. The digest-before-rename guard is not atomic
  CAS against noncooperating external editors.
  The actual Go owner supplies 26 endpoint, 13 key, 8 login-ID and 26 state cases.
  Rust deliberately rejects duplicate/unknown JSON fields, noncanonical base64
  and ambiguous/raw non-ASCII URLs; [fixture notes](../../tests/fixtures/push-v1/README.md)
  record those differences. Current-state Rust→Go→Rust handoff and a real Go
  attempt against Rust's lifetime lock pass on isolated files, preserving the
  same VAPID key and a Go-side unsubscribe. P-256 uses the RustCrypto
  dependency. This storage slice added no TLS client or resident worker; the
  subsequent transport and browser integration are recorded below.
- Web Push request preparation now uses P-256 ECDH, HKDF-SHA256 and AES-128-GCM
  with a fresh OS-random salt and ephemeral key for every message. Plaintext is
  capped at 3,993 bytes; header, padding, ciphertext and tag share one 4,096-byte
  body buffer. VAPID uses ES256 with a raw 64-byte signature, canonical push-service
  audience, configured HTTPS subject and 12-hour expiry. TTL 120, normal urgency
  and the login/payload-derived topic match Go. No JWT library or HTTP/TLS stack
  was added. The private store prepares messages in its existing two-slot blocking
  pool without exporting its scalar, checking login cancellation/expiry and the
  exact endpoint/key tuple before and after crypto. Queued revoked preparation,
  same-endpoint key replacement and cross-login requests are rejected. The
  network sender revalidates authoritative auth/current subscription and watches
  cancellation at send time; a prepared request is not a send grant.
  RFC 8291's published ciphertext matches byte-for-byte. An actual Go helper
  decrypts Rust output and verifies its ES256 signature, then captures the real
  Go Web Push sender for Rust decryption/signature validation. Empty, Korean
  tab/session JSON and maximum binary payloads pass without contacting a provider.
  Selected crypto additions are aes-gcm 0.10.3 (aes/alloc only), hkdf 0.12.4 and
  the existing p256 0.13.2 with ecdh/ecdsa. A scoped RustSec lookup at commit
  1931a145168d457fd79b321277b25b0e51777157 found aes-gcm's RUSTSEC-2023-0096,
  fixed by the selected 0.10.3; no advisories appeared for the other queried new
  crypto packages. This is not a full dependency audit or process-memory result.
- The candidate HTTPS push transport uses the existing Hyper HTTP/1 machinery
  with rustls 0.23.45, tokio-rustls 0.26.5 and native trust roots 0.8.4. It has
  two process-wide immediate outbound slots, no waiting send queue, proxy, redirects or idle
  TCP pool; TLS session resumption retains at most four server entries.
  One OS trust loader caches the shared configuration, accepting at most 1,024
  roots / 2 MiB of certificate input after the native load. Native loading and
  resolution can allocate internally; these limits do not bound OS internals.
  DNS preserves the host resolver, retains admission through the actual blocking
  call and collects at most 33 results to reject overflow above 32. Every address
  must pass the Go-compatible public-address policy before any pinned dial; TLS
  verifies the original hostname. One absolute deadline is the minimum of ten
  seconds, login expiry and the parent deadline. Revocation and shutdown cancel
  pending work; the owner-supplied authoritative callback runs immediately before
  POST, followed by local expiry/cancellation/deadline checks. Hyper's driver stays
  inside the send future, with 8 KiB header/read buffering, 64 response headers
  and a 4,096-byte response discard. Received status survives body-copy errors.
  An actual Go oracle agrees on 217 address cases, including CIDR boundaries and
  mapped IPv4. Synthetic TLS tests cover trust/hostname/expiry, exact POST/SNI,
  redirects, response limits, stalled TLS/headers/body, revoked authorization,
  blocked DNS admission and no late dial. Recreating a transport owner cannot
  bypass permits held by a previous owner's blocked resolver. No push provider
  is contacted.
  The lock selects rustls-webpki 0.103.15 and ring 0.17.14, above the scoped
  RustSec fixes for RUSTSEC-2026-0285, 2026-0104 and 2025-0009; this is not a
  complete advisory audit. TCP pooling/throughput and whole-process memory still
  require measurements.
- `Gateway::with_push` now installs one consumer of the Hub's existing bounded
  completion receiver, the private store and concrete HTTPS client. It adds no
  second queue or deduplication table. Browser config/subscribe/unsubscribe/
  presence/test routes keep cookie/CSRF authority, per-login ownership, explicit
  endpoint transfer and strict separate subscription DTOs accepting browser
  `expirationTime`. Presence is bounded to 256 logins × 16 clients with a 45-second
  TTL; the 30-second test throttle retains at most 256 entries. Each completion
  has a 30-second delivery deadline; individual sends retain the shorter transport
  deadline. Workspace membership uses exact account-scoped session identities.
  Before POST, workspace is the final awaited read, followed by nonblocking exact
  endpoint/key, authoritative login, catalog generation and presence checks.
  Contention or unavailable authority suppresses delivery, never prunes logins as
  expired. Stale 404/410 cleanup is guarded by the attempted endpoint. Shutdown
  cancels/joins the completion worker and drains state transactions.
  A synthetic HTTP/Protobuf/verified-TLS test covers per-account routing, presence,
  reused session IDs, endpoint transfer, test throttling, 410 removal and logout.
  Paused handshakes prove that presence/workspace changes during preparation are
  rechecked. The captured encrypted POST is independently decrypted and its VAPID
  signature verified by Go, including the Korean tab name and exact deep-link
  identity. The Go HTTP oracle contributes 41 raw-body cases; deliberate strict
  duplicate/object policies are documented in the fixture. These are library
  candidate checks; provider/browser-device acceptance remains outstanding.
- The auth-only executable now uses a native process runtime owner with a
  one-second final shutdown wait, including unwinding. It first joins gateway
  cleanup on normal return. Auth shutdown closes admission, cancels sessions
  immediately and drains actual password/storage workers even after their HTTP
  callers disappear. Already-running atomic writes finish; queued writes stop.
  An isolated child-process test leaves a synthetic native resolver permanently
  blocked and proves finite exit after normal return and a root-future panic.
  The OS call is not cancelled or joined by the timeout. Panic exit does not
  promise graceful persistence; full Home process/child-owner joining remains
  an integration gate before using this runtime policy there.
- Phases 3–7 **not complete**: PTY/provider execution, collector replacement,
  service/installer replacement, release soak, device validation or deployment.
- Production remains Go. Nothing from candidate builds has been installed.

Foundation evidence: Rust formatting/clippy/workspace tests and release-library
build, Go/Rust wire/auth/model/config fixtures, macOS cross-process Go/Rust flock,
Go config/model/gateway race tests and relevant vet checks passed. Generated
Protobuf source matches a local regeneration. This is not full `make check`,
device acceptance, Linux execution, advisory/license audit or release validation.
The earlier crypto checkpoint passed 206 workspace/all-target tests; its eight
default-ignored handoff tests passed separately through `make rust-compat`.
Release libraries and the auth-only example built, and the three-process
restart/revocation/SIGTERM smoke passed. The parallel HTTP fixture now adds a
process-local counter to its timestamped path after an initial collision was
found in the full suite; the corrected full suite passed. Parallel synthetic
HTTP fixtures also serialize only their store-open step, avoiding accidental
exhaustion of the production two-slot startup admission. Product admission
limits remain unchanged. Go basic catalog, usage-preference and workspace
oracles passed with `-race`; relevant gateway/workspace vet checks passed. The
workspace oracle covers 10 merge cases and 76 successive real Go store results.
The usage-settings/state tranche received an independent read-only review; the
primary fixed both findings and verified regressions. The workspace storage/action
tranche received a separate independent read-only review with no material findings.
The final compact catalog projection and invalid-restore guard were checked in the
primary thread; affected model/storage tests, formatting/clippy and compatibility
handoffs passed after the guard. Rust-written workspace revision 1 is accepted and
advanced by Go, then reloaded as revision 2 by Rust from the same private file.
The static-streaming tranche passed independent read-only review with no material
findings. Its final suffix-range guard and plain-text Markdown notice policy were
checked in the primary thread; formatting/clippy and all eight affected static
tests passed afterward. Go static oracle race/vet checks also pass. No static filesystem contents or frontend bundle were embedded in Rust.
Diagnostics now have a current-state Rust→Go→Rust handoff too: Go reads Rust's
private history, appends an event and persists it; Rust reloads both events from
that same file. The macOS HTTP test client now accepts a connection reset only
after verifying a complete Content-Length-framed response, covering early
oversized-body rejection without weakening request limits.
The diagnostics tranche passed independent Sol read-only review with no material
findings; the single-owner external-edit limitation above is retained explicitly.
Go diagnostics oracle race and gateway vet checks passed as well.
The upload tranche passed independent Sol read-only review with no production
blocker. The review identified a paused-clock test that advanced past one timer
before starting the next; the corrected test drives both deadlines without
changing runtime limits. The integrated suite now covers first-frame timeout,
expiry, pending Home and blocked browser writes. Actual Go upload receiver
interoperability, Go upload oracle race and gateway vet checks also passed.
Push contract exploration found a candidate completion-overflow disconnect;
the Hub now uses Go's best-effort notification policy. Independent review then
identified missing prequeue freshness/deduplication, which is now implemented
with exact boundary, retry, cap and shared-reader regression tests. Independent
Sol re-review found no remaining material issue. The completion-admission
checkpoint passed its 190-test workspace run, formatting/clippy, release-library
build and Go push tests under `-race`. The subsequent storage tranche passed
independent Sol read-only review with no material findings, ten focused Rust
tests and the real Go handoff/lock check. At that checkpoint encrypted delivery
and browser routes were unimplemented; subsequent slices are recorded below.
The integrated storage checkpoint also passed workspace formatting/clippy,
release-library and auth-example builds, the three-process restart/revocation
smoke, and Go push oracle race/vet checks. No production state was used.
The crypto/preparation tranche passed independent Sol read-only review with no
material findings. Its final 206-test workspace run, eight separate compatibility
handoffs, formatting/clippy, release-library/auth-example builds, three-process
auth smoke and Go push state/crypto oracle race/vet checks passed on macOS.
The HTTPS transport/shutdown checkpoint passed 217 workspace/all-target tests,
eight separately exercised compatibility handoffs, formatting/clippy, release
library/auth-example builds, the three-process auth restart/revocation/SIGTERM
smoke, and Go push/address oracle race/vet checks on macOS. Independent Sol
read-only review identified that multiple transport constructors could bypass a
per-owner DNS budget. Admission is now process-wide, with a cross-owner blocked
DNS regression test. Re-review accepted that fix and identified a redundant
native-root load race; a second TLS-cache check under loader admission fixes it.
The final 11 focused transport tests, formatting and gateway all-target clippy
passed after those fixes; the full compatibility checks are unchanged.
The push owner/routes checkpoint passed 224 workspace/all-target tests and
workspace formatting/clippy. Independent Sol review found an async subscription
wait after workspace authorization; final subscription lookup now uses a
nonblocking exact-key check, and re-review found no remaining material defect.
Nine tests are ignored by default: eight compatibility handoffs plus the new
native trust check. All eight handoffs and the new independently verified TLS
capture passed through `make rust-compat`. Release-library/auth-example builds,
three-process auth restart/revocation/SIGTERM smoke, Go push oracle race tests
and gateway vet passed. The actual macOS native-client initialization/cache check
passed outside the sandbox; a read-only probe found 156 roots / 168,516 DER bytes
and no load/parse errors. The sandbox exposed zero roots with no error, correctly
rejected by the client. Packaged OS/provider/device validation is still pending.
These are synthetic local checks, not deployed push support or device validation.
A seven-round release-mode codec smoke (10,000 iterations per sample) completed
on macOS: synthetic 16 KiB terminal output encoded to 21,930 bytes with JSON v1
and 16,410 bytes with Protobuf v2. Raw codec wall-time samples and source hashes
are local benchmark artifacts; this is not a whole-runtime CPU/memory result.
The candidate private-file helper also rejects hard links and symlink ancestors;
each migrated state owner must preserve/document its accepted path policy when
integrating the helper. Authentication stores now apply this policy to private
reads and validate existing credential identity before replacement. Remaining
production Home/device interoperability and Rust Home execution are pending.

The separate experimental `gateway_candidate` now assembles the verified owners:
AuthStore, Hub, configured assets, preferences, account workspaces, diagnostics and
push with its concrete TLS client and existing completion receiver. It requires
`--experimental-gateway`, explicit absolute input/asset paths and a loopback
listener. Every asset open checks the resolved release root against primary
credentials/token and private users, credential backups, usage preferences and
account workspace directories; checked path components and request descendants
are opened without following links. Valid public release switches still work.
Startup failure drains initialized stores. SIGTERM/SIGINT handlers register
before initialization; if interrupted, the existing startup future is awaited
and its resulting owners are drained before any listener begins serving.
`make rust-full-gateway-candidate` builds this role; `make rust-gateway-e2e`
connects actual Go `connectOnce` catalog/recovery/request workers and a real owned PTY,
using synthetic tmux/ps/lsof commands. The helper starts with a sanitized
HOME/PATH, absent provider credentials, disabled provider scans and an HTTP dialer
that rejects every destination except its loopback gateway. A temporary synthetic
CA exercises ordinary certificate loading without depending on host keychains;
no provider connection is made. Go recovery still reads the public OS boot ID,
which requires leaving the constrained macOS sandbox in this environment.
This is executable/protocol integration, not live tmux, OS-service, provider or
physical browser acceptance. The outer Go WSS/reconnect/singleton wrapper is not
under test here; the harness explicitly starts a replacement `connectOnce`.
The auth-only smoke and production Go paths remain.

The assembly checkpoint passed 228 workspace/all-target tests (nine separately
opt-in tests), workspace formatting/clippy and all existing `rust-compat`
handoffs. A release executable plus a Go race-instrumented Home/browser harness
passed catalog/profiles/workspace, stale session birth rejection, 768 KiB output
with exact per-frame ACKs, input/resize, browser-close cleanup, Home replacement,
logout and SIGTERM checks on macOS. A temporary FIFO stalls certificate loading
to verify SIGTERM during startup exits cleanly without reporting readiness.
Independent review found live asset-root switching could escape the initial
private-state guard, then identified the secondary-account directory; the fix
also excludes credential backups, confirmed by main source tracing. Focused
regressions require unauthenticated private-file reads to return 404, even without
an index file, while a subsequent public release serves successfully. These
checks do not imply production activation or measured memory gains. Focused
re-review found no remaining concrete defect. The final gateway library suite
passed 91 tests (five ignored handoffs/native checks); both release examples,
three-process auth smoke, final actual-Go race subprocess check and gateway vet
passed after the fixes. The earlier eight handoffs and TLS-capture check passed
through `rust-compat`; the final changes only tightened asset boundaries and
startup signal handling.

The first **read-only connected Home slice** now accepts an already authenticated
transport, publishes the basic catalog immediately and five seconds after each
completed scan/send, and reloads validated inventory for `profiles`. Both wire
versions carry an empty capability list; unsupported operations fail explicitly.
One inventory I/O slot stays held until its real worker exits, with at most eight
request owners. JSON serialization caps logical length and backing capacity at
  4 MiB minus 512 bytes; both wrappers fit the existing 4 MiB frame limit. This
excludes the validated inventory/catalog trees and temporary codec copies and is
not a whole-Home memory budget. Shutdown joins catalog commands, request/file
workers and transport. Independent review found caller abort could orphan a
collector: one private peer task now retains cleanup, while dropping the public
waiter requests cancellation. Completed cleanup still requires cancelling and
awaiting the public future with Tokio kept alive. Synthetic tests cover abort
before polling, during catalog sleep and during a slow owned command, missing
tmux, inventory changes, duplicate/cancelled requests and size pressure.
The subprocess check passes against the actual race-instrumented Go
connector endpoint/hub, including ordered profiles, unsupported workspace/open
responses, a subsequent successful request and cleared state after disconnect.
Its tmux commands and inventory are synthetic; it does not exercise browser
authorization, WSS, reconnection or real host state. Re-review found no remaining
material peer defect after the ownership fix.
An additional Rust Hub + Rust Home integration check passes with both v1 and v2,
including catalog/profiles, explicit unsupported replies, stale-generation
rejection after replacement and zero retained Hub payload after both owners join.
It uses duplex sockets and synthetic tmux, not HTTP/TLS or a physical browser.
The integrated workspace passed formatting, all-target Clippy and 243 tests
(ten explicitly opt-in tests), plus the separate Go race-instrumented peer
handoff on macOS. Protoc 35.1 regeneration matches the checked-in types; the new
checksum-pinned Linux CI job has been configured but not run here. The expanded
eight-case codec smoke validates round trips and encoded sizes; it does not
measure whole-process memory, CPU or rendered terminal latency.

The **bounded client upgrade** now offers v2 once over a caller-supplied verified
stream. The first integration exposed tungstenite 0.28 rejecting a valid 101
without a selected subprotocol; an inline Hyper upgrade now validates HTTP/1.1,
Upgrade/Connection, the expected WebSocket accept and the optional single selected
protocol before handing the same socket to the existing WebSocket transport.
No HTTP task, retry, redirect following or second connection is added. Headers
are capped at 8 KiB/32 fields, with a five-second or earlier parent deadline.
Cancellation/drop owns and releases the stream; buffered first WebSocket bytes
are retained. Main review strengthened authority/port and Connection-token
validation and marked Authorization sensitive. Eight synthetic upgrade tests
pass, including v2 selection, valid unselected v1, malformed/duplicate headers,
auth/redirect rejection, buffered data and socket teardown on timeout/cancel/drop.
The race-instrumented actual Go endpoint/hub test now verifies the v2 offer and
same-socket v1 fallback. All-target workspace Clippy and 38 Home tests pass after
integration; the separate Rust Hub/Home dual-codec check also passed.
Independent frozen-scope review found no material defect in this upgrade slice;
TLS hostname binding remains explicitly outside it.

The **Home singleton foundation** now uses the existing persistent
`home-connector.lock` inode and nonblocking exclusive flock. Startup traversal
checks every ancestor through a directory descriptor, rejects symlinks and
untrusted ownership/permissions, creates missing components as 0700, and leaves
existing directory permissions and lock contents unchanged. Errors expose fixed
categories only. Four synthetic macOS checks cover unsafe paths/files, nested
creation, inode preservation and concurrent acquisition. The separate
`rust-home-lock-compat` check uses the actual race-instrumented Go
`homeservice.LockConnector`: each language excludes the other, normal and forced
Go process exit release ownership, and the inode survives both handoffs.
Independent frozen-scope review found no material defect. The candidate reconnect
owner below now retains this guard; system service integration, Linux execution
and full process-lifetime acceptance remain pending.

The **verified WSS dial foundation** now binds DNS, TLS hostname/SNI and HTTP Host
to one validated `wss://authority/connect` endpoint, including bracketed IPv6 and
explicit ports. A process-wide outbound slot survives cancelled native resolution
and remains attached to a successful socket through TLS, HTTP upgrade and the
WebSocket task. Native resolution never opens sockets, so its late completion
cannot dial after cancellation. The total dial budget is 15 seconds, with shorter
stage/caller deadlines. Native-root loading has one process-wide admission slot
and a ten-second caller wait, while an unfinished OS worker retains admission;
success caches one TLS configuration. Root count/DER size and returned address
count are checked after the native libraries return; their internal transient
allocations are not a whole-process memory bound. Configured private gateways
are allowed. No public custom-root or certificate-verification bypass is exposed.
Synthetic CA/loopback tests pass for both protocol selections, certificate
hostname/trust/expiry rejection before bearer transmission, cancellation, abort,
stalled handshakes and owner recreation. Independent review found no material
defect. Native-root startup initially failed inside the macOS sandbox; the same
opt-in test passed with authorized OS trust-store access. That check does not
connect to production. Integrated Home tests, actual race-Go v1 peer and lifetime
lock handoffs, and workspace Clippy passed. At that checkpoint the dialer connected
directly; the HTTP(S) proxy implementation is described below. Full lifecycle/
close-code logging and production CLI/service integration remain parity work.
Rejected HTTP upgrades retain only an optional
numeric status in 100–599; focused checks also reject a nonstandard status without
logging its value or response text.
The final integrated `make rust-check` at this checkpoint passed formatting,
all-target Clippy and 265 tests (12 explicitly opt-in checks). The separate
actual Go peer and Home lock handoffs passed; native macOS trust startup passed
with the access noted above. Linux/CI and real-device execution were not run.

The **candidate reconnect owner** validates the Home schema/role, absolute
inventory path, endpoint and bearer before acquiring the singleton. It performs
one verified attempt at a time and waits three seconds after a dial or peer
failure. A private owner retains the lock through peer cleanup and retry delays
even if its public waiter is aborted. Shutdown cancels and awaits the connected
peer; it never abandons its cleanup future or restarts provider work. Startup
trust failure is returned to the caller. OS blocking work retains its own
process-wide admission as described above; native process teardown still needs
the finite runtime policy. Lifecycle observation keeps only the latest fixed
category and returns copied values, so consumers cannot hold a watch read guard
across an await. This is a status feed, not a durable audit log.
Eight synthetic checks cover early validation, prepare/drop, unpolled run,
sequential retries, cancellation at each stage, delayed-cleanup lock retention,
startup failure and the current read-only peer over a duplex transport.
The final integrated Home suite passed 59 tests (three opt-in checks), plus
workspace Clippy. These tests inject the dial boundary; verified WSS and actual
Go interoperability are separately tested, not yet an end-to-end service run.
Independent frozen-scope review found no material connector defect; the caller
must supply the validated read-only catalog reader and keep Tokio alive through
awaited cleanup.
Authentication or TLS failure never causes downgrade or another request inside
one dial; a later loop iteration may retry the same verified endpoint.

The **candidate native terminal primitives** now include an async PTY owner and
disposable grouped tmux views. `pty-process` 0.5.3 (MIT) provides the safe public
PTY API; HMux keeps `unsafe_code = forbid`. There is no resident reader process
or blocking reader thread. Eight process-wide PTY permits follow both actual
direct-child reaping and all master descriptor halves. Cancellation kills only
the spawned attach client; original tmux/provider processes are never signaled
by this adapter. Synchronous descriptor setup and all current HMux command spawns
share a short gate because the portable dependency sets CLOEXEC after openpt.
Future native spawners must use the same gate. This is not a claim that arbitrary
third-party child spawning participates in it, or that OS spawn calls cannot block.

Views validate `{id, created_at}` before and after creation. A random owner nonce
is installed through `new-session -e HMUX_VIEW_OWNER=...`, in the same operation
that creates the session. Cleanup and the local detach hook require the exact
generated name and nonce. Later option/hook failure therefore still permits safe
cleanup, while duplicate-name creation cannot retag another session. The rolling
`hmux-app-view-*` name and `@hmux_app_view=1` catalog marker remain. A private owner
retains its eight-slot admission through setup cancellation and guarded cleanup;
cleanup has a separate bounded command pool and a fresh five-second deadline.
Already-removed views are an idempotent success. An unconfirmed cleanup returns an
error and quarantines its admission slot for the rest of the process lifetime,
without an extra retry task. A failed cleanup can still leave a view; repeated
failures exhaust admission rather than allow unlimited replacement creation.
Operational diagnostics/recovery for these failures are still integration work.

Nine PTY tests cover controlling TTY, Korean/binary I/O, resize, environment
scrubbing, cancellation/abort, normal exit and descriptor/reaper admission.
Six synthetic view tests cover failures, changed identity, immediate admission,
caller abort, cleanup ownership and quarantine. A separate descriptor test checks
that a concurrent managed command cannot inherit a transient non-CLOEXEC handle.
`make rust-home-terminal HMUX_TEST_TMUX="$(command -v tmux)"` passed both opt-in
checks on macOS with tmux 3.5a: partial creation/collision/foreign-owner cleanup,
and actual attach I/O/resize/close while preserving the original process and
another test-owned client. Every server/socket/session was disposable and isolated.
Independent frozen review found no remaining material defect after the atomic
ownership and admission-quarantine fixes. Older tmux support for `new-session -e`
has not been verified and remains a packaging/host-compatibility gate.
The final integrated `make rust-check` passed formatting, workspace all-target
Clippy and 290 tests, with 14 explicitly opt-in tests skipped. Both isolated tmux
checks were then run separately and passed on the same final source. Go handoff
and native-trust results above are from the preceding checkpoint, not rerun here.
The **connected terminal owner** now joins these primitives into the Home peer
for both JSON v1 and Protobuf v2 and advertises `terminal-output-flow-v1`. Open
responses precede output. Each view has 32 bounded input/control queue slots;
input bytes are copied to avoid retaining a larger decoded frame. Output uses
32 FIFO frame credits / 512 KiB plus one 16 KiB pending chunk, with a 40-second
credit-wait timeout that reports `output-stalled`. ACK handling never waits for
a blocked PTY writer. Queue overflow or a wrong ACK closes only its own view.
Terminal setup and refresh use a process-wide eight-command pool separate from
the catalog's runner; eight blocked opens therefore cannot starve the catalog
and tear down the shared link. That failure was reproduced before the fix and
the regression passed afterwards. Catalog/view constructors require an absolute,
bounded, NUL-free executable path; CLI PATH resolution remains CLI work.

Disconnect and public caller abort cancel the terminal jobs. Cooperative jobs
join both I/O futures (including an in-flight refresh command), direct-child
reaping and view cleanup before releasing peer ownership. Refresh independently
verifies the exact view nonce/name/marker and attached child PID, validates the
pane/TTY and foreground group using bounded `ps`, then rechecks tmux before
SIGWINCH and `refresh-client`. Its two-second total deadline, one-second input
debounce and fixed errors preserve terminal operation when refresh is unavailable.

Six synthetic peer tests cover both codecs, 640 KiB output, Korean/binary I/O,
resize, refresh failure, FIFO credit, input overflow, setup/active caller abort,
and saturated startup across a catalog interval. Five refresh tests use fake
process lookup and a signal seam; no real processes are signaled there. A paused
clock test covers the credit timeout. The actual Go race-enabled hub now verifies
the candidate's shell PTY I/O, ACKs, resize, refresh failure, close and subsequent
requests. A separate Rust Hub/Home test covers the same terminal path with both
codecs plus generation replacement. These process fixtures use synthetic tmux.
The opt-in real tmux test additionally passed actual foreground-group refresh
and original/other-client preservation on macOS. It first failed because the
sandbox blocked `/bin/ps`; the same isolated test passed with authorized process
query access. Linux, packaged WSS/service and browser/device acceptance are not
established by these checks.
Independent frozen review found catalog starvation from shared command admission
and late PTY failure for relative executable paths. Both were fixed and reviewed
again with no remaining material terminal-lifecycle finding.
The terminal integration checkpoint passed `make rust-check`: formatting,
workspace all-target Clippy and 303 tests, with 14 opt-in tests skipped. The
race-enabled actual Go peer handoff also passed on the final integrated source.
The separate real macOS tmux refresh result above is not a packaged or Linux test.

The separate **Home process candidate** assembles explicit config/token startup,
one-time tmux PATH resolution, singleton ownership and the WSS reconnect owner.
It requires `--experimental-home`, explicit absolute token/config files and the
WSS endpoint. Missing explicit configuration fails before state-lock creation;
the shared optional config decoder retains its existing fallback behavior.
The token reader is shared with the gateway and retains a 256-byte cap, private
regular/single-link checks and no symlink ancestors. Only canonical unpadded
32-byte Go-generated tokens are accepted: interior newlines and nonzero padding
bits are deliberately rejected, even though Go's decoder accepts those edits.
Token/config/endpoint values are omitted from startup errors and lifecycle
messages. SIGINT/SIGTERM handlers register before preparation, request shutdown
and await peer/process/view cleanup. PATH lookup accepts executable symlinks and
spaces/Unicode, rejects relative discovered commands, and is capped at 64 KiB /
256 entries. This candidate does not implement production CLI defaults, all
existing flags, private rotating lifecycle logs or service install/adoption.
Five focused token/startup tests passed. The separately built release candidate
also passed its opt-in process test with a synthetic CA, private config and fake
tmux: both WSS codec selections, reconnect, duplicate-process refusal, terminal
startup, SIGTERM with an active PTY, awaited cleanup and token-error redaction.
This is macOS loopback/synthetic process evidence, not an installed service,
real tmux WSS session, browser or Linux validation.
Independent startup review found no remaining material defect. Workspace
all-target Clippy and tests passed at this checkpoint: 308 passed, 15 opt-in
tests skipped. The separate release-process WSS test above was run explicitly
and passed; skipped tests are not counted as executed acceptance checks.

The Home dialer now snapshots `HTTPS_PROXY`/`https_proxy` and
`NO_PROXY`/`no_proxy` once per process. WSS follows Go's HTTPS selection, so
`HTTP_PROXY` and CGI `REQUEST_METHOD` do not select its route. HTTP and verified
HTTPS proxies use CONNECT, followed by separate TLS verification against the
original gateway hostname. Proxy Basic credentials appear only on CONNECT;
the Home bearer is sent only after the gateway certificate is verified. The
existing single outbound permit follows first-hop DNS and the complete nested
stream through cancellation and socket close. The total attempt budget remains
15 seconds, with five-second phase deadlines. Proxy policy is bounded to 2 KiB
for its URL and 4 KiB / 64 bypass entries. CONNECT responses are limited to
8 KiB / 32 headers; response text is never retained in diagnostics.

Synthetic HTTP/HTTPS tunnel tests cover separate credentials/TLS identities,
rejection, cancellation and permit lifetime. Focused tests exercise header
bounds, default/custom/IPv6 authorities and preserving bytes after CONNECT.
Independent review found overly broad IPv4 bypass for IPv6 CIDRs and domain
suffixes, plus silently ignored Unicode bypass rules. Before-fix tests reproduced
both broad bypasses. Network masking/family matching now follows Go's IPNet
semantics, and domain rules reject literal IPs. Unsupported Unicode bypass input
fails explicitly, with regression checks including selected non-UTF8 environment
values. The integrated Home library passes 54 tests, with two opt-in tests skipped;
workspace all-target Clippy passes.
After integration, `cargo test -p hmux-home --all-targets --locked` passed
105 tests with six opt-in tests skipped. The final release binary separately
passed `make rust-home-candidate-e2e` again. The earlier 308-test workspace result
precedes proxy integration; the changed Home package and all-target workspace
Clippy were rerun afterwards. No production service or existing user tmux session
was used, and no deployment/Go retirement occurred.

Remaining proxy compatibility gates: SOCKS5/SOCKS5H and Unicode IDNA proxy/bypass
names are unsupported; no redirects are followed. A malformed or unsupported
selected proxy never falls back to direct access; an explicit valid bypass still
selects direct access as in Go. Non-UTF8 selected proxy input, and non-UTF8/non-ASCII
or over-budget selected bypass input when a proxy is configured, fail closed.
Malformed individual ASCII bypass entries are ignored as in Go. Bypass ports
are interpreted as numeric ports, so noncanonical textual port spelling is not
promised to match Go. Strict CONNECT parsing and these bounds intentionally
reject some inputs tolerated by Go. This is candidate support, not complete
production proxy/service parity or a memory measurement.

Home browser-upload staging is now integrated into the experimental Rust peer.
`filestage::Store` uses an explicit private root, descriptor-relative no-symlink
operations, 0600 files/0700 directories and the Go-compatible `.lock` flock.
The lock spans quota admission, streamed file writes, manifest commit and delivery.
Limits remain 16 files, 32 MiB each, 128 MiB per request, 512 MiB/100 stages per spool,
with manifest space reserved before receiving data. SHA-256 is streamed; body data
is never accumulated as a whole request. Completion sets expiry to three hours
after commit; abandoned incoming directories retain the ten-minute cleanup rule.
Unknown or unsafe spool entries are preserved and fail closed.

The upload owner admits at most two process-wide workers. Each upload has one
queued 256 KiB input frame and one active disk operation; admission stays with
the blocking worker through actual stage cleanup. A dedicated two-slot command
runner verifies exact tmux `{id, created_at}` before staging and immediately
before commit, independent of catalog/terminal command pressure. Queue overflow,
size mismatch, cancellation and stale identity end that upload. Unknown/late
upload data/finish/cancel are ignored. Duplicate in-flight IDs close the candidate
peer rather than reusing an owner. The completion frame's bounded transport
receipt is awaited before accepting the stage, including cancellation during an
in-flight write. Failed/unaccepted stages roll back; cleanup remains best effort
if the filesystem itself fails or becomes unsafe.

The connector owns one startup/one-minute sweeper across reconnects, with a
30-second cooperative deadline and one process-wide blocking sweep slot. Shutdown
joins it and upload cleanup before releasing the Home singleton. Uninterruptible
OS filesystem calls may still delay cleanup; their slots are retained rather
than admitting replacement workers. `home_candidate --staging-root
/absolute/private/hmux/staged-files-v1` explicitly enables uploads. Omitting that
candidate-only flag leaves upload capability disabled and never opens the real
user cache. Production default cache selection and CLI/service wiring remain
later work.

Nine private-filesystem tests cover streaming/file boundaries and hashes,
completion expiry, quota reservations, incomplete/cancelled/undelivered cleanup,
flock cancellation and unsafe entries. Four peer tests cover both codecs,
cross-file binary chunks, session replacement, incomplete/excess/cancelled data,
late-frame isolation, admission/overflow under an occupied spool lock and
caller-abort/disconnect cleanup. Connector and startup tests cover sweeping
while dialing and explicit safe-root opt-in. The rebuilt release candidate passed
loopback WSS in both codecs with completed/partial uploads, reconnect, active PTY,
SIGTERM cleanup and retention of accepted files. The actual Go Gateway/hub built
with `-race` also passed against the Rust Home peer, including binary uploads
crossing file boundaries, Go response/hash/path validation, 0600 files and expiry.
The same test demonstrates Go-held flock blocking Rust staging, Rust-held flock
blocking Go, continued profile replies while the lock is occupied and release
after completion. Independent source reviews found no material defect in the
frozen async ownership/integration and filesystem engine; those reviews did not
run tests.

After upload integration, workspace all-target tests passed **337 tests**, with
**15 opt-in tests skipped**. Workspace all-target Clippy and formatting passed.
The rebuilt release-process WSS test and race-Go interoperability test above were
run separately and passed. This is not a rerun of all opt-in compatibility/native
checks or `make check`/browser-device suites. No production service, original user
session or credential was used; no deployment or Go removal occurred.

### Home session creation and metadata checkpoint (2026-09-24)

The candidate peer now handles create, alias and hidden operations over both
codecs when explicitly configured with a session context. The experimental
executable snapshots HOME/PATH/SHELL once and enables that context. Production
`hmux-web connect` and administration CLI parity remain separate work.

Creation retains the configured base, including an administrator-selected base
symlink, and creates a fresh 0700 child with descriptor-relative exclusive mkdir.
Existing children, files and links are never reused. Names retain Go's 80-character
input limit, 36-character folder slug, 16-character profile slug and 12-hex random
suffix, with at most 32 allocation attempts. A checked-in Unicode 15 category table
preserves letter/number/control behavior; the migration-only Go generator verifies
its provenance without adding a runtime dependency. Commands use absolute resolved
executables and literal argv, including empty arguments. Provider wrappers preserve
the exact signal-to-interactive-shell behavior. Creation reads the identity from
`new-session -P` output, never by a later name lookup. Cancellation, malformed
identity output or metadata failure never kills a started session or deletes its
workspace. Failed creates can therefore leave an unused folder, as in Go.

Metadata and visibility keep their separate Go v1 files and persistent flock
inodes (`sessions.lock` and `visibility.lock`). Reads cap files at 8 MiB and 2 MiB,
respectively; maps decode at most 10,000 entries and tags at most 64 per entry.
Alias writes use the existing 128-byte limit. Required maps, object shapes, unknown
fields, duplicate keys and invalid identities fail closed. Go's null optional
strings/tags remain readable; emitted fields retain Go omission behavior. The
private descriptor-based store additionally rejects symlinks, hard links and
unsafe modes rather than modifying their targets. Atomic private writes sync the
file and parent. Cancellation is checked before persistence; once it begins,
its actual result is returned rather than relabeling a committed write cancelled.

One process-wide action worker retains admission through filesystem work, bounded
tmux command execution and direct-child cleanup. Requests carry a 15-second
cooperative deadline; lock acquisition waits at most two seconds, and fresh
identity queries at most five seconds. Alias/hidden resolvers execute inside the
corresponding transaction lock, rejecting reused `{id, created_at}` values. A
separate single blocking slot applies catalog metadata without caching it.
Profiles remain independently responsive under create/lock pressure. OS filesystem
calls remain cooperatively cancellable; a stuck syscall retains its slot instead
of admitting an unbounded replacement worker.

Focused synthetic tests cover validation without workspace side effects, literal
argv, Unicode, eight concurrent allocations, occupied child links, stale metadata,
malformed-state preservation, lock/cancellation boundaries and both peer codecs.
An actual private macOS tmux server passed ten fake-provider scenarios: Codex and
Claude each exit 0/7/130, terminate on Ctrl+C, or handle Ctrl+C until explicit exit.
Each leaves a usable shell; all sessions survive peer closure until isolated test
cleanup. No original session, real provider process or credentials are used.
Independent source reviews covered action ownership, metadata storage and creation
planning. The postcommit cancellation finding above was corrected in main.

The integrated workspace run passed **355 tests**, with **16 opt-in tests skipped**.
Workspace all-target Clippy passed. The separately added Go-state oracle then
passed in main against the actual Go package built with `-race`; Home all-target
Clippy was rerun after adopting that test. This current-state Go→Rust→Go→Rust
handoff preserves sibling metadata, rejects stale alias/visibility writes, proves
both directions of flock exclusion (including inside the resolver), and retains
both lock inodes. The isolated actual tmux test above and rebuilt release-process
WSS test passed separately. The latter retains both codecs, reconnect, singleton
exclusion, upload/PTY behavior and SIGTERM cleanup; it does not test the complete
production CLI. Formatting, Unicode-table regeneration and whitespace checks pass.
No Linux/device/long-soak result, production deployment or memory improvement is
claimed by these checks.

### Home provider bindings and public conversations checkpoint (2026-09-24)

The Rust candidate now resolves Codex/Claude through fresh process ownership and
serves public conversations in both JSON v1 and Protobuf v2. One collector sends
the initial basic catalog before process enrichment. Process discovery uses two
shared blocking slots and a separate two-slot bounded command runner, never a
process per browser/tab. Startup resolves optional absolute ps/lsof paths without
executing probes; missing tools leave basic terminal/catalog behavior available.

Process tables cap raw input at 32 MiB, 200,000 rows and 100,000 nodes. Each tree
walk caps 10,000 nodes and a wrapper chain caps 16. Codex descriptor lookup batches
256 PIDs, retains at most 16,384 paths/4 MiB across owner and wrapper stages, and
requires the requested owner and every fallback wrapper to appear in lsof output.
A missing PID in partial output is unknown, not proof of an empty descriptor set.
No newest-file or sibling-provider guess is used. Claude uses exact PID registries
and transcript IDs under the default/swap roots. An existing rejected current
registry prevents fallback to a potentially stale swap. Safe descriptor-relative
opens reject symlinks and require a current-user-owned regular file, while keeping
provider-default 0644 records readable. Narrow metadata decoders skip unused JSON
subtrees instead of allocating full Value arrays/maps. These limits and stricter
failure rules are bounded candidate policy, not memory measurements.

A conversation rechecks `{id, created_at}`, pane PID, authoritative provider/file
association, device/inode and nonshrinking size after its bounded tail read.
Replacement, ambiguity or stale lifetime returns no transcript. Public filters
remove tool/private channels, injected instructions and confirmed compaction
handoffs. Complete possible compaction records that cannot be decoded make the
read unavailable rather than risking summary exposure. Escaped JSON strings and
Claude's canonical pre-filter hash preserve Go message IDs. Canonical JSON is
hashed incrementally; a deque retains at most 200 messages/512 KiB text, each
message at most 256 KiB, from a 4 MiB tail. The final encoded reply is limited to
2 MiB minus 4 KiB. Cancellation joins actual workers and query children before
releasing admission. Ordinary blocking filesystem calls remain cooperatively
cancellable between operations; an OS call itself cannot be forcibly interrupted.

Unchanged five-second polls are suppressed with a bounded streaming digest that
excludes generation time and retains no second encoded catalog. Changed state is
sent immediately after collection. Because both existing gateways require a full
catalog newer than 40 seconds, successful unchanged polls renew it after 15 seconds;
this is not a new catalog-free lease protocol. Peer shutdown now cancels/reaps an
in-flight tmux query without starting the next one.

Go-generated oracles cover 51 public-message/filter/ID cases and eight process
selection/wrapper cases. Synthetic two-codec peer tests cover Codex/Claude reads,
stale/pane changes, ambiguous and missing descriptors, inode replacement/shrink,
allowed append, encoded-size trimming, slow inspection, busy admission and joined
cancellation. Private provider data, original tmux sessions and production services
were not accessed. Source reviews identified and corrected malformed optional
registry handling, excessive JSON tree allocation, escaped-field/Claude-ID parity,
unknown descriptor fallback and catalog shutdown/idle behavior.

Final all-target results across the workspace suites are **390 passed, 17 opt-in
ignored**. The initial workspace run caught an obsolete unchanged-catalog test
assumption; the corrected test now proves a semantic update reaches the gateway
while eight terminal starts are blocked. Home all-target tests and the remaining
protocol/model suites then passed. Workspace all-target Clippy, formatting and
whitespace checks passed. The actual Go catalog/catalogstream packages passed
with `-race`. A rebuilt release Home executable separately passed both-codec WSS,
reconnect, active upload/PTY and SIGTERM cleanup. This does not rerun all opt-in
Go/native/OS/device gates or establish runtime memory savings. No production
replacement, migration commit/push or Go removal occurred.

Intentional stricter edge behavior remains: duplicate/cyclic process identities
fail closed; malformed process UTF-8/nonfinite CPU/oversized command rows are
skipped; bounded tree/descriptor/registry overflow is not guessed through.
Recognized duplicate JSON fields and invalid surrogate escapes can reject records
that Go accepts. Current rejected Claude registries and partial lsof uncertainty
also fail closed more strictly. These limitations require no alternate private
record search and must remain explicit during production acceptance.

### Home completion and host metrics checkpoint (2026-09-24)

The experimental Home now owns a best-effort Codex completion observer and one
asynchronous host sampler per connected stream. Production remains Go. Both
services are explicit library options and are enabled by the experimental Home
runtime; browser/PWA code and provider/tmux process lifetimes are unchanged.

Completion discovery receives only session identities/pane PIDs in one replaceable
pending slot, before catalog digest suppression. Fresh process/descriptor bindings
reuse the process-wide inspection admission and command runner. Completion scans
skip Claude registries, display metadata and model/activity tails. One tracker
owns at most 4,096 cursors and 4 MiB of identity/path/record-ID bytes. It reads at
most an 8 MiB file window in 32 KiB checkpoints with a 2 MiB line cap, retains
no transcript history, and emits at most 64 events per scan while advancing the
remaining state. This bounded best-effort policy can drop notifications under
load; it does not provide durable exactly-once delivery.

The first scan/reconnect only establishes a baseline. Unknown ownership,
record/inode replacement, shrink, a changed 256-byte SHA-256 anchor or an oversized
append gap clears/re-establishes it without historical replay. Only newly appended
newline-terminated, armed task completions with valid nonzero timestamps are
sent. Escaped JSON fields, UTC RFC3339Nano timestamps and SHA-256 event IDs match
the real Go parser oracle (23 event inputs, three identity/offset hashes). IDs and
`{id, created_at}` are public; paths and provider record IDs remain private. Events
are published only after a successful full read and anchor. Observation has a
three-second deadline and a five-second cooldown after exceeding it. Optional
notification failures do not independently tear down terminals/catalogs; shutdown
cancels and joins the observer and all owned inspection children.

The host sampler keeps one latest observation and samples five seconds after the
previous sample completes. It has one process-wide blocking slot and three bounded
macOS command lanes with a three-second sample deadline. OS commands use literal
arguments and a cleared C-locale environment, with 64 KiB text, 256-byte total-RAM
and 2 MiB GPU XML output caps. All fields are rebuilt from that observation;
failed fields are omitted, and all unavailable fields clear the sample. The
first basic catalog does not wait for slow metric commands. Native disk sampling
uses safe rustix statfs bindings and the existing filesystem-allocation semantics.
Blocking filesystem calls remain cooperatively cancellable between operations.

Linux reads only the bounded first aggregate CPU line, waits 250 ms and reads it
again, excluding guest counters; it never accumulates every per-core `/proc/stat`
row. `/proc/meminfo` stays bounded at 64 KiB. macOS preserves the second `top`
sample and resident-memory formula, canonical GPU utilization maximum and activity
fallback. GPU parsing uses streaming `quick-xml` 0.38.3 with default features off,
32-level nesting, 64 KiB tokens and 128-byte captured fields. The standard Apple
plist declaration is recognized without fetching a DTD; only predefined XML
entities and numeric references are decoded. Other declarations/entities, invalid
or oversized XML and unavailable platform fields are safely omitted. No new
resident process, runtime daemon or browser decoder is added by these collectors.

Independent read-only Sol reviews covered completion and metrics. Review found
that reading all `/proc/stat` could hide CPU data on high-core hosts; the bounded
aggregate-line reader and regression now cover that case. Main review also added
standard XML character/entity handling without DTD expansion. Synthetic tests
include a 30-case actual-Go CPU/memory/GPU/disk oracle and cover latest-only
queues, stale baselines, partial records, caps, cancellation,
partial sample failure and both codecs' reconnect/no-replay behavior. Native
macOS/Linux accuracy, packaged cross-platform behavior, low-memory comparison,
long-run/device validation and all production replacement gates remain pending.
Integrated results across the workspace suites are **422 passed, 17 opt-in
ignored**: the successful core/gateway workspace prefix, final Home all-target
suite, and remaining protocol/model/generator suites. The first workspace run
exposed a pre-existing terminal-startup test race: its fake tmux removes a marker
before child cleanup/reaping releases admission. The corrected test waits for
protocol admission to recover while rejecting any error other than bounded busy;
Home all-target checks passed afterwards. This did not require a runtime change.
Workspace all-target Clippy (`-D warnings`), formatting and whitespace checks
passed. Go catalog/catalogstream/hostmetrics race tests and the integrated 30-case
metrics oracle passed. A rebuilt release Home executable separately passed both
codecs over verified loopback WSS, reconnect, uploads, PTY and SIGTERM cleanup.
That opt-in process test can run native host metric tools but is not an accuracy
or resource benchmark. Full `make check`, all other opt-in/native/OS/device/soak
checks and comparative memory measurements were not rerun. No migration commit,
push, production deployment or Go retirement occurred.

### Embedded usage parsing and gateway boundary checkpoint (2026-09-24)

`hmux-usage` is now a native library with bounded pure parsers for Claude/Codex
OAuth responses, codex-lb pool/key windows, derived Codex account exports and the
active `cswap list --json` path. It reuses existing serde/chrono dependencies and
adds no daemon or provider process. Home collection still runs in Go: credential
I/O, provider HTTP, command execution, activity scanning, refresh scheduling and
Home publication are not yet wired to these primitives.

Public serialization is an explicit allowlist. The Rust gateway now validates
usage before retaining it for `/api/state`, for both Protobuf v2 and JSON v1.
Decoded/encoded snapshots are capped at 1 MiB, 128 accounts and two nonrecursive
source children. Percentages/counters, source provenance, retry bounds, safe
labels and unique positive account numbers are checked. Codex email is empty;
only approved aliases are shown. Claude cswap emails remain supported. Provider
identity, raw extras, credentials and unused quota windows cannot pass this
boundary. Validation runs outside the Hub state lock; original validated bytes
remain covered by the existing shared retained-payload budget.

API responses are capped at 64 KiB; codex-lb quota at 1 MiB/128 limits and account
exports/cswap output at 8 MiB/128 rows. Typed streaming array visitors enforce
row limits without constructing arbitrary JSON value trees. Missing five-hour
windows stay unobserved, pool scope replaces per-key scope completely, and Codex
plan types use the existing allowlist. cswap decision statuses and original
measurement timestamps survive last-good fallback; its cache expires by both
successful-read and source-measurement age. Source projection helpers preserve
CLI/secondary separation and apply current activity after quota completion;
a future collector must call them under its publication ownership. They do not
prove concurrent runtime behavior by themselves. Existing web five-hour hiding
for any active account lacking that window remains unchanged.

A pure per-provider quota state now issues one active fetch ticket, caches for
60 seconds, retains same-account last-good quota for 600 seconds on transient
failure and honors positive Retry-After with a 300-second fallback/24-hour cap.
Account changes immediately invalidate prior quota, suspension and tickets;
delayed old-account results are discarded. Auth/contract errors are explicit
and cannot masquerade as fresh quota. Keys are opaque 32-byte identity digests,
with redacted Debug; the future credential owner must compute Go's existing
identity precedence. Cached objects contain OAuth quota only, no activity or
accounts. The I/O owner must finish every ticket on success, failure or cancel.
The state itself owns no worker/clock/credential reader. Its five behavioral
tests cover account rotation, cache/sticky/suspension boundaries and invalid
successes; the actual-Go state oracle covers selected status/sticky/retry results,
not a complete refresh-runtime equivalence test. An independent Sol review found
no material state defects.

Synthetic actual-Go fixtures cover 37 public payload cases, 34 OAuth responses,
five codex-lb cases, five account exports, twelve cswap command cases and four
source projections. Two independent Sol reviews covered parsers and boundary
integration. A follow-up review found an incorrect activity-source spelling in
the projection helper; it now uses the exact Go value `api+jsonl`, with real-Go
projection fixtures and both input-source regressions. Manually constructed
ambiguous active-account sets fail closed. Rust intentionally rejects duplicate
typed JSON fields, some malformed scalar/null inputs, unknown malformed status
tokens and epochs outside Chrono's representable range more strictly than Go.
Source attribution remains with the in-tree Token Terrier MIT license and is
recorded in the crate README and third-party notices.

Integrated automated evidence is **444 passed, 17 opt-in ignored**: the complete
workspace run passed 438 tests, and its 15-test usage suite was superseded by the
final 21-test usage suite after projection fixes and quota-state integration.
Final all-target workspace Clippy (`-D warnings`), formatting, JSON inventory and
whitespace checks passed. Related Go stream/usage/codex-lb/account/cswap race
suites passed, followed by updated stream/state race checks. A release gateway
build and the opt-in actual Go Home/Rust gateway process test passed; that test
now explicitly waits for both providers and their source snapshots, as well as
catalog/PTY/reconnect behavior. Its initial sandbox runs stopped before catalog
publication because macOS `sysctl kern.boottime` was denied; the same isolated
synthetic test passed with OS-read permission. No production failure or runtime
workaround was introduced. Real credentials/provider data and original tmux
sessions were not used. Full `make check`, comparative memory/latency,
native/provider/device/soak tests and Go retirement remain pending. No migration
commit, push or production deployment has been performed.

### Usage credentials, OAuth I/O and activity checkpoint (2026-09-24)

The candidate `hmux-usage` crate now parses read-only Claude/Codex credentials
without building arbitrary JSON trees. Its stored credential has only the access
token, optional account header and a precomputed SHA-256 identity digest using
Go's ID/email/token-tail precedence. Refresh/ID tokens and raw account email are
not retained or serialized; Debug and errors are redacted. Input is bounded at
4 MiB, tokens at 16 KiB and identity fields at 1 KiB. Ambiguous duplicate Claude
fields fail closed. These header/duplicate limits intentionally reject malformed
inputs more strictly than Go. Ten synthetic actual-Go credential fixtures verify
ordinary precedence, typing and digest identity.

`hmux-home::usage_credentials` is an authoritative read-only cache with fixed
provider slots. Each cache hit checks current file metadata, including inode and
nanosecond timestamps. Descriptor-relative reads reject symlinks at every path
component, hard links, wrong ownership, writable-by-others and special files;
this ancestor-link policy is stricter than Go's final-component check. Stable
before/held/after revisions and one retry handle atomic replacement. Missing,
unsafe or malformed current files invalidate cached credentials. Two admitted
blocking readers process-wide retain their permits until actual exit, including
when the caller cancels or reaches its five-second deadline. Native blocked I/O
cannot be forcibly interrupted and never initiates network requests.

`usage_http` reuses the existing Hyper/rustls trust and proxy stack and its
httpdate dependency. Fixed HTTPS provider endpoints send only the required
headers, with no redirect, credential refresh, compression decoder, HTTP/2 stack,
idle connection pool or detached HTTP driver. Two process-wide usage sockets are
independent of the persistent Gateway socket; DNS/proxy/TLS/HTTP retain their
admission until the owned work ends. Response headers and body are bounded; body
is streamed up to 64 KiB. The complete provider refresh has a 25-second deadline,
including read-only authentication recovery. Unsupported compressed responses
fail closed; requests do not advertise compression. Blocking DNS may outlive a
cancelled caller, retaining its slot until the OS resolver returns.

`usage_oauth::Owner` serializes one provider's quota refresh. A drop guard finishes
every issued ticket, including an abandoned caller; success, error, cache,
account invalidation and sticky fallback stay in the pure quota state. A 401/403
reloads credentials once and retries only if the effective token or account
header changed. Unlike the old token-only Go recovery check, a same-token Codex
account-header change also retries, after invalidating the old account cache.
No credential writes, OAuth refresh service or provider restart were added.

The pure `activity` module decodes only the selected provider's token fields,
leaving unknown and malformed scalar subtrees borrowed/skipped. It counts fresh
Claude input/output/cache-creation and Codex uncached input/output, with no
reasoning-token double count. Go's 60-second sliding window, 20-second EWMA,
hysteresis/dwell and local-day behavior are preserved for ordinary bounded
inputs. The caller supplies wall time and local calendar dates; the module does
not traverse files. It caps lines at 8 MiB, paths at 2 KiB, labels at 256 bytes,
window events at 4096 and daily sessions at 1024. Daily membership retains only
32-byte digests instead of session paths. Cap/saturation diagnostics expose
undercount and integer overflow rather than claiming complete totals. Seven
actual-Go parser cases and eight burn transitions plus explicit clock-boundary,
late/future/day/overflow/malformed-field tests are synthetic evidence.

Independent Sol reviews covered the credential/store/OAuth owner, activity
parser/tracker and HTTP/dial boundary. They found provider-irrelevant malformed
fields being decoded, and same-token account-header changes missing recovery;
both are fixed with regression tests and re-reviewed. A follow-up caught missing
fixture regeneration and an oversized synthetic digest-test key; the actual-Go
generator and test input were corrected, then the relevant checks passed.

Integrated automated evidence is **473 passed, 17 opt-in ignored**: the full
workspace/all-target run passed 471 tests, superseded for affected library code
by the final 147-test Home library and 36-test usage library results. The final
14 focused Home usage tests, workspace all-target Clippy (`-D warnings`), Rust
formatting, JSON inventory and whitespace checks passed. Go auth/JSONL/burn race
suites passed, with JSONL rerun after adding the two parity fixtures. Real
credentials, provider calls, original tmux sessions and production state were
not used. The synthetic HTTP and TLS/proxy layers are tested separately; an
end-to-end OAuth request through verified usage TLS/proxy remains a coverage
item, as do native provider/OS/device/soak and comparative memory/latency gates.

These candidate owners are not yet wired into the shared Home usage stream.
Remaining work includes bounded activity file discovery/offsets/local timezone,
cswap command and codex-lb/account-file owners, shared source refresh scheduling,
publication-time activity merge, and lifecycle/reconnect integration. The new
limits and digest storage are implementation properties, not a measured whole-
process memory improvement. No migration commit, push, production deployment or
Go retirement occurred; the full Rust migration goal remains active.

Next: remaining Home operations, usage, recovery/hooks, production
CLI/services and release gates. Do not enable workspace
reconciliation until recovery lineage exists in the catalog. Production Home upload handling remains Go until
the coordinated verified runtime replacement.
The push owner now reuses the existing Hub admission, queue and transport. Its
final checks authorize a current snapshot; they do not atomically serialize a
remote Home workspace mutation with bytes already being sent to a push provider.
Packaged cross-platform and real-browser push acceptance remain release gates.

Keep the transport as one shared owner and join its send futures after requesting
shutdown; cancellation does not synchronously close sockets until they are polled
or dropped. DNS workers can remain occupied after shutdown and never dial later.
The process runtime's final wait must follow application-owned persistence/socket
and child-process cleanup. Native resolver/trust behavior still needs packaged
macOS/Linux acceptance. These services are enabled in the experimental full
gateway only, not the auth-only example or production. No memory improvement is claimed.

The auth-only executable is built separately with `make rust-gateway-candidate`;
`python3 tests/rust_auth_candidate.py` creates synthetic credentials and starts and
stops three isolated processes. Do not use it as a production replacement: static
assets, Home `/connect`, terminal, uploads, usage, diagnostics and push are unavailable.
`Gateway::with_home` enables candidate library Home, terminal, uploads, state and forwarded
actions; `Gateway::with_preferences` enables account usage settings and
`Gateway::with_workspaces` adds private account workspace persistence.
`Gateway::with_assets` enables bounded file streaming from the configured public
asset directory; `Gateway::with_diagnostics` adds the bounded account diagnostic
store and report/ingestion routes; `Gateway::with_push` adds the private push
owner, browser routes and completion delivery worker. The auth-only example deliberately does not enable these services. HTTP upgrade admission remains held
through cooperative socket-task cleanup, with a five-second shutdown bound.
Its HTTP policy currently caps connections at 64, headers at 8 KiB and JSON requests
at 16 KiB; JSON response encoding is capped at 6 MiB + 8 KiB per response (the
maximum combined catalog/usage state plus wrapper/settings space) and 8 MiB of
retained backing capacity across all concurrent JSON replies. Credits stay with
the actual `Bytes` owner through HTTP body handoff and slices, and return only
after its last reference is freed. Exhaustion returns 503 without queuing encoders.
Request handling
is bounded to 30 seconds and header/body reads to 5/15 seconds. The candidate's
keep-alive/header deadline is 5 seconds rather than Go's separate 60-second idle
timeout; full HTTP lifetime/resource parity remains a release gate. Response
budgets exclude route DTOs, allocator overhead and transient allocation growth;
full-process RSS/PSS still requires measurement. These are safety limits, not
measured memory claims.

Home heartbeat processing can time out if a consumer stops draining the bounded
inbound handoff; it does not grow a control-frame queue around backpressure.
Duplicate candidate Home sockets receive the Go-compatible 1008 close frame.
Best-effort close writes have a two-second total budget, including queue time,
followed by unconditional teardown and joining the transport task. Browser
messages and writer payload admission are bounded at 64 KiB per connection;
terminal input remains 32 KiB. The separate browser control decoder ignores
additive fields, retains exact session identity and streams capability entries
without collecting their array. Duplicate recognized fields still fail closed;
unused fields are skipped. Oversized WebSocket frames may terminate without an
explicit 1009 close. Remaining codec/close-code differences and operational
logging remain release work. The command runner's cleanup requires a running Tokio
runtime; Home shutdown must join owned work before stopping that runtime.


---

## Native macOS checkpoint before Linux acceptance

Preserved from the current control on 2026-09-24. The results and limitations
below describe that checkpoint; later status belongs to the current control.

## Block 2 result and Block 3 handoff

- Shared workspace and provider actions are connected through runtime → connector
  → peer for JSON v1 and Protobuf v2. Provider writes/jobs use bounded native work;
  successful setup coalesces OAuth/LB/cswap refresh and invalidates old account data.
- Sanitized workflow hooks/reports, Go-compatible private state, retention,
  exact-lifetime catalog overlay, binding resolver and streamed text views are
  implemented. Actual Go/Rust current-state and concurrent-helper checks pass.
- Recovery completes before initial catalog publication. Transactions retain the
  recovery lock and worker admission through cancellation and actual child exit;
  connector shutdown joins owners before releasing its singleton.
- Independent review fixes cover atomic tmux identity-check/termination,
  exclusive provider launch gates, and restricted-service-PATH executable lookup.
  Main integration also corrected macOS boot-time parsing. Restored providers
  return to a shell on exit; only exact restored lifetimes rebase shared tabs.
- Block 3 integrates the native commands, service lifecycle and installer. Keep
  macOS/Linux user services opt-in and retain current rollback. The remaining
  executable/OS gates are listed below; library APIs alone do not satisfy them.
- Keep one writer per checkout, synthetic tools/state and disposable sockets.
  Run each connected slice's checks; retain unchanged gateway/auth evidence.

## Block 3 integration status

- Native `hmux-web init/serve/connect` and `hmux-agent` entrypoints are integrated.
  Enrollment has synthetic TOTP/private-file, no-clobber and SIGTERM echo-restoration
  checks. Native `serve` passes the actual Go Home integration; native `connect`
  passes the isolated WSS flow for JSON v1 and Protobuf v2, reconnect, upload, PTY
  and signal cleanup. macOS boot-time reads require the documented test allowance.
- Gateway transport observations and shared private rotating logs are integrated:
  fixed diagnostic fields, 64 queued records, nonblocking overflow accounting and
  joined shutdown. Log/queue and request-cancellation/redaction checks pass.
- Helper contract corrections are integrated: inventory override reaches create,
  fresh workspace state is initialized, existing inventory is validated, setup
  publication syncs the parent directory, and macOS TTY input uses native readiness.
  Eleven agent CLI tests, 201 Home library tests (2 opt-in tests ignored), strict
  agent/Home Clippy and two actual Go/Rust helper handoff tests pass.
- Native launchd/systemd lifecycle, exact connector adoption, bounded manager
  commands and cancellation-safe publication are integrated. Twenty-three service
  checks pass (22 suite cases plus the owned-child adoption signal check). Native
  macOS checks read the test and its owned Rust child, preserving
  empty/spaced argv, process birth and allowlisted environment. A small bounded OS
  adapter fixes the generic sysctl wrapper's PID-query failure. Independent source
  review covered lifecycle/install and the adapter; Linux signaling now pins a pidfd.
  The documented macOS positive-PID signaling window remains as in Go. Linux code
  has not run on Linux; simulated managers do not establish actual registration,
  reboot/login or supported-OS acceptance.
- Installer preparation/recovery journals before staging, bounds I/O/lock waits
  and retains one backup per executable. Fourteen integrated library tests and
  two native installer CLI tests pass. The native command runs setup-home and
  optionally installs the service; a thin Python compatibility entrypoint only
  delegates to that command. Two-binary renames remain sequential, so additional
  transitional executable-pair checks are required.
- Six web CLI unit tests pass. The macOS arm64 release bundle now contains both
  native binaries, web assets, notices and a verified hash manifest. Its four
  enrollment PTY checks and native WSS flow pass (JSON v1/Protobuf v2, reconnect,
  uploads, PTY, private rotating logs and signal cleanup). WSS needs the existing
  macOS boot-time test allowance. Three actual bundle checks pass: complete hash
  coverage, native installation/repeated update/configuration preservation (including
  the Python wrapper), and binaries-only installation. Final targeted Clippy, fmt,
  ShellCheck, whitespace and document-link checks pass.
- Additional packaged Rust gateway + Go Home verification is **not accepted**:
  two automatic permission reviews timed out; the sandbox retry failed at Go Home
  recovery, consistent with the existing macOS boot-time restriction. The earlier
  native debug `serve` integration remains accepted; it is not release-artifact
  evidence. No running service, real credentials or existing tmux session was used.

Next: complete supported-OS lifecycle/install and transitional command gates,
including the remaining packaged gateway integration, before accepting all of
Block 3. The connected macOS executable slice is locally verified. Block 4 then
adds full-stack resource/stress/rollback evidence; do not repeat unchanged parser
checks. Production/build defaults remain Go.

## Latest accepted evidence

Block 2 Home validation: **274 tests passed with 8 opt-in tests ignored** across
all Home targets with two test threads. A subsequent boot-parser regression test
and the freshly built **debug** Home WSS test also pass: **276 distinct Home tests
executed, 7 opt-in Home cases still unexecuted in this block**. The initial parallel
run hit a pre-existing two-second Python-fixture timeout; its isolated recheck
and the bounded-concurrency Home run passed. Home Clippy denies warnings; fmt and
changed-document checks pass. This is functional evidence, not release packaging.

Actual Go `-race` checks exchange recovery files (including null/pending state),
verify two-way persistent-lock exclusion, compare workflow state/text output and
preserve concurrent helper writes. Separate actual tmux tests on disposable
sockets with fake Codex/Claude verify interrupted restore/retry, persisted mapping
before provider launch, resume argv/configuration, surviving shells and no duplicate
launch on same-boot sync. No existing provider/session state was used.

The WSS test verifies both codecs, reconnect, signal cleanup, private inputs,
uploads and PTY with recovery now enabled. The first sandboxed run could not read
macOS boot time; the isolated test passed with that read permitted after a retry
of an automatic-approval timeout. Physical reboot/login, Linux and device behavior
are not established by these checks. Prior gateway/model/protocol/usage partitions
remain accepted; they were not rerun for this Home-only block.

Synthetic 16 KiB terminal output encoded to 21,930 bytes with JSON v1 and 16,410
with Protobuf v2. This is a wire-size result, not a total memory/latency result.
Full `make check`, native installation, paired resource measurements, actual device
acceptance, long soaks, production activation and Go retirement remain incomplete.



## Native installation and lifecycle acceptance, 2026-09-24

- The native Go/Rust matrix was extended with one full unacknowledged browser
  credit window, a healthy second browser/control request, and lossless resumption.
  All five lanes passed on macOS and Linux; the macOS Go Home retains the earlier
  explicit-CA core-subprocess limitation and Linux oracle lacks Go race instrumentation.
- Rust Protobuf/JSON each passed 100 native view cycles on both OSes, then 10,000
  cycles per codec on Linux. Original synthetic session identity and PTY cleanup
  were checked throughout. Resource checkpoints and bounds are in bench/hmux/README.md;
  no CPU/latency, real-provider, browser-device or long-soak acceptance is implied.
- Four installed web/helper states passed sequential upgrade/rollback on both OSes.
  These checks exercise installed web usage startup and helper config/workflow
  continuity; they do not claim a second network matrix. Native Rust-Go-Rust gateway
  authentication rollback separately preserved current logins and revocations.
- Focused independent review found service publication preceding old-owner shutdown.
  The integrated fix preflights setup/service inputs, disables existing autostart,
  verifies shutdown, publishes under the service lock and enables on success.
  Ambiguous publication failure stays disabled; binary/service publication remains
  sequential. Main additionally corrected the first patch's blind restart after
  recovery and the reboot-autostart exposure. The reviewer did not clear the final
  complete release.
- The integrated fix passed 27 service tests (two opt-in ignored), six web binary
  tests and strict Clippy on each OS. Fresh release bundles on both OSes passed
  four bundle checks, including failed setup/preflight preserving installed files.
  A direct Linux debug-directory check was rejected by trusted-parent validation;
  copying the same candidates to a private fixture root passed without weakening
  runtime permission checks. The macOS native matrix required its authorized
  out-of-sandbox synthetic rerun for OS process inspection.
- All 92 contract/test-owner rows remain in tests/fixtures/contracts.json. Its old
  status/verification fields were preserved unchanged in the separate dated JSON
  checkpoint archive. Current functional gaps and acceptance gates remain in the
  sole migration control; no commit, push, production activation or Go retirement
  was performed by this acceptance slice.


## Executable acceptance checkpoint before final parity, 2026-09-24

### Recorded state

1. **Completed Linux slice:** fixed native permission/device-number types and
   portable package hashing. Native CLI/install/service checks and strict Clippy
   pass; packaged executable pairs pass with actual WSS negotiation. Test roots
   use private modes, retaining production trust checks.
2. **Completed macOS packaged matrix:** the rebuilt release candidate passes all
   five role pairs with a race-enabled oracle. Go Home uses its actual connector
   core in a test subprocess with an explicit CA; native Go ignores the synthetic
   CA environment on macOS. Host trust is unchanged. This proves interoperability,
   not native Go CLI trust. Prior sandbox/approval failures remain historical.
3. **Close installation gates:** transitional installed helper combinations now
   pass on both OSes. Managed-service activation ordering is corrected and
   focused tests pass on both OSes. Run the full service CLI in an isolated
   user-service environment. Actual rendered
   launchd/systemd tests now pass with unique disposable `hmux-e2e-*` units; this
   does not prove reboot/login or justify touching the production service name.

The broad Go checks passed; `make check` stopped at an unwritable existing npm
cache. The remaining `make web-check` passed with an isolated temporary cache.
The first full Linux Rust run found five test fixtures hardcoding macOS temporary
paths (22 test failures); these fixtures now use the native canonical temporary
directory. Home library checks now pass on both OSes (201 passed, two opt-in
cases ignored). The affected/remaining Linux integration partitions and strict
workspace Clippy pass, and the model partition also passes (13 tests). All native
workspace crates were covered across the split runs; opt-in skips remain explicit.
Main reviewed the portability and fixture changes. A focused independent review
found publication before validation/stopping the old managed owner. The correction
runs setup/preflight first, disables autostart and verifies old-owner exit before
publication under the service lock, then enables/starts on success. Publication
failures retain the stopped/disabled state; recovery never blindly restarts an
uncertain binary. Both OSes pass 27 service tests (two opt-in ignored), six web
CLI tests, strict Clippy and failed-setup/preflight binary-preservation checks.
Main reviewed the integrated fix. Activation is still sequential; failure after
binary publication can leave new binaries installed with autostart disabled.
Full service CLI and physical reboot remain unverified; this is not independent
release clearance. Fresh macOS/Linux release bundles now include the correction
and pass all four manifest/install/update/failure-preservation checks. Earlier
transport/stress observations identify their pre-correction binary hashes;
the correction changes installation/service activation, not transport behavior.

The shared native matrix now models verified tmux detach hooks as well as explicit
view cleanup, and accepts the Go gateway's observed revocation EOF while retaining
Rust's policy-close assertion and requiring revoked HTTP access to return 401.
It drains bounded pre-close output and requires actual closure; a read timeout
cannot count as a successful disconnect.
The extended matrix now passes on both OSes with a withheld-credit browser beside
a healthy browser/control request, followed by lossless resumption. Native Rust
Protobuf/JSON lanes each passed 100 view open/input/close cycles on both OSes,
checking cleanup and recording gateway/Home resources separately. The Linux
10,000-cycle runs pass for both Protobuf and JSON (20,000 cycles total). Native
Rust → Go → Rust gateway rollback on both OSes preserves current login state and
revocations in both directions, with no restored state snapshot. This closes the
native auth-state handoff check, not full deployment/service rollback acceptance.
No running production service, credentials or existing tmux state is used.
Three paired quiet Linux runs per scenario measured Rust gateway PSS of
6.24–6.72 MiB versus default Go 11.94–13.88 MiB; see the scoped
[measurement report](../../bench/hmux/README.md). These are preliminary gateway-only
observations, not full-stack CPU/latency/stress/soak acceptance.

Two functional parity gaps from the archived checkpoints remain explicit gates:
session IP-location enrichment (`internal/webgateway/session_location.go`) has no
Rust owner yet, and Home's proxy policy still rejects SOCKS and non-ASCII IDNA
bypass rules. Complete them before declaring native runtime parity; archive cleanup
does not waive these contracts.

At this checkpoint, the next planned work was to proceed directly to the connected full-stack measurement,
stress/rollback and soak work in Block 4. Full runtime performance, physical device
acceptance, 24h/72h soaks, production activation and Go retirement remain unfinished.



## Location and Home proxy parity, 2026-09-24

- Gateway session lists now include optional location metadata. The fixed
  credential-free ipwho.is GET uses the existing native trust, public-destination
  validation, global two-slot DNS/socket admission and shutdown owner. It has an
  8192-byte response cap, no redirects and a two-second budget. The locator
  coalesces two addresses, caches at most 256 entries (24h success/1h failure),
  avoids caching cancellation/timeout/admission failures and labels private or
  reserved IPs locally. No new dependency, idle connection pool or resident task.
- A new actual-Go oracle preserves public/reserved address boundaries, including
  mapped IPv4. Session results remain account scoped and are revalidated after
  the await; logout during lookup aborts delivery. Synthetic TLS tests check
  endpoint/headers, certificates, redirects, response size and socket cleanup.
- Independent read-only review found that stalled public lookups could prevent
  later private-address labels. Main now classifies those locally before the
  outbound budget, and a mixed-address timeout regression passes on both OSes.
  Review found no other material security issue in this focused slice; it is not
  independent clearance for the entire release.
- Home now handles socks5/socks5h with remote target DNS, optional RFC1929 auth,
  default port 1080, bounded exact handshake reads and original target TLS checks.
  IDNA proxy/bypass normalization uses idna 1.1.0 with idna_adapter 1.1.0's Unicode
  Rust backend; ICU is absent from Cargo.lock. Main additionally normalized
  Unicode proxy authorities before DNS/TLS, retaining delimiter and size checks.
  IDNA-related notices/licenses are included. Dependency source size is not a
  resident-memory measurement; compiled bundle sizes are recorded separately.
- macOS gateway library: 97 passed, 5 opt-in ignored, plus the new authenticated
  location-route test (1 passed). Linux combined gateway library: 98 passed,
  5 ignored. The final review correction passed five focused location tests on
  both OSes. Linux integrated Home library: 205 passed, 2 ignored. macOS delegate
  Home suite: 204 passed, 2 ignored; main's integrated proxy/SOCKS partition adds
  Unicode-authority coverage and passes 14 tests. Gateway/Home strict all-target
  Clippy and fmt pass on both OSes. No real accounts, external provider requests,
  running services or original tmux resources were involved.

- Fresh final macOS/Linux release bundles each pass all four hash/install/update/
  failure-preservation checks. On-disk hmux-web size changed from 9,078,336 to
  9,426,720 bytes (macOS) and 10,335,128 to 10,707,864 bytes (Linux). The helper is
  unchanged on Linux and differs by 16 bytes on macOS. These include both location
  and proxy additions; they do not isolate IDNA or measure resident memory. Earlier
  stress records retain their earlier binary hashes; performance runs will identify
  the fresh artifacts. No production activation or Go retirement occurred.
- Main's final SOCKS review matched Go's bundled client and corrected mapped
  IPv4 literals to the IPv4 address type. Four focused SOCKS checks pass on both
  OSes (including IPv4, IPv6 and mapped targets); strict Home Clippy and rebuilt
  bundle manifests pass. IDNA backend transitive license texts were also retained.
  Installation logic is unchanged from the accepted four-check bundle suite.
- The connected measurement slice added an opt-in paired native runner. Main
  corrected pair-order alternation, preserved raw RTT samples, kept GC overrides
  confined to opted-in runs and recorded sampling overhead. A three-lane smoke
  passed, followed by five pairs per lane: 15,000 echoes with no runtime errors.
  Conditions, numeric results and limits are in ../../bench/hmux/README.md. Private
  raw logs retain original binary hashes; the later SOCKS-only correction is not
  silently included in those measured hashes. No tuned-Go, sustained CPU, device
  or soak acceptance is claimed.

## Sustained runner, soak harness and advisory checkpoint

- Extended native performance runs to 500,000 equal-size inputs per lane and
  120 seconds connected idle, bounded at ten million total inputs. Default and
  explicitly tuned Go are separate lanes; Rust JSON/Protobuf keep identical work.
  Raw RTT arrays are retained once in bounded files, with ten active checkpoints.
  Four-lane short smoke passed. The first sustained attempt failed during its
  first Go lane because the oracle did not read/respond to WebSocket pings during
  the 60-second idle phase. This is a harness failure, not accepted runtime
  evidence. A single joined reader with a two-frame queue now handles idle and
  active traffic; sustained results remain pending at this checkpoint.
- Added a 10-second-to-72-hour native synthetic soak runner with frozen binary
  copies, atomic progress and bounded logs/samples. Main corrected three
  independent-review findings: enforce check/transient cadence, reset the soak
  and final-lifecycle deadlines after preparation, and retain a live process-group
  guard until bounded TERM/KILL cleanup. The runner separately verifies frozen
  fixture executable paths have exited, including PTY clients with separate groups.
  macOS 12-second JSON/Protobuf smoke passed; the revised Protobuf smoke and actual
  SIGTERM interruption/owned-fixture cleanup passed. Initial sandboxed macOS
  attempts lacked boot-identity/process-inspection permission; native runs used
  authorized unsandboxed access to synthetic state. A 24h macOS Protobuf run has
  started; it has not passed yet. No 72h or physical-device acceptance is claimed.
- RustSec database `ef8244d224cb89be53491e0c55a96c1279d9fdf1` identified
  `RUSTSEC-2026-0194` and `RUSTSEC-2026-0195` in quick-xml 0.38.3. HMux's bounded
  GPU XML parser uses the plain Reader and does not iterate tag attributes or use
  NsReader; those affected APIs are not used by the current parser. The dependency
  was nevertheless updated to pinned 0.41.0. Cargo audit 0.22.2 then reported zero
  vulnerabilities and warnings for lock SHA256
  `917c532d1841a36419740715ffa70c30779f2acd36380c62097a9a1a74b414bb`.
  Audit 0.21.2 could not parse current CVSS 4 advisories; the newer tool succeeded
  against the fetched database. Upstream MIT license bytes are unchanged.
- After the dependency update: 16 related Home metric tests and strict all-target
  Home Clippy pass separately on macOS and Linux. Both native bundles rebuild and
  their manifests verify. This is advisory acceptance, not the complete
  dependency-license review, release readiness, deployment or Go retirement.

## Sustained measurements and native notices

- The corrected persistent-reader sustained runner completed 12 native Linux
  runs: three repetitions of default Go, tuned Go, Rust JSON and Rust Protobuf,
  each with 60 seconds idle and 250,000 echoes. All three million echoes passed.
  Raw RTT file hashes were verified and the measured source snapshot retained.
  The initial tuned-Go environment also affected its synthetic Go tool; those
  three lanes remain recorded but are excluded from isolated tuning claims.
  Clearing runtime overrides at tool entry passed a targeted Go-only smoke;
  a three-pair corrected comparison is running. The nine unaffected default-Go/
  Rust runs and observed noise are documented in ../../bench/hmux/README.md.
- Added cargo-deny license/source policy and an offline native notice collector.
  It follows the union of host/target normal/build dependency closures rooted in
  hmux-web/hmux-agent, excludes dev-only attribution, preserves upstream files
  and ring source copyright blocks, and copies Rust standard-library copyrights
  and license texts. Index and bundle manifests retain file sizes/hashes.
  License/source policy passes; the existing audit remains zero findings.
- The collector's three synthetic tests and native manifest/notice-integrity
  checks passed on both OSes: 158 macOS crates and 141 Linux crates, plus Rust's
  standard library. Linux's first offline metadata attempt failed because a
  release-only cache lacked dev-only lockfile packages; fetching the locked
  target graph recovered it. Independent Sol/high review confirmed the build
  needed that explicit prefetch, and found three omitted ring ARM assembly
  copyright headers using `@` comments. Main added target/host prefetch and `@`
  capture plus a synthetic regression assertion. Corrected macOS packaging,
  manifest/index checks and explicit coverage of all three headers passed;
  corrected Linux packaging waits for its performance run to finish.
- These package changes add no resident process or runtime dependency and do not
  change the frozen native executable hashes. The first-party source license is
  still an owner decision; third-party attribution does not grant one. GitHub CI
  policy/notice jobs are configured but have not run remotely. No migration
  commit, push, service activation or Go retirement has occurred.

- Final corrected Linux notice packaging also passed all three collector tests
  and both manifest/notice-integrity checks. The build fetches the locked target
  and (when different) host metadata graph before offline generation; the
  standalone notice-check target also prefetches its host graph.
- Native capacity acceptance now passes on macOS arm64 (race-enabled oracle)
  and Linux amd64 (cross-compiled non-race oracle), each in JSON and Protobuf.
  Eight admitted views retain echo/ACK after a ninth is rejected with 1013; no
  rejected PTY is created, one closed slot admits a replacement, and all
  disposable PTYs drain while the original synthetic identity survives. The
  runner requires the actual acceptance marker and all eight resource rows,
  preventing an older or skipped oracle from silently passing. Main added a
  final mutation-count check to catch delayed extra PTY creation. Scope and
  resource observations are in ../../bench/hmux/README.md.
- Main then found a second tuning confound: after clearing synthetic tool
  overrides, the Go oracle itself still inherited GC settings. The partial
  comparison was explicitly stopped and preserved as invalid for isolated
  tuning attribution. Custom HMUX_PERF_GO_* controls now forward settings only
  to native Go gateway/Home; inherited oracle/tool runtime settings are cleared.
  A fresh default/tuned smoke and Sol/high read-only isolation review passed.
  The corrected three-pair measurement is running; unaffected default-Go/Rust
  lanes were not repeated. No tuned-runtime performance conclusion is accepted
  until that separate run completes.

## Corrected tuning and activity workload results

- The driver-isolated Go comparison completed six native runs (three alternating
  default/tuned pairs), 250,000 echoes and 60 seconds idle each, with zero errors.
  All raw RTT hashes and reported percentiles were verified. The oracle and
  synthetic tools keep default GC settings; only gateway/Home receive the chosen
  overrides. These results and their measured tradeoff are recorded separately
  from the earlier unaffected Rust comparison in ../../bench/hmux/README.md.
- Added an opt-in release example using the production activity Reader with new
  synthetic Claude/Codex JSONL trees. On each OS, smoke plus three measurements at
  1,024 and 4,096 total files passed exact backfill totals, content-byte accounting,
  ten unchanged polls, appended bursts, inode replacement, no replay and explicit
  session-count caps. The first Linux smoke correctly rejected group-writable
  fixtures from inherited umask 0002; explicit mode 0600 fixed the fixture.
  Corrected Linux runs and macOS smoke pass; strict example Clippy passes on
  both. Original Mac samples retain their earlier fixture-only artifact hash.
  Actual provider files and runtime reader behavior were not changed. This is
  reader-library timing, not packaged Home CPU/memory or a matched OS comparison.
- Linux 12-second JSON and Protobuf soak smokes each completed final reconnect,
  revocation, shutdown and frozen-process cleanup. The native Linux 24h Protobuf
  candidate run then started against frozen executables, after performance work
  finished; source snapshots and runner identity are retained privately. The
  macOS 24h process/guard were independently confirmed live and continue. Neither
  24h run has passed yet. No 72h RC/device/service-CLI gate or deployment is implied.

## Documentation, standalone tooling and native collector slice

- Reduced the contributor entrypoint to the normal checks and links; moved detailed
  runnable Rust recipes into ../../tests/RUST.md. Replaced stale protocol progress
  prose with durable contracts and preserved the original in
  RUST_PROTOCOL_CHECKPOINTS_2026-09-24.md. Changed relative links, default archive
  exclusion and whitespace were verified. Current execution remains owned solely
  by ../RUST_MIGRATION.md.
- Shell formatting now installs pinned shfmt 3.13.1 release assets and verifies
  SHA-256, removing its Go toolchain requirement. Bootstrap passed on both OSes;
  cached reuse, formatting and ShellCheck passed locally. Existing build-script
  formatting was normalized without changing behavior. CI now requests the same
  formatting gate, but GitHub CI has not been run.
- Actual native Home activity collection plus terminal input passed on both OSes
  and wire codecs: exact backfill/append/replacement counts, quiet non-replay and
  bounded raw RTT/resource checkpoints. Main added CPU sampling, raw percentile
  verification and signal cleanup; independent Sol/high review found no material
  scope blocker. Linux outliers led to a missing outbound TCP_NODELAY fix. Both
  native release rebuilds passed the same workload and 21 focused dial tests per
  OS; one opt-in native trust check per OS was skipped. Scoped samples and their
  noise/claim limits are retained in ../../bench/hmux/README.md. This is neither
  a new distribution bundle nor production activation.
- Read-only retirement audit identified still-JSON Protobuf control bodies as
  real remaining implementation work. The live control now explicitly requires
  typed schemas/internal owners/v1 adapters before final RC wire acceptance. It
  no longer describes all remaining work as release validation only. Original
  frozen 24h candidate soaks continue; neither is complete and they cannot stand
  in for a final RC containing later changes.


## Pre-release control checkpoint — 2026-09-24

Immutable copy before the second documentation cleanup. All running/pending
labels below are observations from that checkpoint, not live instructions.
The current queue and refreshed soak status are in [Rust migration](../RUST_MIGRATION.md).

### Rust migration — current work

Updated 2026-09-24. This is the **only current control for the Rust migration**.
Production remains Go. Rust code is an uncommitted candidate in the working tree;
no migration push, production activation or Go retirement has occurred.

#### Objective and completion

**Low memory, web terminal for ai agents.** Replace all HMux native runtime code
with Rust: Gateway, Home, embedded usage, administration/workflow helpers,
services and installation. Keep TypeScript/xterm.js, tmux and provider CLIs.
Use Protobuf v2 over the existing Home WebSocket with rolling JSON v1 compatibility.
Do not add a sidecar, gRPC service, browser decoder or mandatory Docker runtime.

Completion requires all supported commands/contracts, native packaging and
acceptance, measured resource/regression gates, current-state rollback, and then
removal of active Go sources/build dependencies. Passing library tests alone does
not complete this goal. Preserve every gate in [Rust contracts](../RUST_CONTRACTS.md).

#### Current state

| Area | Candidate implementation | Remaining before complete replacement |
| --- | --- | --- |
| Gateway | HTTP/static assets, accounts/login/TOTP/revocation, hub/terminal flow, uploads, push, diagnostics and state owners; synthetic and mixed Go Home integration evidence | Native pairs pass on both OSes; remaining capacity/resource, service rollback, device and soak gates |
| Home | WSS/reconnect/singleton, catalog/provider bindings, conversations, completion, metrics, PTY/views, session creation/file staging, shared workspace, provider setup and workflow owners | Packaged pairs pass on macOS/Linux; service, resource, device and soak acceptance remain |
| Wire | Protobuf v2 envelope, typed terminal/file bytes, catalog/usage and all 12 action/reply contracts; contextual JSON v1 adaptation and shared backpressure | Typed control slice accepted on both OSes; resource, service, device and final RC gates remain |
| Usage | OAuth, codex-lb, cswap/account exports, bounded activity scanner and shared Home publication are integrated | Shared packaged runtime and setup refresh tested; collector workload/resource/soak gates remain |
| CLI/services/install | Native web/helper commands, service lifecycle and native installer integrated; Go remains the production default | Bundles, installed transitional pairs and rendered user-service lifecycle pass on both OSes; full service CLI and release gates remain |
| Resources/release | Codec/quiet samples, 20,000 Linux view cycles, native capacity on both OSes, sustained/default/tuned-Go Linux comparisons and activity-reader samples | Remaining concurrent/whole-Home workloads, devices/soaks and service rollback; scoped measurements are not whole-product acceptance |

Gateway/Home library implementation does not mean every endpoint action or native
command works in the assembled product. The detailed command/route/state inventory
is [contracts.json](../../tests/fixtures/contracts.json); fixtures describe tested
behavior, not a second execution queue. All listed contracts remain in scope;
old per-row status notes are preserved as dated checkpoints in the archive.

#### Execution blocks

Work in the following order. Finish a connected, usable slice before opening the
next block. Delegate independent owners in isolated copies; main owns integration.

| Block | Deliverable | Exit check |
| --- | --- | --- |
| **1 — Shared Home usage (accepted locally)** | OAuth + codex-lb + cswap + account exports + bounded JSONL activity; one shared collector, source preferences, publication and reconnect/shutdown ownership | Synthetic Rust Home → gateway → state flow for both codecs; correct sources/counts; no replay, stale activity rollback or worker growth on reconnect |
| **2 — Complete Home and helpers (accepted locally)** | Recovery/checkpoint/workspace continuity, provider setup jobs, workflow/hooks and remaining hmux-agent operation backends | Fake-provider reboot/restore and helper concurrency; exact identities, original tmux/provider survival and current-state compatibility |
| **3 — Executables and installation (implemented; service acceptance pending)** | Complete hmux-web init/serve/connect/service and hmux-agent CLI; launchd/systemd; simple native installation/update with host workspaces/CLIs | Native macOS/Linux command/service/install tests; paths/PATH, process adoption, partial-install recovery and both-binary packaging |
| **4 — Release rehearsal (protocol accepted locally)** | Typed control schemas/adapters, full-stack compatibility, resource measurements, stress, device/OS acceptance, 24h/72h soak and current-state rollback | Complete typed-control semantics and required gates in Rust contracts; unavailable mandatory gates do not count as passed |
| **5 — Rust default and retirement** | Coordinated verified role replacement, Rust-only build/check/docs, removal of active Go sources and vendored runtime | Verified release hashes/services, retained Go rollback artifacts and clean Rust-only build; no Go runtime/build dependency |

Blocks are delivery units, not substitutes for the phase/contract gates. A changed
security/concurrency boundary still requires focused verification. Run integration
and independent review after the relevant owners are connected; avoid repeatedly
rechecking unchanged parsers or rerunning the complete workspace for each small edit.
Record each block's result here in place, rather than appending a new work diary.

#### Accepted evidence and limits

| Slice | Accepted evidence | Not established by it |
| --- | --- | --- |
| Home/continuity | 276 distinct Home tests in Block 2; current-state Go/Rust recovery/workflow and lock handoff; isolated tmux/fake-provider recovery; both wire codecs | Physical reboot/login, device behavior, complete resource/soak gates |
| Native helpers | 11 agent CLI checks; 201 Home library checks; two actual Go/Rust helper handoffs; all four installed web/helper pair states across sequential upgrade/rollback pass on both OSes | Installed-pair checks cover web CLI startup and helper config/workflow continuity; network/service behavior has separate gates |
| Services/storage | Native Linux service/installer/CLI suite and metadata pass; real launchd/systemd temporary units pass argv/environment/cwd, restart and stop; prior lifecycle/OS-adapter independent review | Full production service CLI and login/reboot acceptance; macOS PID signal race remains documented |
| Native installer | 14 library and two CLI checks; bounded paired journals/backups and installed helper setup | Two-file activation is sequential, not pairwise atomic |
| macOS arm64 bundle | Hash manifest and four install/update/failure-preservation checks; four enrollment PTY checks; packaged WSS in JSON v1/Protobuf v2 including reconnect, upload, PTY, logs and shutdown | All five role pairs now pass with race-enabled oracle; macOS Go Home uses actual connector core with isolated CA, not native Go CLI WSS trust |
| Linux amd64 bundle | Native build, complete hashes, four bundle checks, four enrollment checks, two Go/Rust helper checks; all four gateway/Home pairs plus Rust JSON fallback pass | Cross-compiled Go oracle has no race instrumentation; fake tmux tools do not prove real-device or long-running behavior |
| Location/proxy parity | Go IP-boundary oracle; verified synthetic HTTPS/account/logout tests; SOCKS remote DNS/auth/target-TLS tests; Unicode proxy/bypass cases; native macOS/Linux and strict Clippy pass | No live geolocation/proxy service was queried; no physical device acceptance |
| Static checks | Targeted strict Clippy on macOS/Linux, fmt, ShellCheck, Go tests/race/vet, web typecheck and 171 web tests pass | Opt-in cross-language/native cases retain their separate evidence; GitHub CI and deployment have not run |
| Typed transport integration | 57 protocol and 443 Home/Gateway checks pass on macOS (15 opt-in skips); 57 protocol, 22 hub and 47 focused Home checks pass on Linux (one opt-in skip); four native Go/Rust role/codec pairs pass per OS; independent review, strict Clippy and codegen/format checks complete | Whole-product performance-budget acceptance, service rollback, physical devices and final RC soak |
| Protocol measurement | A synthetic 16 KiB terminal payload is 21,930 bytes in JSON v1 and 16,410 in Protobuf v2 | Whole-runtime memory, CPU or latency improvement |

Detailed checkpoint counts, opt-in skips and failed/retried checks are preserved in
[historical evidence](../archive/RUST_MIGRATION_HISTORY_2026-09-24.md). They remain
scoped evidence; do not rerun unchanged parsers solely to rebuild a test count.

#### Current block and next actions

The native functional candidate is implemented. **Typed transport is now accepted
as a connected slice:** all 12 actions/replies, snapshots, terminal and file data.
Both OSes pass the native mixed-peer and codec checks; independent review found
no material issue in the final runtime changes. This closes the protocol work,
not the full migration or production release.

The remaining work is release acceptance, grouped into three deliverables:

1. **Resource and fault acceptance:** finish the pending concurrent/whole-Home
   workloads and evaluate measured budgets. Retain already accepted tests.
2. **Service and device acceptance:** run the fixed-name service/current-state
   rollback under isolated OS accounts, then actual Safari/iOS/Android and
   login/reboot checks. These environments have not been supplied or verified.
3. **Final release:** finish the existing 24h runs, freeze the RC for its 72h
   soak and release review, then coordinate cutover and Go retirement.

Do not restart earlier frozen soaks or claim they validate subsequent changes.
No new feature scope is opened while closing the migration.

| Connected slice | Current result | Next action |
| --- | --- | --- |
| Typed snapshots | Catalog/usage schema, allocation preflight, v1 adapters and Home→Gateway integration accepted; protocol and native mixed-peer checks pass on macOS/Linux. Review-found near-limit cache renewal disconnect fixed and regression-tested | Complete as a connected slice; HTTP caches retain bounded public JSON. Final RC must include this schema generation |
| Typed actions/replies | Accepted under `controls1`: all 12 operations, pending-context v1 replies and budgeted typed ownership; final native pairs, integrated checks and independent review pass | Complete; include final schema in the eventual RC and refreshed distribution bundles |
| Contributor tooling | shfmt uses pinned standalone release assets with verified hashes; macOS/Linux bootstrap and local formatting/ShellCheck pass | Default Go build/check and migration-only Go oracles remain until coordinated retirement; preserve golden fixtures and rollback artifacts |
| Native sustained measurement | Default-Go/Rust sustained comparison plus corrected three-pair Go tuning complete; all completed runs have zero echo errors; raw hashes/percentiles verified | Retain separate/confounded evidence; remaining concurrent workload and final budget acceptance |
| Dependency and package release | quick-xml pinned to 0.41.0; audit has zero vulnerabilities/warnings; license/source policy passes; fresh-cache and assembly-notice review corrections verified on both OSes | Accepted locally; CI remote execution and first-party license decision remain separate |
| Native capacity | Eight active views, ninth rejection, surviving inputs, slot reuse and cleanup pass on both OSes/codecs | Accepted locally; retain checkpoints and proceed to remaining concurrent/collector workloads |
| Activity and terminal workload | Reader checks pass at 1,024/4,096 files; actual native Home checks pass for both providers/codecs/OSes, with exact totals and resource/RTT samples; outbound TCP_NODELAY omission fixed and rechecked | Accepted scoped collector/terminal slice; short noisy samples are not final budget acceptance. Other concurrent/fault workloads remain |
| 24h candidate soak | Frozen Protobuf runs are active on macOS and Linux; both-codec native smoke passed on both OSes, interruption cleanup passed on macOS | Observe each existing run to completion without restarting it; neither has passed the elapsed-time gate yet |
| Service CLI / current-state rollback | Disposable launchd/systemd units and installed pairs pass on both OSes | Full fixed-name service CLI requires an isolated OS account; current accounts own production/performance services |
| Release acceptance | Not released; production is Go | Remaining workloads, physical devices, frozen 72h RC soak and independent release review precede cutover |

Results and claim limits are in [benchmarks](../../bench/hmux/README.md). The initial
sustained idle attempt failed because the harness did not read WebSocket pings;
that harness error was corrected before the completed measurements. Preserve
original hashes and confounded results rather than relabeling earlier evidence.

Full service CLI isolation requires a separate non-root account with a real
macOS GUI launchd session or Linux systemd user manager. Temporary HOME/XDG paths
do not isolate the fixed service name or real-UID connector scan. No such account
has been supplied or verified; do not run this test under the production UID.
Physical login/reboot is a separate unverified gate.

The macOS/Linux 24h soaks are **running, not passed**. The frozen release-candidate 72h soak has
not started. Safari/iOS/Android input/background/resume validation remains; do not
substitute synthetic tests or mark unavailable mandatory gates passed.

Complete the remaining fault/concurrent workloads and release review.
Block 5 follows verified acceptance: coordinate
role replacement, verify deployed hashes/services, retain Go rollback artifacts,
and retire active Go runtime/build sources. No production credentials or existing
tmux sessions are used by migration tests. Do not repeat unchanged suites solely
to reconstruct counts; detailed retries and limits stay in the dated evidence.

#### References and evidence

- [Rust contracts](../RUST_CONTRACTS.md): complete scope, source map, compatibility,
  locking, protocol, measurement and release gates. Read only the relevant section.
- [Fixture inventory](../../tests/fixtures/README.md) and
  [Rust verification](../../tests/RUST.md): test owners and runnable commands.
- [Historical Rust checkpoints](../archive/RUST_MIGRATION_HISTORY_2026-09-24.md): dated
  implementation/review/test details preserved from the former long plan.
- [Validation](../VALIDATION.md): maintained Go deployment and actual device evidence;
  it does not control the Rust work queue.

Private logs, scratch copies and raw runtime data remain outside tracked docs.
Do not copy their contents into this control. Update current state/next action here;
retain detailed immutable evidence with its owner or the archive.
