# Native migration fixtures

Synthetic compatibility inputs only. No real credentials, conversations, config,
IP addresses or runtime state may be copied here. Golden outputs were generated
with the Go owners retained at checkpoint `c061f28fe7ea8e865578ac1189240447d0ebaa6f`
and are now consumed by Rust without a Go toolchain. A fixture or unit test does not imply the component is deployed.

[contracts.json](contracts.json) maps contracts to current Rust owners/tests.
`baseline_*` and `historical_migration_tests` refer to the recorded Git revisions,
not paths required in this checkout. Earlier implementation labels and verification notes
are preserved unchanged in the [dated checkpoint inventory](../../docs/archive/RUST_CONTRACT_CHECKPOINTS_2026-09-24.json).
Use [Rust migration](../../docs/RUST_MIGRATION.md) for current completion and gaps.

The fixtures include auth, wire, config/model, catalog/provider bindings,
conversations, usage/activity, workspace, metrics, uploads, push and diagnostics.
Current Rust tests verify these frozen outputs without running Go. Cross-process
Go handoffs were migration evidence and now require retained external artifacts;
see [native verification](../RUST.md#optional-historical-comparisons).

Do not silently regenerate old expected outputs from the implementation under
test. Contract changes need reviewed new vectors, preserving prior-version
fixtures and documented intentional decoding/security differences.
