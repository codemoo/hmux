# Native runtime measurements

This reference records scoped measurements, not release acceptance. The gateway
driver below and native gateway/Home runners have different measured boundaries;
each result states its workload, exclusions and artifact provenance.

## Deployed-artifact memory comparison (2026-09-25)

The retained deployed Go executable and current deployed Rust executable were
measured on the same Linux amd64 host in isolated native gateway/Home fixtures.
Three runs each of Go JSON, Rust JSON and Rust Protobuf completed: nine runs,
90,000 sequential 64-byte echoes and zero errors. Each run uses the existing
functional/slow-view precondition, 20 warm-up echoes, 10 seconds connected idle,
then 10,000 measured echoes. Go uses default GC settings. Go/Rust JSON order
alternates; Protobuf runs third. Real services and tmux sessions were unchanged.

PSS in MiB; medians of three checkpoints per lane:

| Lane | Idle gateway | Idle Home | Idle pair total | Pair total after 10,000 echoes |
| --- | ---: | ---: | ---: | ---: |
| Go / JSON v1 | 11.62 | 13.34 | 24.88 | 25.65 |
| Rust / JSON v1 | 4.61 | 6.33 | 11.05 | 11.05 |
| Rust / Protobuf v2 | 4.75 | 6.30 | 11.08 | 11.07 |

Pair totals are medians of each run's gateway+Home sum, so they need not equal
the sum of independently rounded role medians. Rust Protobuf pair PSS was 55.5%
lower at idle and 56.8% lower after echoes. These are short synthetic checkpoints,
not peaks, whole-Home collector/conversation workloads or long-term acceptance.
Browser, provider CLIs, tmux, proxy and oracle memory are excluded.

The Go SHA256 was `0fba1c375d41b2551b7b9ca119ece37b66b189ded363f81c460a36ac9cee0be1`;
Rust was `7d355330907bed6501199544480227f2502f16252132f310971bb281ea7a364c`.
The native oracle hash, harness hashes, all process checkpoints and raw RTT files
remain in private artifacts. This is a fresh run, separate from earlier candidates.

Separately, read-only live samples at 2026-09-24 16:14–16:15 UTC showed Linux
Gateway PSS 9.95 MiB / RSS 12.16 MiB (ten one-second samples, zero swap), and
macOS Home RSS 134.81 MiB (ten samples). A subsequent macOS `vmmap -summary`
reported physical footprint 18.1 MiB and lifetime peak footprint 30.5 MiB. RSS
includes shared and reusable resident pages; footprint and Linux PSS are distinct
OS metrics and are not added into a cross-host total. No matched live Go sample
exists, so these current readings do not establish production before/after savings.

## Gateway-only harness

Initial harness, not a complete performance report. It runs an explicit gateway
binary as a child with new private temporary credentials/state/assets and uses a
synthetic Home. It does not access tmux or provider state. The only network endpoint
is a freshly allocated loopback port; `hmux.example` is a Host/Origin value, not a
remote destination. TOTP and the real password KDF remain enabled during warm-up.

The initial Go driver and its generation commands remain at source checkpoint
`c061f28fe7ea8e865578ac1189240447d0ebaa6f` (`bench/hmux/go-driver`).
They are historical evidence, not part of the current build. Native comparisons
use explicit retained baseline/oracle executables; see
[optional historical comparisons](../../tests/RUST.md#optional-historical-comparisons).

Use `--catalog 0` for S00, or `--views 1|2|4|8` for S02. `--gogc` and
`--gomemlimit` are explicit Go-child-only tuning options; inherited values are
removed. Results refuse to overwrite an existing file. Failed runs have status
`failed`, not zero-valued successful metrics. Runtime values are synthetic, but
keep local output outside the public tree until provenance/claim review.

Use `--gateway rust` for the native packaged `hmux-web serve` command.
The driver also accepts `--gateway rust-candidate` with the separate
`target/release/examples/gateway_candidate` executable. It rejects Go GC flags
for both Rust roles and records the gateway implementation and `json-v1` Home protocol
in the result. Both children use sanitized temporary HOME/PATH environments.
Rust eagerly loads native TLS trust; Go does so on outbound demand. Account for
that difference when interpreting idle observations. This does not benchmark
Protobuf, the Rust Home, full asset loading or any unimplemented migration feature.
On constrained macOS runners, native trust loading or the child-PID `ps` query
may require running this isolated harness outside the sandbox.

The measured boundary is only the gateway child PID. Driver/fixture generation,
Nginx/TLS, browser, real Home and providers are excluded. Linux samples smaps_rollup
RSS/PSS and proc task/FD counts; macOS samples RSS via ps, reports other unavailable
values as null and does not label RSS as footprint/PSS. Sampling is every 250ms
plus collection time, not continuous peak measurement. Driver overhead is outside
the measured PID but can affect timing; it is not a production configuration.

The historical harness implements cold HTTP readiness and warmed quiet S00/S01/S02
memory samples only. No CPU, latency, active-output, fault, cgroup, 24h/72h or real
browser results are claimed. Full product assets, usage/push warm-up, paired runs,
result aggregation, hardware/revision provenance and S03–S14 remain required for
release measurements. These initial smoke runs are harness checks, not budgets.

## Preliminary native Linux samples (2026-09-24)

Three alternating Go/Rust pairs per scenario, native `serve`, 3-second warm-up and
10-second sampling on the same Linux amd64 host. Values below are medians of each
run's median PSS. Go uses its defaults; the Rust binary is an optimized candidate.

| Quiet scenario | Go gateway PSS | Rust gateway PSS |
| --- | ---: | ---: |
| No Home (S00) | 11.94 MiB | 6.24 MiB |
| One synthetic Home, 100 catalog entries (S01) | 13.34 MiB | 6.57 MiB |
| Same catalog, four quiet views (S02) | 13.88 MiB | 6.72 MiB |

These are gateway-only observations, not a full-product or tuned-Go comparison.
The proposed five-pair budget gate, CPU/latency, active output, real Home/browser,
and long soaks remain unvalidated. Process readiness was 20–24 ms, but polling
uses a 20 ms interval; do not infer a small startup-speed advantage from it.
Raw private samples retain binary/source hashes, kernel/CPU metadata and run order.
They remain outside tracked documentation; no production address/state was sampled.

## Native lifecycle stress (2026-09-24)

The separate `make rust-native-stress` harness runs real native Rust gateway/Home
processes with synthetic tmux/provider tools and isolated TLS/state. Linux amd64
passed 10,000 view open/input/close/cleanup cycles for each codec, plus a withheld
render-credit browser beside a healthy browser/control request, lossless resumption,
Home replacement, logout and shutdown. macOS arm64 passed 100 cycles per codec
with the race-enabled Go oracle. No production service or session was used.

Linux RSS checkpoints before/after the 10,000-cycle phase:

| Codec | Gateway RSS | Home RSS |
| --- | ---: | ---: |
| Protobuf v2 | 8.35 → 8.42 MiB | 10.11 → 10.41 MiB |
| JSON v1 | 8.36 → 8.38 MiB | 10.11 → 10.37 MiB |

Gateway threads stayed at 3 and Home at 4; final FD counts were 18 and 13, with
no increase over the initial checkpoints. Samples were taken every 100 cycles.
This is bounded lifecycle evidence, not a peak-memory, CPU/latency, Go comparison
or 24h/72h soak result. Binary hashes and all checkpoints remain in private run
artifacts. Subsequent installer-only fixes have separate native test evidence.

## Native view capacity (2026-09-24)

`make rust-native-capacity` passed on macOS arm64 and Linux amd64, for JSON v1
and Protobuf v2. Each native Rust gateway/Home pair admitted eight simultaneous
views, rejected the ninth with WebSocket close 1013 without creating a Home PTY,
preserved echo/ACK on all admitted views, reused one released slot and drained
all disposable views. The original synthetic tmux identity remained available.
The macOS oracle used Go race instrumentation; Linux used a cross-compiled oracle
without race instrumentation. This is bounded capacity acceptance, not a stress
or whole-product memory-budget result.

Linux gateway/Home FD counts were 19/13 before, 27/29 at capacity and 19/13 after
drain; threads remained 3/4 in both codecs. Protobuf PSS was gateway/Home
4.51/6.18 MiB before and 4.51/6.43 MiB after; JSON was 4.51/6.17 MiB before and
4.58/6.36 MiB after. macOS reports RSS only, with unavailable PSS/FD/thread fields
left null. These four checkpoints per role are neither memory peaks nor evidence
of long-term retention. Exact values, binary hashes and logs remain in the private
results. The runtime binaries match the sustained measurement/candidate soak;
only the oracle and its opt-in scenario changed.

## Native paired idle and socket echo (2026-09-24)

`make rust-native-perf` measures real native gateway/Home processes with synthetic
host tools and TLS. Five Go/Go JSON-v1, Rust/Rust JSON-v1 and Rust/Rust Protobuf-v2
runs completed: 15 runs, 1,000 sequential 64-byte inputs each, zero errors. Go/Rust
JSON order alternates between pairs; the Protobuf lane runs third. Each run warms
one persistent view with 20 echoes, then samples 10 seconds of connected idle.
The preceding full integration/slow-view burst is part of the precondition; this
is a warm post-burst state, not a cold or untouched idle baseline.

Linux x86_64, kernel 5.15.0-179-generic, 12 affinity CPUs, about 31.1 GiB host RAM.
Cgroup limits were unavailable, not unlimited. Rust release 1.88.0 and default
Go 1.26.2; the cross-compiled oracle also uses Go 1.26.2 without race instrumentation.
The remote Go executable was unavailable to the version probe; its separately
captured build metadata establishes the version. Both toolchain and source/binary/
dependency hashes remain with the private samples.

Medians across five runs (PSS is the final idle checkpoint):

| Pair / codec | Gateway PSS | Home PSS | RTT p50 | RTT p95 | RTT p99 | Echoes/s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Go / JSON v1 | 11.41 MiB | 12.99 MiB | 0.108 ms | 0.142 ms | 0.243 ms | 8,095 |
| Rust / JSON v1 | 4.41 MiB | 6.29 MiB | 0.131 ms | 0.152 ms | 0.188 ms | 7,322 |
| Rust / Protobuf v2 | 4.58 MiB | 6.17 MiB | 0.118 ms | 0.141 ms | 0.168 ms | 7,944 |

This workload showed lower Rust PSS and p99, but higher median RTT and lower echo
throughput than Go. It is not evidence that every interaction is faster. P99 ranges
were 0.229–0.343 ms (Go), 0.170–0.205 ms (Rust JSON) and 0.152–0.192 ms (Protobuf).
RTT ends at oracle socket receipt, not an xterm write callback. Throughput elapsed
time includes synchronous resource samples; driver, browser, proxy, tmux and
provider memory/CPU are excluded. These checkpoints do not establish peak memory.

Median process CPU deltas per 1,000 echoes were gateway/Home 0.11/0.07 s (Go),
0.07/0.04 s (Rust JSON) and 0.07/0.03 s (Protobuf). Active phases lasted only about
0.12–0.14 s; the 10 ms CPU-counter resolution makes these coarse observations.
Idle medians had no measurable tick increase; this does not prove zero idle CPU
or the proposed 0.1% budget. Tuned-Go comparison, sustained CPU/throughput, multiple
views, browser rendering, fault/capacity, host collector workloads and soaks remain.

The measured Rust executable SHA-256 was
`26ca88684de99c3a493d6314d62d4eaeea40fccf7efd412390edb1317f1f3873`.
A subsequent SOCKS mapped-IPv4 encoding correction does not exercise this direct
connection workload, but its rebuilt binaries have distinct hashes. Preserve these
results against the measured artifact; do not silently relabel them as a new run.
The first measurement attempt failed before launch because the isolated copy lacked
oracle source files for provenance; copying them completed the reported runs.
No production service, credentials or original tmux sessions were touched.

## Sustained native idle and socket echo (2026-09-24)

The sustained workload uses the same native gateway/Home boundary, host and
toolchains as the short paired workload, with 60 seconds connected idle followed
by 250,000 sequential 64-byte echoes per run. Three repetitions of default Go,
tuned Go, Rust JSON and Rust Protobuf completed: 12 runs and three million echoes,
zero echo errors. A joined socket reader remains active during idle to answer
WebSocket pings. Raw RTT files and their SHA256 hashes were verified.

The initial tuned-Go lane also passed its GC settings into the Go oracle and
synthetic tmux/echo process. Those three runs are preserved but excluded from isolated
runtime-tuning conclusions. The nine default-Go/Rust runs below have no such
tuning; they contain 2.25 million echoes. A tool-isolated rerun was stopped when
main found the oracle still received tuning. The corrected Go-only runner clears
GC/runtime settings from the oracle and tools, forwarding settings only to native
candidates through separate harness variables. Smoke and independent isolation
review passed; its completed three-pair result follows. Earlier evidence is not relabeled.

Medians of three runs; PSS is the last connected-idle checkpoint:

| Native pair / codec | Gateway PSS | Home PSS | RTT p50 | RTT p95 | RTT p99 | Echoes/s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Go / JSON v1 | 12.26 MiB | 12.70 MiB | 0.109 ms | 0.182 ms | 0.342 ms | 8,255 |
| Rust / JSON v1 | 4.51 MiB | 6.26 MiB | 0.132 ms | 0.163 ms | 0.196 ms | 7,238 |
| Rust / Protobuf v2 | 4.56 MiB | 6.19 MiB | 0.120 ms | 0.147 ms | 0.176 ms | 8,009 |

Process CPU consumed during 250,000 echoes, measured in 10 ms ticks:

| Native pair / codec | Gateway CPU | Home CPU | Gateway/Home final active PSS |
| --- | ---: | ---: | ---: |
| Go / JSON v1 | 27.12 s | 18.41 s | 11.92 / 13.26 MiB |
| Rust / JSON v1 | 18.65 s | 10.11 s | 4.52 / 6.29 MiB |
| Rust / Protobuf v2 | 17.16 s | 8.08 s | 4.57 / 6.20 MiB |

These samples support lower Rust memory and process CPU for this workload;
median latency still favors Go. Keep the observed noise: Go p99 ranged from
0.266 to 2.891 ms and throughput from 5,210 to 8,831 echoes/s; Rust JSON p99 was
0.190–0.201 ms and Protobuf 0.172–0.186 ms. The slow Go run is retained, and its
cause has not been attributed. Three repetitions are exploratory evidence, not
the final paired budget acceptance. The Protobuf lane always runs last within
each repetition, so ordering is another limitation.

Median idle CPU deltas over 60 seconds were gateway/Home 0.01/0.04 s for Go,
0.00/0.02 s for Rust JSON and 0.01/0.01 s for Protobuf. Zero recorded ticks is
below resolution, not proof of zero work. RTT ends at socket receipt; process
samples exclude browser, driver, synthetic tools, proxy and providers. Sampling
overhead is included in echo elapsed time; checkpoints are not memory peaks.

The measured Rust binary SHA256 is
`5d43e69f11a1ae992a7ab4bee0fb8bccc0422a1a073019c56784113d5344a8f9`;
Go is `3df0c0fa7063d8e8d4f1121d197ee4a706bcd3a509be90eb7ca2c0bb963d9936`.
The private result retains the oracle hash, exact harness source snapshot,
dependency hashes, machine metadata, run order and all raw samples. The earlier
idle-reader harness failure is separate from these completed runs. No service,
production data or pre-existing tmux session was used.

## Corrected native Go tuning comparison (2026-09-24)

Three alternating default/tuned Go pairs completed with the same binaries and
250,000 echoes plus 60 seconds idle per run: 1.5 million echoes, zero errors.
Only native gateway/Home receive `GOGC=50 GOMEMLIMIT=32MiB`; the Go oracle and
synthetic tools keep their default runtime settings. An independent isolation
review and smoke preceded this run. All six raw RTT hashes and p50/p95/p99 values
were verified against their 250,000 samples.

Medians of three runs; idle PSS is the final idle checkpoint, CPU covers echoes:

| Go setting | Gateway PSS | Home PSS | Gateway/Home CPU | RTT p50 | RTT p95 | RTT p99 | Echoes/s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Default | 11.74 MiB | 12.79 MiB | 25.59 / 17.42 s | 0.104 ms | 0.140 ms | 0.247 ms | 9,015 |
| GOGC=50, GOMEMLIMIT=32MiB | 10.42 MiB | 11.51 MiB | 31.93 / 20.29 s | 0.112 ms | 0.221 ms | 0.329 ms | 7,783 |

The tuned settings traded lower PSS for higher process CPU and socket latency in
this workload. Default p99 ranged 0.244–0.252 ms; tuned p99 0.300–0.330 ms.
Median idle CPU gateway/Home was 0.01/0.03 s and 0.00/0.04 s respectively over
60 seconds; zero is below the 10 ms counter resolution. The earlier Rust runs
are a separate block, not simultaneous tuned-Go/Rust pairs; do not merge them
into a fresh paired budget verdict. No global or production Go settings changed.

The Go executable hash is unchanged from the sustained comparison. The corrected
oracle SHA256 is
`ecbad9ee2cb7953cb5f21ebcd4c5591a643a8063c3f81381162c0bb43e8f8008`.
The source snapshot and all raw artifacts remain with this separate result. The
first confounded tuning run and interrupted tool-only correction remain recorded;
neither is accepted as isolated native tuning evidence. Socket receipt, sampling,
synthetic fixture and whole-product exclusions remain the same as above.

## Incremental activity reader (2026-09-24)

The `hmux-home` example `activity_bench` exercises the production JSONL reader
with only private synthetic files. A single reader processes both providers;
each file starts with 32 events. Ten quiet polls must read zero content bytes,
then eight files per provider receive 64 appended events. Atomic inode replacement
and follow-up quiet polls must update totals exactly once. File counts and byte
counts are checked, incomplete scans fail, and the 1,024-session accounting cap
must remain explicit while token totals continue to accumulate.

Native release builds, three runs per size on each OS, each preceded by an
8-file-per-provider smoke. Medians across runs (quiet is each run's median of ten
immediate polls); these are separate host observations, not a controlled OS comparison:

| Host | Total files | Bootstrap | Quiet poll | Append burst | Inode replacement |
| --- | ---: | ---: | ---: | ---: | ---: |
| macOS arm64 | 1,024 | 411.5 ms | 7.3 ms | 19.5 ms | 6.8 ms |
| macOS arm64 | 4,096 | 1,634.8 ms | 31.9 ms | 45.9 ms | 32.5 ms |
| Linux amd64 | 1,024 | 453.3 ms | 4.1 ms | 19.5 ms | 4.3 ms |
| Linux amd64 | 4,096 | 1,996.6 ms | 18.6 ms | 35.1 ms | 18.7 ms |

On macOS, bootstrap consumed 5,943,872 / 23,819,584 content bytes respectively;
both sizes then consumed 184,832 appended and 361 replacement bytes. On Linux,
the corresponding counts were 6,042,176 / 24,212,800, then 187,904 and 367.
The generated current-day RFC3339 timestamps had different fractional precision;
the host workloads are therefore not identical byte-for-byte. All quiet polls
consumed zero content bytes. Metadata traversal still occurs, so
zero content bytes does not mean zero filesystem work. Bootstrap ranges were
377.3–413.4 ms and 1,629.4–1,690.5 ms. All totals, file counts and cap assertions
passed. Linux bootstrap ranges were 451.4–458.2 ms and 1,970.6–2,003.2 ms.
The private samples retain source/lockfile and executable hashes.

The first Linux smoke rejected all fixture files because its inherited umask
0002 created group-writable files. The reader correctly rejected them as unsafe;
the fixture now explicitly creates mode 0600 files inside its mode 0700 tree.
Corrected Linux smoke/measurements and macOS smoke pass. The macOS measured
artifact predates this fixture-only permission correction and retains its own
hash/source snapshot; the production reader is unchanged. Strict example Clippy
passes on both OSes. No permissions on actual provider files were changed.

Fixture creation is excluded from scan times; filesystem caches are not cold.
Polls are immediate and use a fixed synthetic event clock, not the production
cadence. Times include admitted worker scheduling, metadata and parsing within
the example process. This is not a packaged Home memory/CPU result, external
provider request measurement, Go comparison or browser latency result. The
existing macOS synthetic soak ran independently during these samples. Linux
measurements ran after native echo comparison and before its soak. Full packaged
collector/concurrent-load acceptance remains separate.

## Native collector with terminal input (2026-09-24)

`make rust-native-activity` runs the actual native Home collector and gateway,
with 512 private JSONL files per provider and 32 initial events per file. It
checks authenticated state totals of 16,384 → 17,408 → 17,411 tokens per provider
through backfill, append and inode replacement; session counts stay exactly 512.
Two six-second quiet phases cross the production five-second scan cadence and
reject replay. One persistent view sends echo markers during collector polling;
fixture writes themselves are synchronous. All eight runs below passed exact
state, echo, view cleanup and subsequent reconnect/revocation/shutdown assertions.
The wrapper verifies explicit phase/result/resource markers and raw percentiles;
a skipped/older oracle cannot pass. Independent integrated-harness review found
no material blocker for this scope.

Initial runs used the previously packaged release executables. Short-run Linux
samples contained 42–45 ms outliers. Inspection found Home's outbound TCP sockets
left Nagle enabled, unlike the Gateway accept path and Go's TCP default. The Home
dialer now enables TCP_NODELAY before TLS/proxy wrapping and on its loopback LB
socket. New native release executables passed the same workload on both OSes and
codecs; 21 focused dial checks per OS passed (native trust-store check remains
explicitly opt-in), plus local strict library Clippy and formatting.

One run per OS/codec/version; RTT is socket receipt in milliseconds. Memory is the
final active-view checkpoint in MiB: **macOS RSS, Linux PSS**, gateway / Home.
These metrics are not comparable across operating systems.

| Host | Home TCP setting | Codec | Echoes | RTT p50 / p95 / p99 | Maximum RTT | Gateway / Home memory |
| --- | --- | --- | ---: | --- | ---: | --- |
| macOS | before | protobuf | 140 | 0.407 / 1.253 / 4.565 | 81.267 | 9.78 / 13.73 |
| macOS | before | json | 141 | 0.438 / 0.726 / 1.756 | 1.939 | 9.97 / 14.14 |
| macOS | NODELAY | protobuf | 140 | 0.411 / 0.648 / 1.595 | 1.870 | 10.17 / 14.45 |
| macOS | NODELAY | json | 141 | 0.435 / 0.838 / 4.117 | 12.335 | 10.19 / 14.47 |
| Linux | before | protobuf | 143 | 0.250 / 0.349 / 42.553 | 42.812 | 4.53 / 7.38 |
| Linux | before | json | 144 | 0.288 / 0.424 / 0.639 | 44.653 | 4.47 / 7.38 |
| Linux | NODELAY | protobuf | 144 | 0.699 / 1.052 / 1.160 | 1.797 | 4.64 / 7.28 |
| Linux | NODELAY | json | 144 | 0.841 / 1.202 / 1.466 | 1.514 | 4.53 / 7.41 |

No 40 ms tail occurred in the Linux NODELAY recheck, but its median and process
CPU increased in these short samples; macOS JSON also had a higher p99. Do not
interpret this as a universal latency improvement or final regression-budget
acceptance. Linux CPU deltas across the five checkpoints were 0.03/0.04 s
(Protobuf) and 0.04/0.06 s (JSON) before; 0.10/0.08 and 0.10/0.12 s after, for
Gateway/Home respectively. Linux counters have 10 ms resolution. All Linux
checkpoints retained gateway/Home thread counts 3/5 and FD counts 20/15.

Both hosts had their independent original-candidate soak running. Mac oracle
used the race detector; the Linux oracle was cross-compiled without it. No build
ran on a host during its measured recheck. Samples begin after native startup,
so backfill readiness here is not cold-start timing. Resource samples are not
peaks, full provider polling or Go comparisons; RTT excludes browser rendering.
Sources, binary hashes, logs and raw RTTs remain with the private run artifacts.
The NODELAY executables are rebuilt native candidates, not refreshed distribution
bundles or a deployment. Running soaks retain their original frozen binaries;
final RC packaging/soak must include subsequent transport changes.

## Rust codec samples

```sh
CARGO_HOME=/tmp/hmux-cargo cargo run --locked --release -p hmux-protocol \
  --example codec_bench -- 10000 > /tmp/hmux-codec-samples.json
```

This separate microbenchmark compares the Rust JSON v1 compatibility adapter
against the validated Protobuf v2 codec for synthetic 1/64/16384-byte terminal
output, 32 KiB input, a 256 KiB upload chunk, output ACK, profiles request and a
100-session catalog. The current catalog case uses typed Protobuf fields and the
same semantic snapshot through the JSON v1 adapter. Earlier JSON-body catalog
results retain their original source/binary meaning. Seven rounds
alternate codec order after warm-up. Schema 2 output identifies each operation
and direction, with raw elapsed nanoseconds, iteration counts and encoded bytes.
Correctness is checked before timing. Each decoded result is dropped before the
next iteration. Use `100` iterations for a quick harness smoke; the same selected
count applies to every case, including the large upload chunk.

Use a quiet machine and record source revision/dirty diff, toolchain and machine
conditions beside the result. These are codec-only wall times, not process CPU,
allocation counts, RSS/PSS, throughput under backpressure or rendered latency.
Both implementations are Rust; this is not a Go-versus-Rust benchmark. No gateway
or Home memory/performance claim follows from these samples alone.
Decode reuses pre-encoded inputs: Protobuf can borrow their `Bytes` backing,
whereas JSON/base64 decoding allocates decoded bytes. The hub's separate retained
payload copy/accounting, real socket allocation and browser rendering are excluded.


The 2026-09-24 `snapshots1` smoke (100 iterations × seven rounds on macOS arm64)
verified all codec round trips. Its 100-session catalog encodes to **24,617 bytes
in JSON v1 and 6,013 bytes in typed Protobuf**. This is a deterministic wire-size
comparison of the synthetic fixture, not a process memory or latency result.
Background migration tests were active; no timing-budget conclusion is drawn.
Raw result SHA-256: `f0ad56eeb6cbebdce3ab8f17c609b17d2663897c2424567b8657ca161d6a8e7a`.
Harness source SHA-256: `51859ae5e97fd225e9e97e8694ae19aa62477e6ad931d7e2fd7066592686d63a`.
