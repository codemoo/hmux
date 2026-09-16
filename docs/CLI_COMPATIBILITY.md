# Archived CLI compatibility

The primary product is the HMux web/PWA client; the native app is optional. This page describes the retained
Go/fzf interface and its optional standalone Ghostty frame.

Use an explicit executable to avoid personal shell functions named hmux:

```bash
HMUX_HELPER="$HOME/Applications/HMux.app/Contents/Helpers/hmux"
"$HMUX_HELPER" --no-update-check ls
"$HMUX_HELPER" --no-update-check new shell
"$HMUX_HELPER" --no-update-check workflow --json
```

The installed `~/.local/bin/hmux` bootstrap selects the validated Go runtime.
`archive/terminal/scripts/hmux-entrypoint.sh` is an explicit compatibility launcher and Home
source-runtime maintenance entrypoint. It can install prerequisites and sync
managed UI fragments; it is not a shell-startup requirement.

## Selector

The fzf selector searches safe metadata and shows runtime, model, state,
working duration and workspace. Search uses contiguous terms; the native
app's search supports token-wise fuzzy matching.

| Key | CLI selector action |
| --- | --- |
| / | Reveal/focus search |
| Esc | Cancel search/input mode; remain in launcher navigation |
| Ctrl-N | Inline create: profile and optional session name |
| Ctrl-R | Change display alias; empty input clears it |
| Ctrl-X | Confirm actual session termination |
| Ctrl-Q | Exit launcher |
| Enter | Open selected session |

Visible rows omit hidden metadata. The three-second refresh suppresses
identical snapshots and preserves the stable-ID cursor. A missing lifecycle
timestamp is shown as unknown, not an invented working duration.
Mobile selection uses a narrow remote UI and continues in the same TTY.

Direct attach is a handoff on the selected original session; `--shared`
allows simultaneous clients. Home calls from inside tmux use switch-client.

## Optional standalone Ghostty frame

Two disposable tmux UI servers own the header/tabs, pane edges and helper
footer. Neither owns user work. Normal resize propagation reaches the target
PTY. The target server receives no HMux UI options or keys.

The framed target attach is always shared. Its PTY proxy handles only fixed
private Ghostty sequences; ordinary input and native Ctrl-b pass through.

| Key | Framed CLI action |
| --- | --- |
| Cmd-L / Cmd-Backquote | Return to persistent selector |
| Cmd-1–Cmd-9 | Select launcher-local tab |
| Cmd-R | Edit display alias |
| Cmd-W | Close visual tab |
| Cmd-Q | Exit launcher |

Closing/returning stops the disposable client, never another tmux client.
Plain primary drag copies through the disposable outer layer and OSC 52.
These bindings and frame servers do not apply to native HMux surfaces.

## Configuration and maintenance

`archive/terminal/config/frame.tmux.conf`, `archive/terminal/config/frame-ui.tmux.conf` and
`archive/terminal/config/ghostty.ghostty` serve only this compatibility interface.
`archive/terminal/config/tmux.conf` is a retired no-op marker.

Run `archive/terminal/scripts/sync-ui-config-macos.sh` explicitly to update fragments. It backs
up changes, rejects unsafe paths and removes retired target-tmux and shell
autostart includes without reloading a live server. The native app instead
uses bundled `HMuxGhostty.config`.

If the optional font is missing, use `scripts/install-monatendard.sh --check`
and then the explicit installer. A standalone Ghostty app and fzf are
compatibility prerequisites; they are not prerequisites for native rendering.

For an old installation containing target-server HMux options, use the
reviewed migration in [Migration](MIGRATION.md). Never source the retired
fragment or add target bindings to repair a frame problem.
