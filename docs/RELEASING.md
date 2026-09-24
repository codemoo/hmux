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
connector. `deploy/web/build.sh` builds the web assets and Go binaries. Keep account
and session stores outside the release directory. Stage and verify a new release,
retain the previous release, then switch the service. Check authentication, CSP,
static assets and Home connectivity after activation. Publishing source does not
update running installations automatically.

The native Rust runtime has separate `make rust-package` bundles. Follow
[Rust verification](../tests/RUST.md#test-entry-point) before using a new artifact;
[migration status](RUST_MIGRATION.md) and [validation](VALIDATION.md#rust-transition)
distinguish maintained trials from full release acceptance. Publishing the Rust
source does not switch default builds, retire Go or complete device/soak gates.

## Source license

A top-level HMux license has not yet been selected. Public source visibility does
not itself grant a general reuse or redistribution license. Existing third-party
licenses continue to apply to their components. Add the maintainer-approved source
license before describing HMux as MIT-licensed or generally reusable open source.
