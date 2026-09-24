# Wire v1 synthetic compatibility corpus

`messages.json` is frozen output of the prior Go gateway decoder and encoder,
preserved at checkpoint `c061f28fe7ea8e865578ac1189240447d0ebaa6f` in
`internal/webgateway/wire_compatibility_test.go`. Rust checks it without a Go
executable. Semantic operation validation and authentication belong to higher
layers. Regenerating this baseline requires a separate historical checkout and
review; do not rewrite it from the new implementation under test.

The Rust compatibility decoder deliberately rejects a null envelope, noncanonical field
spelling and repeated typed fields that Go accepts. These deltas are recorded per
fixture; they are **not evidence of complete wire parity**. Existing HMux encoders
use canonical objects. Before release, each delta must be implemented compatibly
or accepted as a documented protocol hardening with all mixed-version tests.
Invalid UTF-8 and unpaired Unicode escapes also require explicit corpus coverage;
Go and Serde differ. The deployed Rust transport retains the canonical v1 fallback.

The pure Rust output credit state machine enforces the existing 32-frame/512-KiB
window, exact FIFO ACK sizes and oldest-frame stall deadline. Async transport, cancellation and peer generations have separate integration
tests; these codec fixtures alone do not prove browser/device acceptance.
