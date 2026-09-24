# Embedded usage primitives

This crate implements the embedded Rust usage primitives, preserving the reachable
HMux `stream.RunWithSources` contracts from the retired
[Token Terrier collector](../../third_party/token-terrier-server/UPSTREAM.md).
Its source attribution and MIT license remain in that directory's `LICENSE` and
`NOTICE`; they remain after Go source retirement.

The crate adds no service, process, credential writer or provider request.
`hmux-home` owns bounded read-only credential I/O, OAuth/LB HTTP, `cswap list --json`,
activity file scanning and shared collector scheduling/publication. These paths
run in the Home process. The Gateway validates received usage snapshots before
retaining bounded browser payloads. Current acceptance and deployment status belong
to [the migration control](../../docs/RUST_MIGRATION.md).

- `credentials`: streaming read-only Claude/Codex credential parsing, at most
  4 MiB; retains access token, account header and opaque identity digest only.
  No refresh token, account email, raw tree or credential serialization.
- `activity`: selective JSONL token parsing (8 MiB line bound), 60-second burn
  window/EWMA/hysteresis and local-day totals. Up to 4096 window events and
  1024 fixed-size session digests; explicit cap/overflow diagnostics report undercount. The
  caller supplies local dates, file offsets, source enrichment and deduplication.
- `oauth`: Claude/Codex API normalization, at most 64 KiB.
- `codex_lb`: pool/key quota normalization, at most 1 MiB/128 limits.
- `codex_accounts`, `cswap`: at most 8 MiB/128 accounts, original observation
  times and source-specific fallback behavior. Codex uses aliases with blank
  emails; cswap's bounded email labels are intentional.
- `transport`: complete public field allowlist, at most 1 MiB, 128 accounts per
  snapshot and two nonrecursive sources, no raw provider objects or credentials.
- `quota_state`: pure OAuth refresh tickets, account-keyed 60-second cache,
  600-second sticky fallback and bounded 429 retry. The Home owner must finish
  every ticket, including error/cancel, and merge activity after I/O.
- `sources`: fixed CLI/secondary projection and publication-time activity merge
  helpers. The Home owner calls them with the current activity state
  after I/O, under its publication ownership.

Only synthetic fixtures are used. Frozen prior-version Go outputs remain
compatibility data; current Rust tests require no Go executable. Duplicate typed JSON fields and malformed
scalar values may reject more strictly than Go. Missing windows remain absent;
no zero-valued observed window is fabricated. Body limits must also be enforced
by each I/O owner before a complete body is allocated; the Home OAuth
HTTP owner enforces that limit while streaming. HTTP support reuses existing
Hyper/rustls and their httpdate dependency; it adds no daemon or HTTP/2 stack.
These caps are implementation limits, not whole-process memory measurements.
