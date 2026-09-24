# Wire v1 synthetic compatibility corpus

`messages.json` is produced by the actual Go gateway decoder and Go JSON encoder
in `internal/webgateway/wire_compatibility_test.go`. It contains no runtime state.
`make rust-compat` verifies Go → Rust → Go against these fixtures. Semantic
operation validation and authentication are not performed by this framing codec.

Regenerate only after reviewing a contract change:

```sh
HMUX_REGENERATE_WIRE_FIXTURES=1 go test ./internal/webgateway -run TestWireV1CompatibilityCorpus -count=1
```

The initial Rust codec deliberately rejects a null envelope, noncanonical field
spelling and repeated typed fields that Go accepts. These deltas are recorded per
fixture; they are **not evidence of complete wire parity**. Existing HMux encoders
use canonical objects. Before release, each delta must be implemented compatibly
or accepted as a documented protocol hardening with all mixed-version tests.
Invalid UTF-8 and unpaired Unicode escapes also require explicit corpus coverage;
Go and Serde differ. No production endpoint uses this candidate codec yet.

The pure Rust output credit state machine enforces the existing 32-frame/512-KiB
window, exact FIFO ACK sizes and oldest-frame stall deadline. Async wakeups,
transport cancellation, connection generation and end-to-end rendering remain
integration work, not covered merely by these unit tests.
