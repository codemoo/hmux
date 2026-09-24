# Token Terrier attribution

Only provenance and license notices remain in this directory. The native usage
collector is implemented in Rust at `crates/hmux-usage` and integrated into
`crates/hmux-home`; no Go collector, sidecar or Go toolchain is distributed.

The original imported snapshot came from `token-run/server-go`, revision
`9cb4123338a3f18cd84303834997a8c95e452e21`, on 2026-08-24, with local collector
changes. The original Go source digest (`cmd/**/*.go` and `internal/**/*.go`,
sorted) was `c2178feacdb932e58cbf9d7aff47fdd50a7c915fdecaf09a609fbcf48234d0d2`.
The final HMux Go collector and migration oracles remain in Git at
`c061f28fe7ea8e865578ac1189240447d0ebaa6f`.

Retain `LICENSE` and `NOTICE` for derived Rust parsers, models and fixtures.
