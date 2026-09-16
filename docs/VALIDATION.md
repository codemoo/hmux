# Validation

This page records checks for the public source tree. Older implementation and
private-deployment checkpoints are preserved in
[the historical validation log](archive/VALIDATION_PRE_PUBLICATION.md).
They are not claims about a user's independent deployment.

## 2026-09-16 — initial public-source preparation

The repository now presents HMux as one persistent host for Codex, Claude and
shell sessions, accessed primarily through the web/PWA client. The old standalone
terminal packages/configuration/tests are under `archive/terminal/`; the optional
native app remains under `macos/`. Shared host services and native bridge entrypoints
remain buildable. The module path is `github.com/codemoo/hmux`.

Publication excludes private settings, caches, binaries, traces and local screenshots.
The first commits import the current implementation by component without inventing
past development commits. Direct runtime dependency and bundled-font notices are
included; the native static-library SBOM/signing gates remain open.

Checks completed locally:

- Full Go tests, race tests and vet; in-tree collector tests.
- Go formatting, ShellCheck and shell formatting.
- Web type/style checks, 92 tests, production build and deployment archive build.
  The archive contains the gateway, frontend and direct runtime license notices.
- Native smoke checks: models, catalog/workspace changes, surfaces/input/search,
  conversation, catalog stream, usage, metrics, files, helper lifetime and restart.
  The loopback catalog test required execution outside the restricted sandbox.
- Synthetic provisioning/bootstrap and archived frame/config/keybinding/shell/font
  checks passed. No existing user tmux sessions were used.
- Candidate-file and independent publication reviews found no live credentials or
  private deployment identifiers. Generated caches, bytecode and binaries are ignored.
  Markdown entrypoint links and staged whitespace checks pass.

Existing stale/permission-restricted local dependency caches failed the first runs;
clean task-specific Go/npm caches resolved them without changing dependency versions.
No full native app build, notarization or complete legacy live integration suite was
performed for this source reorganization. Historical device acceptance remains
separate from these checks. GitHub CI results are visible on the repository Actions tab.

Source publication does not redeploy the running web service or restart existing
terminal sessions.

### Public CI follow-up

The first macOS CI run used system LibreSSL, which lacks the Ed25519 verification
option. CI now installs OpenSSL 3 explicitly. Core Go tests/vet/race and web checks
passed on GitHub. The optional native surface-deck contract failed on macOS 14 even
after replacing a fixed 20ms sleep with a bounded condition wait. Native CI targets
the locally validated macOS 15 baseline; this is not a claim that macOS 14 is fixed.
