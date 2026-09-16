# HMux app icon

The master [HMuxIcon.png](HMuxIcon.png) is a 1024 × 1024 RGBA PNG.
The connected terminal-pane H keeps the app recognizable. A matte charcoal
tile, warm paper mark and blue cursor follow the Flexoki Dark palette, with
restrained relief and even transparent margins for the macOS silhouette.

The ten macOS slots in
[Ghostty.appiconset](../Overlay/Assets.xcassets/Ghostty.appiconset/Contents.json)
are resampled from the master with `sips`, preserving alpha. The upstream asset
name remains the build contract; the artwork is HMux's own.

Updated with the built-in image tool on 2026-09-08. A dedicated background
extraction edit removed the generated checkerboard; actual alpha was checked
before resampling. No image API key or fallback CLI was used.

## Refinement prompt

> Use case: logo-brand, precise-object-edit. Refine the supplied HMux icon into a sophisticated native macOS icon while preserving recognition of the connected dual-terminal H monogram. Make it quieter, cleaner, more expertly proportioned: balanced monogram of two vertical round-corner terminal panes joined at center, with moderately slimmer strokes than reference; generous internal negative space; a tiny blue square cursor in lower right pane. Very subtle shallow emboss, matte warm ink charcoal #100F0F to #1C1B1A body, warm paper #CECDC3 monogram, muted Flexoki blue #4385BE cursor. Standard macOS rounded-square silhouette with even margins. Remove the thick shiny metallic outline and excessive plastic gloss; just a delicate warm edge highlights the silhouette. No text or decorative additions. Crucial: deliver actual RGBA transparency outside the icon silhouette, NOT any rendered checkered background, gray background, white background, or black background. The checkerboard from image viewers is NOT part of the artwork and must not be drawn. 1024x1024 square PNG icon with real alpha; center it with about 6% empty margin. This is a production app icon, not a mockup.

## Alpha correction prompt

> Use case: background-extraction. Edit target: the supplied refined HMux app icon. Remove the grey checkerboard pattern completely and output a PNG with an actual alpha channel: the exterior around the rounded square must be truly transparent (alpha zero). A checkerboard painted into RGB is NOT acceptable. Keep the warm charcoal rounded tile, paper-colored connected H terminal frames and blue cursor unchanged. Clean precise silhouette with smooth anti-aliasing and minimal transparent shadow. Exactly one square app-icon asset, straight-on, no mockup. Do not add a background color or any checkerboard pixels. This request is specifically to produce transparent-background RGBA PNG output, not an RGB illustration of transparency.
