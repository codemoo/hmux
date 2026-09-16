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
  `e3ed7cc7c0897def6e57fb8dca2b2bdb48604037ff5faa023942b60bf1593439`
  (reproduce from this directory with
  `find cmd internal -type f -name '*.go' -print | LC_ALL=C sort | xargs shasum -a 256 | shasum -a 256`)
- Local HMux delta: the public `stream` package links the existing collectors
  into `hmux` and `hmux-agent`, emits bounded sequenced NDJSON snapshots, and
  treats Claude/Codex CLI credentials as a revision-tracked, read-only source.
  `cmd/daemon` remains available for upstream contract coverage but is not
  bundled or launched by HMux. `internal/codexlb` accepts an explicit aggregate
  key. The unused upstream `internal/source` SSH bridge remains excluded: HMux
  uses its own host-key-checked, allowlisted transport.

On a remote client, the existing HMux SSH identity authorizes the exact command
`hmux-agent usage-stream --stdio`. No usage bearer is created. SSH stdin is the
lifetime lease, and bounded TERM/KILL cleanup removes a stalled child. Provider
credentials, codex-lb keys and Home hostnames are not sent. Claude cswap emails
are bounded labels explicitly requested by the owner; Codex uses aliases only.
The transmitted snapshot is a strict HMux-only allowlist and excludes provider
raw response fields.

See `LICENSE` and `NOTICE` in this directory.
