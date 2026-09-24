# Releasing HMux

## Current distribution model

This repository publishes source for independent, trusted self-hosted deployments.
It does not provision a shared public server or include access to the maintainer's
machines. The only distributed UI is the web/PWA app; macOS binaries are Home
host services. There is no separate desktop bundle or client updater.

## Source publication

1. Run the checks in [CONTRIBUTING.md](../CONTRIBUTING.md) and inspect the staged diff.
2. Exclude local caches, build outputs, credentials, private topology and transcripts.
3. Retain all third-party notices and font licenses. Review additions to bundled assets.
4. Use focused commits. Push source only after local gates pass; verify CI on GitHub.
5. Document material limitations and migration steps in the relevant reference.

The first publication imports the current implementation by component. No earlier
commit chronology is reconstructed. The existing `VERSION` is retained and is not
an assertion that a matching GitHub binary release exists.

## Web deployments

Follow [WEB.md](WEB.md), using your own domain, HTTPS gateway, credentials and Home
connector. `make build` packages web assets and the Rust Gateway/Home/helper into
`dist/web-<platform>/` and `dist/hmux-web-<platform>.tar.gz`. Build a supported
non-host target only with an explicitly configured Rust target, linker and SDK.
Keep account and session stores outside the release directory. Stage and verify a
new release, retain the previous release, then switch the service. Check the SHA-256
manifest, authentication, CSP, static assets and Home connectivity after activation.
Publishing source does not update running installations automatically.

Run [Rust verification](../tests/RUST.md#test-entry-point), including `make bundle-check`
with the selected `HMUX_RUST_BUNDLE`, before using a new artifact. The project is
Rust-only, but dated trial evidence and the outstanding device/soak limitations in
[migration status](RUST_MIGRATION.md) remain limitations; retirement does not make
them passed release gates.

## Source license

A top-level HMux license has not yet been selected. Public source visibility does
not itself grant a general reuse or redistribution license. Existing third-party
licenses continue to apply to their components. Add the maintainer-approved source
license before describing HMux as MIT-licensed or generally reusable open source.
