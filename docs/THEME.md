# Flexoki Dark

HMux uses the palette from [Omarchy Flexoki Dark](https://github.com/euandeas/omarchy-flexoki-dark-theme),
pinned at `aa418ffb68ec1eaa72a85b02970ada07ce6f3d26`. The theme is dark even
when macOS uses Light appearance. Native macOS window controls and interaction
remain intact.

| Role | Color |
| --- | --- |
| Terminal and window base | `#100F0F` |
| Sidebar, titlebar and dialogs | `#1C1B1A` |
| Raised surfaces and dividers | `#282726` |
| Hover | `#343331` |
| Selected row / terminal selection | `#403E3C` |
| Primary text and terminal cursor | `#CECDC3` |
| Selection accent and navigation | `#4385BE` |
| Connected / running | `#879A39` |
| Waiting | `#D0A215` |
| Failure | `#D14D41` |
| Completed | `#3AA99F` |

The native semantic palette lives in
[HMuxDesign.swift](../macos/HMux/Overlay/Sources/HMux/HMuxDesign.swift).
Supporting text uses lighter Flexoki base shades (`#B7B5AC`, `#9F9D96`) for
small macOS labels. Native sheets and popovers share the dark appearance,
foreground and accent. The quick switcher has an opaque themed background.

[HMuxGhostty.config](../macos/HMux/HMuxGhostty.config) pins all 16 ANSI colors,
foreground/background, cursor and selection directly from `colors.toml`. It
is bundled independently of personal Ghostty preferences. Runtime terminal OSC
color changes remain supported; HMux's backing surface follows them.

The compatibility [Ghostty config](../archive/terminal/config/ghostty.ghostty), tmux frame and
fzf selector use the same ink/paper colors and blue navigation accent. Their
managed installation preserves personal overrides and backups. Changing source
files alone does not rewrite the owner's installed Ghostty or tmux settings.

The [app icon](../macos/HMux/Brand/README.md) uses the connected terminal H,
matte charcoal, warm paper and blue cursor. Its ten native sizes retain real
transparency. The Bedlington animation remains a template tinted with the
primary text color. Theme attribution is included in
[ThirdPartyNotices.md](../macos/HMux/ThirdPartyNotices.md).
