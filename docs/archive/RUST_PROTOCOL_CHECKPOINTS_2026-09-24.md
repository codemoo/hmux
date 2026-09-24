# Historical Rust protocol checkpoints — 2026-09-24

Preserved from the protocol reference during documentation cleanup. Pending and
incomplete labels below describe earlier checkpoints, not current status. Use
[the current migration control](../RUST_MIGRATION.md) and
[protocol contracts](../../proto/README.md) for current work.

# Home WebSocket Protobuf v2 (candidate)

User-selected target for the full Rust native migration. Schema:
[home.proto](../../proto/hmux/v2/home.proto). This is not active on any production socket.

- Offer/select `hmux-home.pb.v2` during the authenticated `/connect` upgrade.
  Only a successful 101 with no protocol selection uses legacy JSON v1. Unknown
  selection, auth/TLS errors and malformed frames do not downgrade.
- One `Envelope(version=2)` per binary WebSocket message. Existing v1 Home uses
  text JSON. No gRPC/extra socket/server and no new browser decoder dependency.
- Typed oneof discriminates operations and direction. Opaque IDs are scoped to
  the current peer generation. Browser FIFO ACK sizes and v1 credit limits stay;
  monotonically sequenced v2 ACKs would require a separate schema/adapter change.
- Terminal/file bytes use Protobuf bytes (Rust `Bytes`), avoiding base64. Some
  catalog/usage/action payloads explicitly remain bounded JSON bytes for migration.
  A typed outer envelope is not a claim that all control schemas are Protobuf.
- Terminal output is capped at 16 KiB to fit one FIFO ACK credit; terminal input
  retains its 32 KiB cap. Terminal exit preserves the bounded error category.
  Usage's provider tag must agree with its retained JSON snapshot.
- Before allocating generated message fields, a bounded preflight walks known
  field types, lengths/counts, nesting and singular/oneof duplication. Unknown tags
  currently fail closed; schema expansion needs negotiated/versioned policy.
  Integer range checks precede prost's narrowing casts.
  Generated prost decoding alone does not authorize a message.
- Receiver also needs stateful admission. The candidate hub now implements
  current-peer generation checks, pending/view/upload caps and FIFO ACK credit.
  Browser account authorization/revocation/expiry is integrated in the opt-in
  candidate terminal route. The candidate Home owner checks exact tmux session identity; provider binding
  and recovery remain incomplete. Codec tests alone cannot prove those contracts.
- `Bytes` may retain an entire WS allocation: queue accounting must track backing
  ownership, and long-lived small slices may need copying. No zero-copy memory
  saving is claimed without measurements.

Candidate implementation includes a v1 JSON adapter and a shared WebSocket I/O
task with separate read/write futures. Write admission is immediate and bounded
to 16 reservations / 4 MiB; once started, a frame has its own five-second write
deadline. Caller cancellation discards only queued work. The task owns both
socket halves and drops both on teardown even if the consumer is idle. Incoming
handoff retains at most one queued and one pending frame, each at most 4 MiB;
decoded payloads held by the caller are a separate budget. Socket read buffering
is 4 KiB and application write buffering is disabled. Submit copies into owned
storage to prevent a small slice retaining a larger allocation; this is bounded
ownership, not a claim of zero copies.

Synthetic duplex-socket tests cover cancellation, abort, queue/write deadlines,
teardown, wire types and the v1 adapter. Detached cleanup submissions use the same
bounds without spawning receipt waiters; an admitted cleanup that expires or is
cancelled closes the peer instead of silently leaving remote work behind.
The same I/O task sends a nonce Ping every 15 seconds and accepts only its matching
Pong within 10 seconds. Peer Pongs are flushed even when no application data is
written. A full inbound handoff can delay control reads and therefore time out;
there is no extra queue or task that grows around a stalled consumer.
The library's opt-in authenticated Home route negotiates both versions over real
HTTP/1 sockets and joins its hub/transport during shutdown. Duplicate Home peers
are rejected, and old-generation replies cannot reach a replacement. Retained hub
payloads (snapshots, replies, upload results and output) share an 8 MiB budget that
survives reconnects and follows the actual `Bytes` owner until its last slice is
dropped. Decoded fields are copied into independently owned backing before
long-term retention. This does not account for transient/parser allocations.
An isolated actual Go peer/upload worker and file-stage receiver accepts the
candidate's legacy v1 upload traffic. The full experimental gateway also passes
an actual Go `connectOnce` catalog/request/PTY integration harness, including
768 KiB output with FIFO ACKs, replacement and logout. Its tmux/process commands
are synthetic; the outer WSS/reconnect/singleton wrapper and real host acceptance
remain pending. The separate authentication-only executable still leaves
`/connect` unavailable.

The browser terminal route reuses this I/O owner with a 64 KiB message/writer
payload budget, binary terminal bytes and JSON controls. It does not negotiate
the Home Protobuf subprotocol. Best-effort close writes have a two-second budget
followed by local teardown; task joining fits inside HTTP's five-second cleanup.
Synthetic tests exercise account cancellation, expiry, output ACK, pending-open
abandonment and blocked-writer shutdown. This is not device validation or full
Go/Rust endpoint interoperability.

The Rust Home peer consumes an already authenticated transport and publishes
basic catalog/profiles and disposable terminal streams with either codec. It
advertises `terminal-output-flow-v1` and, with an explicit private spool,
`web-upload-v1`. Synthetic tests
cover cooperative/caller-abort cleanup, 640 KiB output, FIFO credit, blocked
input, cancellation and terminal-load isolation. The subprocess test also passes
against the real Go connector endpoint/hub with fake tmux commands and an owned
shell PTY (Korean I/O, resize, refresh failure and close). A bounded client upgrade offers v2 over an
already verified stream and accepts an absent selection only after validating
the HTTP/1.1 101, Upgrade/Connection and WebSocket accept headers. Unknown or
duplicate selections, extensions, auth failures and redirects are rejected
without retry. It drives Hyper inline with an 8 KiB/32-header ceiling and a
five-second or earlier caller deadline; cancellation/drop releases the stream,
and bytes buffered after the headers survive handoff. This avoids tungstenite
0.28's stricter no-selection rejection without disabling handshake validation.
The actual Go endpoint test verifies same-socket v1 fallback after the v2 offer.
The separate Home WSS dialer now verifies native-root TLS against the same host
used by DNS and HTTP, within a 15-second total attempt budget. A process-wide
outbound slot follows blocked DNS work and then the successful socket's lifetime;
native-root loading also retains one slot through caller cancellation/timeout.
Synthetic CA/loopback TLS tests cover both selections and rejection before bearer
transmission; native trust startup passed separately on macOS with OS trust-store
access. The actual Go peer test remains plaintext loopback. A candidate outer
owner now holds the Go-compatible singleton across sequential three-second
retries and awaited peer cleanup, including public caller abort. Its synthetic
tests inject the dial boundary. The separate opt-in `home_candidate` now also
passes an actual-process test with a synthetic CA and loopback WSS, both codecs,
reconnect, singleton exclusion and signal-driven active-PTY cleanup. It loads
explicit private token/config files and resolves tmux once from PATH or an
absolute override. HTTP/HTTPS proxy CONNECT is supported with bounded environment
selection, separate proxy/gateway TLS identities and fixed redacted errors.
SOCKS/IDNA and installed service acceptance remain incomplete; see the migration
document for explicit proxy compatibility limits. Production activation is pending.

Native Rust PTY and nonce-owned grouped views are integrated into this peer. Each
terminal has at most 32 queued input/control frames (input payloads are copied
into bounded owned storage), 32 outstanding FIFO output credits / 512 KiB, and
one 16 KiB chunk waiting for credit. A 40-second credit wait ends only that view.
PTY writers and output-credit waits never block the shared Home reader. Terminal
setup/refresh commands use a process-wide eight-job pool independent of catalog
collection and cleanup. Disconnect joins I/O, child reaping and guarded view
cleanup. An actual isolated macOS tmux check covers refresh and original/other
client survival; OS process-query access is needed outside restricted sandboxes.
Combined WSS/service, real browser and Linux acceptance remain pending; no memory
gain is claimed by these functional checks.

Home uploads are now an explicit candidate capability (`web-upload-v1` with a
configured private Store). The same owner handles both v1 and v2: two admitted
blocking workers, one 256 KiB queued chunk each, exact cumulative ACKs, pre/post
session identity verification, private Go-compatible spool lock and manifests,
and three-hour completion expiry. Caller abort/disconnect joins stage cleanup;
the connector owns the startup/minute sweeper across reconnects. Candidate process
tests use `--staging-root` with synthetic data, never a real user cache. Production
CLI/default-root/service acceptance and Linux/browser checks remain pending.

With an explicit session context, the candidate Home also handles create,
alias and hidden actions over both codecs. Their small bounded JSON control
payloads remain unchanged. Creation uses literal tmux argv and fresh subfolders;
metadata changes revalidate the live session under the corresponding Go-compatible
state lock. One process-wide action worker retains admission through cancellation
and direct-child cleanup; profile replies remain independent. This is candidate
behavior, not production CLI/service parity.

Generated Rust is checked in so native builds do not require protoc. The developer
codegen tool is `tools/hmux-protocol-gen`; runtime crates depend on prost/bytes,
not prost-build. Current generation uses prost-build 0.14.1 and protoc 35.1.
`make rust-proto-check` verifies that compiler version and regenerates to a
temporary directory before comparing the formatted result. The dedicated CI job
installs the official Linux protoc archive with its pinned SHA-256 digest; its
compiler is a build check dependency, never a runtime dependency.
To inspect changes manually before replacing generated code:

```sh
CARGO_HOME=/tmp/hmux-cargo cargo run --locked -p hmux-protocol-gen -- /tmp/hmux-proto-generated
rustfmt --edition 2021 /tmp/hmux-proto-generated/hmux.v2.rs
diff -u crates/hmux-protocol/src/generated/hmux.v2.rs /tmp/hmux-proto-generated/hmux.v2.rs
```

Reserve removed field numbers/names; never reuse them. Fixture/fuzz/direction and
allocation-bounds tests must accompany schema edits. Regeneration is a schema
consistency check, not evidence of wire compatibility or performance.

References checked for design on 2026-09-24:
[prost](https://github.com/tokio-rs/prost) documents bytes mapping, unknown enum
preservation and separate protoc codegen. `Cargo.lock` records resolved versions.
The formal format reference is [Protocol Buffers encoding](https://protobuf.dev/programming-guides/encoding/).
Performance claims require same-workload benchmarks; encoded-size tests alone do
not measure process memory, CPU or network/render latency.


## Accepted typed snapshot slice — 2026-09-24

This later checkpoint supersedes the earlier catalog/usage JSON-body status.
The current contract is [proto/README.md](../../proto/README.md); migration work
remains controlled by [RUST_MIGRATION.md](../RUST_MIGRATION.md).

- Typed catalog/workflow/metrics and usage/source/account fields use envelope
  tags 35/36 and explicit `hmux-home.pb.v2.snapshots1` negotiation. Retired tags
  23/24 are rejected. Old offers fall back to JSON v1 with no selection.
- Home retains shared typed usage; Gateway consumes typed snapshots and retains
  bounded public HTTP JSON caches. v1 decoding preflights catalog expansion;
  protobuf scanning bounds field types/counts/depth and a 16 MiB tree estimate.
  Optional/list presence, 64-bit integers and usage allowlists retain coverage.
- macOS: 49 protocol and 435 Home/Gateway checks passed, with 15 opt-in checks
  skipped. The final hub suite has 17 passing checks including the review fix.
  Linux: the same 49 protocol checks and final 17 hub checks passed.
- Four changed native pair cases passed per OS: Rust Gateway/Go Home,
  Go Gateway/Rust Home, Rust/Rust protobuf and Rust/Rust JSON fallback.
  The macOS oracle is race-enabled and Go Home uses its actual connector core
  with an isolated CA; Linux uses a cross-compiled, non-race Go oracle.
  These tests cover catalog/profiles/workspace, PTY flow/input/resize,
  close/reconnect/logout and gateway shutdown with synthetic host tools.
- The first macOS mixed-pair attempt stopped during synthetic Go recovery under
  the sandbox. The unchanged isolated harness passed outside that restriction;
  both results remain in private logs. No production tmux/state was used.
- Independent Sol/high review identified a near-4 MiB catalog renewal that
  transiently charged both cache generations against the 8 MiB retained budget.
  The regression failed before the fix and passed after releasing the replaceable
  cache reference first. External Bytes readers remain charged and cannot bypass
  the budget; the test checks both cases.
- Strict Clippy passed on both OSes; schema regeneration and formatting checks
  passed locally. The current codec smoke verified all round trips; its scoped
  wire-size result is recorded in [benchmarks](../../bench/hmux/README.md).
  These are native test builds; distribution bundles and production were not
  changed. Earlier 24h soaks retain their original frozen executables and do not
  validate this newer transport generation.

Final test-binary SHA-256 values:

| Platform | hmux-web |
| --- | --- |
| macOS arm64 | `3f999a808894aa55671d999e68e3dfd8eb2991e4f7941763155971acd54fe3c4` |
| Linux amd64 | `308b87f90612fa4b1f384e11f481ca900434123ce5ecf283386f32f4196cfe8c` |

Remaining action/reply JSON is not full typed-control completion. Resource-budget,
service-account, device and final soak/release gates remain separately required.


## Accepted typed action/reply slice — 2026-09-24

This checkpoint supersedes the remaining action/reply JSON status above.

- All 12 operations use typed request/result oneofs internally. JSON v1 replies
  are interpreted only against the live generation's pending request context;
  cancelled/unmatched replies are discarded. Negotiation is now explicitly
  `hmux-home.pb.v2.controls1`; removed request/response JSON field numbers and
  names remain reserved. Legacy HTTP/CLI/transport adapters retain JSON contracts.
- Home dispatch, workspace, providers, conversations, terminal replies and staged
  uploads are connected to those types. Gateway reply owners retain their charge
  against the 8 MiB shared budget until the final external clone drops, including
  across reconnects. Semantic protobuf validation does not generate JSON; HTTP,
  CLI and legacy serialization still allocate bounded JSON at their boundaries.
- macOS: 57 protocol and 443 Home/Gateway checks passed, with 15 opt-in skips.
  The initial combined run found one stale negotiation expectation; the corrected
  test passed in a focused rerun, and the remaining Home targets passed. Counts
  combine distinct checks across these runs, not repeated successes.
- Linux: 57 protocol, 22 hub and 47 focused Home checks passed, with one isolated
  real-tmux opt-in skip. A conversation fixture initially inherited umask 0002 and
  produced group-writable config rejected by the runtime. The fixture now sets
  explicit private modes; its failing check passed under the same umask. The
  runtime's configuration safety check was preserved. Failed logs are retained.
- Four native role/codec cases passed per OS: Rust Gateway/Go Home,
  Go Gateway/Rust Home, Rust/Rust protobuf and Rust/Rust JSON. The first Linux
  run exposed a default empty HTTP session encoded as a present protobuf session;
  normalizing it to absent fixed sessionless actions. An authenticated HTTP
  regression now covers the case. Final pairs include that fix and Home's
  streaming profile response-size guard. macOS uses a race-enabled oracle;
  Linux's cross-compiled oracle is not race instrumentation.
- Independent Sol/high review of the frozen runtime plus the two supplemental
  fixes reported no material findings. Strict all-target Clippy passed on both
  OSes; schema regeneration, local formatting, whitespace and documentation-link
  checks passed. A final test-only permissions correction was separately rechecked.
- Distribution bundles and production were not activated. Earlier soaks remain
  tied to their original binaries. Whole-product resource, service, device and
  final RC acceptance are still governed by the current migration control.

Final native test-binary SHA-256 values:

| Platform | hmux-web |
| --- | --- |
| macOS arm64 | `c5c771a40e35d14a2f606205bb1331e2ef5924f0c9da6d5c3aeda773096a412a` |
| Linux amd64 | `7d355330907bed6501199544480227f2502f16252132f310971bb281ea7a364c` |
