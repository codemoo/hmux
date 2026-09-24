# Home WebSocket Protobuf v2

The Rust candidate uses [home.proto](hmux/v2/home.proto) over the existing
Home–gateway WebSocket. Production remains Go until verified cutover.
[Migration status](../docs/RUST_MIGRATION.md) owns acceptance and next work;
[historical protocol checkpoints](../docs/archive/RUST_PROTOCOL_CHECKPOINTS_2026-09-24.md)
preserve earlier implementation and test reports.

## Wire contract

- Offer/select `hmux-home.pb.v2.controls1` during the authenticated `/connect` upgrade.
  A validated HTTP 101 with no selection uses legacy JSON v1 on that same socket.
  Unknown selections, auth/TLS failures and malformed frames never downgrade.
- One `Envelope(version=2)` per binary message; legacy v1 uses text JSON.
  No gRPC, extra service/socket or browser decoder. Browser terminal bytes and
  browser HTTP JSON APIs keep their existing formats.
- The typed oneof identifies operation and direction. IDs belong to the current
  peer generation. Exact `{id, created_at}` identity, provider bindings and
  account authorization remain server responsibilities.
- Terminal/file bytes use Protobuf `bytes`, avoiding base64. Terminal output is
  limited to 16 KiB, input to 32 KiB; v1 FIFO ACK units/credit are preserved.
  Monotonic sequence ACKs require a separate schema/adapter change.
- Catalog and usage snapshots have typed fields, including nested workflows,
  source/account usage, optional values and list presence. Home keeps shared
  typed usage snapshots; Gateway converts them once into bounded browser JSON
  caches. The v1 adapter alone handles legacy snapshot JSON on the Home socket.
- Actions/replies use typed fields for the 12 allowlisted operations, terminal
  open/exit/refresh and attachment completion. Acceptance is recorded only in
  the migration control; this document defines the transport contract.
  JSON remains at browser/CLI presentation and the rolling v1 transport boundary.
- Legacy v1 action replies carry no operation. Gateway selects their decoder from
  the live generation's pending request (or terminal-open context); cancelled and
  unmatched late replies cannot select a decoder or complete a newer request.
- Control generation `controls1` reserves former request field 4 and response
  field 2 (`payload_json`). Snapshots keep typed envelope tags 35/36; retired
  snapshot tags 23/24 remain reserved. Older `hmux-home.pb.v2` or `snapshots1`
  offers receive no selection and use v1; selecting an obsolete token is rejected
  by the new Home. Never reuse an old token for incompatible field semantics.
- Bounded preflight checks field types, lengths/counts, nesting, singular/oneof
  duplicates and integer ranges before generated decoding allocates fields.
  Unknown tags currently fail closed; evolution needs versioned negotiation.
  Generated decoding alone does not authorize a message.

## Ownership and limits

One I/O owner shares a socket's read/write halves. Admission is bounded to 16
write reservations / 4 MiB, with a five-second deadline after writing starts.
Caller cancellation discards queued work; teardown drops both halves. Incoming
handoff holds at most one queued and one pending frame, each at most 4 MiB.
Socket read buffering is 4 KiB; application write buffering is disabled.
Decoded payloads retained by callers require separate accounting.

Snapshot preflight also admits a conservative 16 MiB decoded-tree estimate
(repeated element capacity plus owned text), before prost/serde materialization.
Catalog limits follow source owners: 10,000 sessions, 100,000 window names per
list, 64 tags, 1,024 workflows per list and 128 nodes per workflow, with the
aggregate budget applied across the tree. Usage is at most 1 MiB, 128 accounts
per snapshot and two source snapshots with no further children. Presence wrappers
preserve null/empty lists, and malformed optional host metrics retain existing
normalization behavior. Large snapshot variants are boxed so they do not inflate
every terminal envelope. These are admission limits, not measured peak RSS.

The owner sends a nonce Ping every 15 seconds and requires its matching Pong
within 10 seconds. Peer Pongs flush without application writes. A stalled
application consumer may fill inbound handoff and time out; there is no growing
queue around it. Failed admitted cleanup closes the peer. Gateway retained hub
payloads share an 8 MiB budget across reconnects and backing-buffer lifetimes.
Typed replies keep an immutable shared owner and charge owned strings, spare vector
capacity and boxed results until the last reply owner drops. HTTP rendering does
not turn internal dispatch back into JSON.
Long-lived decoded fields receive independent backing; no zero-copy or peak
memory claim follows from these limits.

Each Home view permits 32 input/control frames, 32 FIFO output credits / 512 KiB,
and one 16 KiB chunk awaiting credit. A 40-second credit wait ends that view.
PTY writes and credit waits do not block the shared reader. Setup/refresh uses an
eight-job pool separate from catalog and cleanup. Closing a view preserves the
original tmux/provider session; disconnect joins owned I/O and guarded cleanup.

Upload admission permits two blocking workers and one 256 KiB queued chunk per
worker. Cumulative ACKs, identity revalidation, private spool locks/manifests,
cancellation cleanup and three-hour expiry are shared by both codecs.
Browser terminal transport has a separate 64 KiB message/writer budget and
bounded close/HTTP cleanup; it does not negotiate the Home subprotocol.

## Schema maintenance

Generated Rust is checked in; native builds do not need protoc. Runtime uses
prost/bytes, while `tools/hmux-protocol-gen` uses prost-build 0.14.1 and protoc
35.1. Run `make rust-proto-check` for schema edits. It checks compiler version,
regenerates into a temporary directory and compares formatted output. CI installs
the official compiler archive using its pinned SHA-256 digest.

For manual inspection:

```sh
CARGO_HOME=/tmp/hmux-cargo cargo run --locked -p hmux-protocol-gen -- /tmp/hmux-proto-generated
rustfmt --edition 2021 /tmp/hmux-proto-generated/hmux.v2.rs
diff -u crates/hmux-protocol/src/generated/hmux.v2.rs /tmp/hmux-proto-generated/hmux.v2.rs
```

Reserve removed field numbers/names. Schema changes need fixture, direction and
parser/allocation-bound coverage plus mixed-version checks. Codegen equality and
smaller encoded payloads do not prove full compatibility, memory or latency gains.
See [Rust verification](../tests/RUST.md), [measurements](../bench/hmux/README.md),
[prost](https://github.com/tokio-rs/prost) and the
[encoding specification](https://protobuf.dev/programming-guides/encoding/).
