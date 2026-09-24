# Historical evidence

Start with [current documentation](../README.md) and [validation status](../VALIDATION.md).
These records preserve what was known at the stated date; pending deployment
statements and old UI experiments are not current instructions.

- [Rust contract plan through 2026-09-25](RUST_CONTRACTS_HISTORY_2026-09-25.md):
  proposal review, former Go source map, migration phases and gates before consolidation.
  [Runtime contracts](../RUST_CONTRACTS.md) remain the durable current reference.
- [Validation through 2026-09-25](VALIDATION_HISTORY_2026-09-25.md): full rollout and
  publication record before separating current evidence from historical checkpoints.
- [Rust migration through 2026-09-24](RUST_MIGRATION_HISTORY_2026-09-24.md): preserved
  candidate checkpoints, reviews, tests, claim limits and the pre-release control
  before documentation consolidation; current control is
  [Rust migration](../RUST_MIGRATION.md).
- [Rust protocol checkpoints](RUST_PROTOCOL_CHECKPOINTS_2026-09-24.md): superseded
  implementation/test narrative; current protocol contracts remain in
  [proto/README.md](../../proto/README.md).
- [Rust contract checkpoints](RUST_CONTRACT_CHECKPOINTS_2026-09-24.json): unchanged
  snapshot of the former per-contract implementation notes, including superseded
  `not_started`/pending labels. The live [inventory](../../tests/fixtures/contracts.json)
  retains every contract and test-owner mapping; current status is in the migration control.
- [Validation through 2026-09-24](VALIDATION_HISTORY_2026-09-24.md): dated tests,
  release receipts, blocked attempts, acceptance evidence and claim limits.
- [Web UI history](WEB_UI_HISTORY_2026-09-09.md): superseded browser experiments.
- [iOS input history](IOS_INPUT_HISTORY_2026-09-09.md): superseded input experiments.

Private logs, screenshots, raw conversations and credentials remain outside tracked
records. Archived evidence is excluded from default ripgrep discovery; search this
directory explicitly with `rg --no-ignore` when investigating history.
