# Releasing HMux

## Current distribution model

This repository publishes source for independent, trusted self-hosted deployments.
It does not provision a shared public server or include access to the maintainer's
machines. Native builds use pinned Ghostty sources and currently use ad-hoc signing.
A source publication is not a notarized macOS application release.

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

## Native binary releases

Before publishing downloadable macOS application bundles:

- Complete the dependency license inventory/SBOM for the statically linked native
  libraries; retain [native notices](../macos/HMux/ThirdPartyNotices.md).
- Build and verify the complete app from the pinned dependency manifest.
- Configure Developer ID signing and notarization with maintainer-owned credentials
  stored outside the repository, then verify the distributed bundle.
- Validate archive paths, checksums and supported CPU/OS; exercise rollback.
- Bump `VERSION` deliberately and create an immutable matching release. Never replace
  an already published artifact with different bytes under the same version.

Until those gates are complete, use the documented local source-build route rather
than describing ad-hoc packages as a signed public release. Release keys and Apple
certificates are not supplied by this repository.

## Source license

A top-level HMux license has not yet been selected. Public source visibility does
not itself grant a general reuse or redistribution license. Existing third-party
licenses continue to apply to their components. Add the maintainer-approved source
license before describing HMux as MIT-licensed or generally reusable open source.
