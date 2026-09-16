# Optional HMux macOS client

The primary HMux experience is the [web/PWA client](../README.md). This directory
retains a separate native SwiftUI/AppKit client with an embedded Ghostty terminal.
It uses the same host services and session identity rules as the web client.

- [Native development and behavior](HMux/README.md)
- [Pinned dependencies](HMux/Dependencies.lock.json)
- [Native third-party notices](HMux/ThirdPartyNotices.md)
- [Build/install operations](../docs/OPERATIONS.md)

Build from source on an Apple Silicon Mac with the documented Xcode/toolchain.
`make native-smoke` checks native contracts. The Go bridge remains at `cmd/hmux/`;
shared host services stay in `internal/`. Do not move or duplicate those services
into the UI. The old standalone terminal interface is in `archive/terminal/`.

Current local packages are ad-hoc signed. Complete the dependency notice/SBOM,
Developer ID and notarization gates before publishing a downloadable app release.
The web source publication does not claim those native release gates are complete.
