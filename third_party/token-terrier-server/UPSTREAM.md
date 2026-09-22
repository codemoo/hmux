# Embedded HMux usage collector

This directory is an in-tree fork/snapshot of `token-run/server-go`, embedded so
HMux can display Claude and Codex/codex-lb usage without a separate menu-bar
application, daemon installation, fixed port, or launch agent.

- Source repository revision at snapshot time:
  `9cb4123338a3f18cd84303834997a8c95e452e21`
- Snapshot date: 2026-08-24
- The revision is a provenance reference, not a claim that these files match that
  commit byte-for-byte. The snapshot included local collector changes; this
  repository tracks the imported code and all subsequent changes independently.
- Go source digest (`cmd/**/*.go` and `internal/**/*.go`, sorted):
  `c2178feacdb932e58cbf9d7aff47fdd50a7c915fdecaf09a609fbcf48234d0d2`
  (reproduce from this directory with
  `find cmd internal -type f -name '*.go' -print | LC_ALL=C sort | xargs shasum -a 256 | shasum -a 256`)
- Local HMux delta: the public `stream` package links the existing collectors
  into the `hmux-web` Home connector, emits bounded sequenced NDJSON snapshots, and
  treats Claude/Codex CLI credentials as a revision-tracked, read-only source.
  `cmd/daemon` remains available for upstream contract coverage but is not
  bundled or launched by HMux. `internal/codexlb` accepts an explicit aggregate
  key. The source-aware stream keeps CLI, claude-swap and codex-lb quota values
  separate. Its claude-swap reader runs only `cswap list --json` on a bounded
  cadence with bounded output and discarded diagnostics. The unused upstream
  `internal/source` SSH bridge remains upstream test/support code only; HMux does
  not import or launch it. Home invokes `stream.RunWithSources` in process.

One source-aware collector serves the connected browsers through HMux's existing
bounded, authenticated Home WSS channel. Context cancellation and pipe closure
end collection when the connector disconnects. No separate usage bearer or SSH
process is created. Provider credentials, codex-lb keys and Home hostnames are not
sent. Claude cswap emails are bounded labels explicitly requested by the owner;
Codex uses aliases only. Snapshots allowlist usage fields and exclude raw responses.

See `LICENSE` and `NOTICE` in this directory.
