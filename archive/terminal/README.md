# Archived standalone terminal UI

HMux now centers on one persistent host and remote web/PWA access. This directory
preserves the previous fzf selector, framed tmux terminal interface and shell/UI
configuration for reference and compatibility. It is not a recommended setup path.

- `ui/`: terminal selector and rendering.
- `frame/`: legacy framed tmux client.
- `scripts/`: explicit shell entrypoint and managed UI migration helpers.
- `config/`: retired terminal configuration templates.
- `tests/`: isolated regression checks for this interface.

Shared SSH/tmux/session services stay in the root `internal/` packages. `cmd/hmux/`
also serves the optional native app, so it remains outside the archive. Its legacy
commands and the agent's compatibility selector import these archived Go packages.
The archive remains in the root module and is covered by Go tests; it is not a
second copy of the implementation. Run `make legacy-check` for the historical
integration suite and [CLI reference](../../docs/CLI_COMPATIBILITY.md).

Do not run tests against an existing user tmux server. Live tests must create
isolated sockets and disposable `hmux-e2e-*` resources only. Old shell cleanup
signatures are intentionally preserved to recognize installed legacy includes.

The normal `scripts/bootstrap-macos.sh` installs host/runtime tools without this
UI. Only an explicit `HMUX_INSTALL_LEGACY_UI=1` opts into its managed UI setup.
