# Native Rust verification

The native source, default checks and packages are Rust-only. Historical Go
implementation and migration drivers are preserved at Git commit
`c061f28fe7ea8e865578ac1189240447d0ebaa6f`, not as an active second implementation.

## Test entry point

Use the pinned Rust toolchain (1.88.0), Node.js 22+, Python 3, ShellCheck, jq and
tmux. Normal builds consume checked-in Protobuf types; protoc 35.1 is needed only
for `make rust-proto-check`. Cargo dependencies are locked. The default Cargo cache
is `/tmp/hmux-cargo`; override `CARGO_HOME` for your own cache.

```sh
make check
make integration
make build
# Select the directory for this build host:
HMUX_RUST_BUNDLE="$PWD/dist/web-darwin-arm64" make bundle-check
# Linux x86_64 uses dist/web-linux-amd64; ARM64 uses dist/web-linux-arm64.
```

`make build` builds one native host bundle containing `hmux-web`, `hmux-agent`,
web assets, native install wrapper, release metadata, hashes and dependency notices.
Explicit `HMUX_RUST_TARGETS` cross builds need the target linker/SDK; they are not
implied by a build on another OS. `make native-build` builds only native executables.
Build and tests never install or restart maintained services.

`make test` bounds test concurrency to two workers by default (`RUST_TEST_THREADS`).
Ignored tests require explicit resources and are reported as skipped. The native
pair gate launches actual `serve`/`connect` commands through a test-local TLS proxy,
logs in and checks catalog/terminal ACK and reconnect using synthetic host tools.
The Home WSS suite independently checks both JSON v1 and Protobuf v2. Real tmux
session/view tests use a disposable isolated socket and fake providers.
The native CLI gate also drives the guided installer through a PTY with synthetic
bundle tools: service opt-out, literal workspace input, plain output, invalid
connection input and cancellation before setup. A valid-token Yes case stops at
an intentional setup failure; these checks do not register a real service.

The opt-in native metrics check reads only the host's CPU, RAM and filesystem
statistics; GPU support is optional. It does not attach to tmux or provider work:

```sh
CARGO_HOME=/tmp/hmux-cargo cargo test -p hmux-home --lib \
  metrics::tests::native_host_samples_have_fresh_cpu_memory_and_disk \
  --locked -- --ignored --exact
```

Run it with normal host inspection permissions. Passing it verifies collection,
not the authenticated browser's rendering or its clock/freshness behavior.

The compatibility names `rust-check`, `rust-build`, `rust-package`,
`rust-bundle-check`, `rust-native-cli` and `rust-native-matrix` invoke current Rust
checks/builds. They do not compile the retired Go implementation.

## Unified installation and first-login setup

`hmux-web install` chooses role (Gateway/Home/both) and target (local/SSH).
Focused native CLI/PTY checks cover role selection, cancellation, conflicting
options, private connection import, opt-out from Home startup, and noninteractive
`init-web` token preservation. Unit tests check transfer manifests, path/host
injection rejection, Gateway managed-file rollback and secret-state validation.
Gateway HTTP tests cover origin/token denial, TOTP and non-TOTP setup, atomic
account creation, normal login after setup and restart with setup retired.

These fixtures do not provision an actual Linux host, call production SSH,
install packages, issue public certificates or activate a real user service.
Before a release promises automatic provisioning on a distribution, exercise
managed/external HTTPS, repeat installation, failed activation/rollback and SSH
cancellation on disposable Linux/macOS hosts. Keep existing maintained services
and original tmux/provider sessions outside those tests.

## Optional historical comparisons

Frozen JSON fixtures remain normal Rust tests. A few ignored Rust cross-language
tests and Python benchmark/handoff runners accept **explicit retained external
binaries**; these are historical comparison tools, not the build or CI path.
They never rebuild Go from the active checkout. Their historical sources are
available at the checkpoint above, and `legacy_baseline.py` records those source
hashes separately from the current runner and binary hashes. Fetch that Git commit
if using a shallow checkout. Do not substitute unverified artifacts or production
state. Keep raw outputs private.

For resource runners set `HMUX_RUST_WEB_BIN` to the exact release executable and
`HMUX_GATEWAY_ORACLE_BIN` to the retained baseline test executable. Use a new
absolute private output directory:

| Target | Required output variable | Extra inputs |
| --- | --- | --- |
| `rust-native-stress` | `HMUX_STRESS_OUTPUT` | Optional `HMUX_STRESS_CYCLES` |
| `rust-native-capacity` | `HMUX_STRESS_OUTPUT` | Both wire codecs |
| `rust-native-activity` | `HMUX_ACTIVITY_OUTPUT` | Synthetic JSONL/terminal workload |
| `rust-native-perf` | `HMUX_PERF_OUTPUT` | Linux, `HMUX_GO_WEB_BIN` retained baseline, optional pairs/count/idle |
| `rust-native-soak` | `HMUX_SOAK_OUTPUT` | Optional `HMUX_SOAK_SECONDS` and codec; success requires final receipt |

`rust_helper_compat.py`, `rust_installed_pairs.py` and `rust_native_matrix.py` also
accept old/new binary paths explicitly. Ignored Rust helpers use the documented
`HMUX_GO_*` variables beside each test. Run them only with the matching retained
test executable from the checkpoint. Previous Go↔Rust evidence remains dated in
[the archive](../docs/archive/README.md); it is not a new acceptance result.

## Browser and device acceptance

Use an isolated Rust instance with private synthetic credentials/workspaces and
`hmux-e2e-*` tmux sessions. Do not use real conversations, providers or login state
in published evidence. Record exact binary/asset hashes, device/browser versions
and actual observations.

- Desktop Safari/Chrome: login and logout/revocation, tab selection/close,
  selection/copy, links/uploads, IME multiline editing, resize and reconnect.
- Physical iOS/PWA and Android: composition/auxiliary keys, keyboard viewport,
  background/resume, reconnect, file attachment and notification deep links.
- Rust Gateway/Home/helper: catalog, provider bindings/conversations, usage,
  shared layouts, persistent login and original-session survival after view close.
- Services: isolated real OS account, login/reboot and current-state rollback;
  retain old artifacts without restoring stale credentials/session stores.
- Soak/resources: fresh progress plus final successful receipts tied to exact
  executable hashes; short smoke tests do not pass 24h/72h gates.

Source retirement does not claim these remaining gates passed. The current
acceptance queue is [RUST_MIGRATION.md](../docs/RUST_MIGRATION.md).
