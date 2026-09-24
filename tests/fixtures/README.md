# Native migration fixtures

Synthetic compatibility inputs only. No real credentials, conversations, config,
IP addresses or runtime state may be copied here. Golden outputs are generated
with the existing Go owners, then consumed by Rust and checked back in Go where
supported. A fixture or unit test does not imply the component is deployed.

[contracts.json](contracts.json) maps all 92 contracts to their Go owners and
available Rust test owners. Earlier implementation labels and verification notes
are preserved unchanged in the [dated checkpoint inventory](../../docs/archive/RUST_CONTRACT_CHECKPOINTS_2026-09-24.json).
Use [Rust migration](../../docs/RUST_MIGRATION.md) for current completion and gaps.

| Domain | Evidence | Current coverage |
| --- | --- | --- |
| Auth v1 | `auth-v1/synthetic.json`, Go/Rust oracle and synthetic HTTP/process tests | PBKDF2/TOTP/session DTOs, restart/revoke/toggle and current-state Go/Rust rollback; no real account state |
| Session location | `session-location-v1/go-addresses.json`, synthetic HTTPS/account tests | Go public/reserved IP boundaries, fixed endpoint, bounded cache/timeouts, account isolation and lookup cancellation on logout; no external lookup in tests |
| Model/config | `config-v1/`, Go/Rust oracle | Model validation and private read-only config; no writes |
| Usage preferences | `usage-preferences-v1/go-oracle.json`, synthetic HTTP/store tests | Account/profile isolation, revision/default/JSON parity and Rust/Go/Rust current-state handoff; no production settings |
| Usage payload/source parsing | `usage-transport-v1/`, `usage-oauth-v1/`, `usage-codex-v1/`, `usage-cswap-v1/`, `usage-state-v1/` | Actual Go allowlist, provider normalization, cswap status/original-time and Codex pool/alias fixtures; bounded Rust parsers, pure quota cache/retry decisions and both-codec gateway rejection; shared Home source owners, synthetic HTTP/CLI/account refresh and both-codec publication; no actual provider calls |
| Usage credentials | `usage-credentials-v1/{cases,go-oracle}.json` | Actual-Go token/account precedence; streaming credential parser, bounded safe Home file cache and one-retry OAuth owner; synthetic files/tokens only |
| Usage activity | `usage-activity-v1/{parser-cases,parser-go-oracle,burn-steps,burn-go-oracle}.json` | Actual-Go JSONL token and burn state outcomes; bounded synthetic JSONL scanner, events/sessions and explicit local-day dates; append/reconnect/failure tests; no real transcripts |
| Shared workspace | `workspace-v1/go-oracle.json`, Rust model tests | Pure transitions and private Rust storage against actual Go results; synthetic account routing/restart/cancellation and current-state Rust/Go/Rust handoff; both-codec Home workspace replay/reconnect and recycled-identity rejection; synthetic multi-boot lineage checks; physical reboot/platform acceptance pending |
| Workflow | Actual Go/Rust disk and output oracle, synthetic hooks, native helper handoffs | Sanitized lifecycle, private locking, concurrent helpers, retention, exact birth binding and streamed text views; current-state helper handoffs pass on both OSes |
| Recovery | Actual Go/Rust disk bridge, synthetic tmux fixtures | Actual Go/Rust checkpoint/null/pending and mutual-lock handoff; disposable macOS tmux with fake providers validates interrupted reboot/retry and gate ordering; Linux native synthetic tests pass; physical reboot and Linux real-tmux acceptance remain separate |
| Provider setup | Fake provider CLI and isolated setup socket tests | Status/key/jobs/profile and bounded cancellation/shutdown owners; no actual install/login calls |
| Static assets | `static-v1/go-oracle.json`, synthetic file and HTTP tests | 63 actual Go gateway cases (Markdown MIME normalized) for deployed MIME types, redirects, HEAD, conditions and ranges; bounded streaming/cancellation and live root-link switching; candidate path/error policy differs explicitly (see migration plan) |
| Browser diagnostics | `diagnostics-v1/go-oracle.json`, synthetic store/HTTP tests | 41 decode cases and 140 actual Go transitions with record hashes; compact templates reconstruct input batches; privacy, account isolation, rate/history caps, TTL, corrupt-file preservation, joined final saves and current-state Rust/Go/Rust handoff; no production logs |
| File uploads | `upload-v1/go-oracle.json`, candidate HTTP and isolated Go receiver | 17 start/19 completion cases, chunk hashes and response binding; v1/v2 relay plus actual Go peer/worker/filestage commits with synthetic identity verifier and private spool; Rust Home stage owner and synthetic WSS upload assembly also pass; release/device acceptance pending |
| [Push state](push-v1/README.md) | `push-v1/go-oracle.json`, isolated actual Go helper | 26 endpoint/13 key/8 login-ID/26 state cases, bounded private storage, lifetime flock, cancellation/revocation and current-state Rust/Go/Rust unsubscribe; current-state storage compatibility |
| Push crypto | RFC 8291 vector, actual Go sender/independent Go decryptor | Exact standard ciphertext; cross-language ES256, headers, topic and empty/Korean/max payload checks; bounded stored-key preparation with revocation and exact-subscription checks; no provider requests |
| Push routes/delivery | `push-v1/go-api.json`, synthetic HTTP/Protobuf/TLS peer, independent Go receiver | 41 raw-body API cases, account/presence/deep-link delivery, late workspace/presence changes, transfer/throttle/410/logout; no provider requests |
| Tmux catalog/process graph | `catalog-v1/go-oracle.json`, `catalog-v1/go-process.json`, fake-command Rust tests | Go basic session/window parsing and eight process selection/wrapper cases; synthetic current ownership and admission tests; no real provider records |
| Host metrics | `hostmetrics-v1/go-oracle.json`, Go/Rust oracle and synthetic sampler tests | 30 actual Go CPU/memory/GPU/disk cases; bounded sampling, failure replacement, both-codec nonblocking catalogs and child cleanup; native accuracy/resource validation pending |
| Completion observation | `completion-v1/go-oracle.json`, Go/Rust oracle and synthetic two-codec peer tests | 23 Go event cases and three IDs; safe baseline, inode/shrink/anchor/binding changes, bounded append and no history replay on reconnect; no real push calls |
| Public conversations | `conversation-v1/go-oracle.json`, synthetic Rust peer/filesystem tests | Actual Go public filters and stable IDs, both Home codecs, exact session/pane/provider/file rechecks, text/encoded caps and cancellation; no private transcripts |
| [Wire v1](wire-v1/README.md) | Actual Go decode/marshal plus Rust re-encode | Envelope/bytes/field boundaries; documented codec deltas remain |
| [Protobuf v2](../../proto/README.md) | Codec/v1 adapter, paused duplex and authenticated TCP tests | Bounded envelopes, heartbeat, hub lifetime and retained-payload budget; no production activation |
| Private storage/locks | `hmux-core` tests; Go `filelock` helper via `make rust-compat` | Atomic commit-stage errors and cross-process exclusion on isolated files; no production state writes |
| Native commands | `hmux-core/tests/command.rs` | Synthetic owned-child admission, bounded output, partial exits and cancellation/reaping; no provider/tmux process tests |
| Full gateway executable | `runtime.rs`, `rust_full_gateway_test.go`, `make rust-gateway-e2e` | Assembled owners and actual Go Home catalog/recovery/request workers plus owned PTY; synthetic tmux/process tools and loopback networking; no real provider/tmux or browser-device acceptance |

The owning [migration plan](../../docs/RUST_MIGRATION.md) tracks the overall lane.
Keep detailed tests with the owner; do not duplicate changing phase state here.
