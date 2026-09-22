# Historical web UI validation

Superseded experiment log, not current instructions. See [Web HMux](../WEB.md).

## Web font and sidebar follow-up (2026-09-09)

Bundled JetBrains Mono Nerd Font Regular/Bold WOFF2, replacing a nonexistent
font-family reference. FontTools decoded both assets and verified equal ASCII
advance widths plus box drawing, Powerline and Nerd Font glyphs. Both deployed
HTTPS assets returned the exact local bytes with `font/woff2`. Hangul remains a
system CJK fallback. Browser rendering is not proven by these file checks.

Added desktop sidebar collapse with device preference, mobile inert drawer,
Alt+Shift+Left/Right tab selection and Alt+Shift+B sidebar toggle. The xterm key
handler consumes these chords before terminal input; browser tab/history chords
and composing input are not intercepted. Build/type/format checks, nine frontend
tests and Go gateway tests passed. Playwright was attempted but automatic approval
review again returned 503; interactive browser verification remains outstanding.

## Monatendard and iOS/mobile follow-up (2026-09-09)

The authoritative custom font setting is `config/ghostty.ghostty`: Monatendard
Nerd Font Mono. The web font assets are bundled independently.
Replaced the web JetBrains assets with lossless Regular/Bold WOFF2 conversions
of the installed Monatendard fonts and included upstream licenses/notices.
FontTools verified all 11,172 modern Hangul syllables and exactly two ASCII cell
widths per Hangul syllable. Both deployed HTTPS font files matched local bytes.

Mobile default font is 12px with a separate preference; desktop remains 14px.
Added visualViewport height/offset layout, fixed mobile chrome, retained-terminal
resize, viewport/touch/gesture pinch guards, and an iOS composition adapter that
holds intermediate Jamo, previews local composition and sends NFC committed text.
Build/type/format checks and 11 frontend tests passed. The composition sequence
test covers intermediate syllables, final-input ordering, Enter flush and disposal.
These are synthetic event tests; real iOS IME, keyboard geometry and pinch behavior
have not yet been verified on a physical device. The web assets were deployed and
public font/index responses were checked; no host binary update was performed for
this web-only follow-up.


## iOS input and compact keyboard follow-up

User testing found that the prior composition adapter did not resolve Jamo
separation. It was removed. iOS now uses a native draft editor and submits the
completed string through the terminal wire boundary, avoiding xterm textarea
mutation while the keyboard composes. Tests verify whole-draft NFC and bracketed
paste/Enter framing; these are not physical-iPhone acceptance results.
Mobile defaults to 10px, allows 8px, and keyboard-visible layout removes the
status/usage rows and bottom padding while reducing tabs and auxiliary keys.


## Additional web account profiles

Account/profile tests passed under race detection: separate login identities,
independent TOTP counters including restart replay rejection, separate persistent
tab layouts, shared Home catalog access, rejected browser profile injection and
logout isolation. Existing gateway/authentication tests and shared-workspace race
tests also passed. Additional profiles reuse the common merge store on the gateway;
the primary Home workspace remains unchanged.


## PWA and conversation opening (2026-09-09)

Added standalone manifest, native HMux PNG icons, Apple home-screen metadata, and
installation controls in login/settings. Automated tests verify icon dimensions,
manifest identity, API bypass and anonymous no-store offline navigation. The worker
uses no cache storage. Frontend type/format checks, all 16 frontend tests and the
production build pass. Physical-device installation remains to be checked.
Conversation initial loading scrolls to the latest message, guarded against stale
web requests. The native conversation smoke/typecheck also passes; that source
change has not been included in a new installed macOS build in this follow-up.


## iPhone PWA safe-area follow-up (2026-09-09)

Moved safe-area ownership from the disappearing tabbar/footer to the mobile app
shell. Top padding accounts for visualViewport offset; bottom padding is omitted
while the keyboard is visible. Login, fixed sidebar, floating tabs and dialogs
now use protected bounds, including narrow-height touch landscape layouts.
Frontend type/format checks, 16 tests and production build passed. Browser visual
verification was attempted but automatic approval review disconnected and rejected
Playwright execution. Physical iPhone PWA verification remains outstanding.


## PWA font/resume and keyboard inset correction

Removed subtraction of viewport pan offset from the top safe area. Retain the
physical inset through keyboard transitions, resetting on orientation changes.
Added font preloads and font loading/redraw on resume and network recovery.
Type/format checks, 16 frontend tests and production build passed. These checks
do not reproduce physical iPhone font rendering or keyboard geometry; device
acceptance remains outstanding.


## Bottom inset after keyboard dismissal

Calculate only the remaining bottom inset after visualViewport exclusions.
Regression cases cover full, partial and absent viewport exclusions, viewport
translation, keyboard reduction, and devices without bottom insets. All 17
frontend tests, type/format checks and production build pass. Physical iPhone
keyboard-dismissal appearance still needs user verification.


## Compact keyboard-closed footer

User reported the viewport-exclusion adjustment did not reduce the gap. The
workspace now explicitly reduces the residual inset by 12px (minimum zero),
with a 24px minimum footer and 4px vertical padding. Full insets remain on login,
sidebar and dialogs. Build, type/format checks and 17 tests passed; actual device
appearance and home-indicator clearance remain to be confirmed.


## Android focus and font loading

Suppressed automatic focus on tab selection and connection readiness for Android.
The native xterm input path is unchanged. Both WOFF2 files passed OpenType Sanitizer;
TTF compatibility copies retain all 11,172 Hangul syllables. Android explicitly
loads/registers both weights with WOFF2/TTF sources before terminal creation.
Type/format checks, 17 frontend tests and production build passed. Actual Android
keyboard and font rendering acceptance remains outstanding.


## Android keyboard layout detection

Added orientation-scoped expanded viewport tracking for resize-content keyboards.
Regression coverage checks opening, floating-control focus, Back dismissal while
input retains focus, browser toolbar changes and orientation reset. All 18 frontend
tests, type/format checks and production build pass. Physical Android keyboard
geometry still requires device verification.


## Transition remeasurement and typing surfaces

Added coalesced frame and settled remeasurement on viewport, focus, tab, sidebar,
dialog and app-resume transitions. Measurements target the current connected
terminal and skip hidden reader views. Default block cursor becomes a bar; helper
textarea and composition/native editor colors are explicit. Type/format checks,
18 tests and build passed. Physical device transition/typing appearance is not
verified by these checks.


## Android tab-to-keyboard transition follow-up

Hidden Android terminal textareas are blurred on tab selection so they cannot
remain the keyboard scroll anchor. Geometry is re-read at 80/200/450/800ms after
layout events, in addition to the next frame, and app bounds are observed along
with stage bounds. Build, type/format and 18 frontend tests passed. The reported
physical-device height issue remains pending user acceptance.


## Android hidden input anchor

xterm CoreBrowserTerminal._syncTextArea positions its invisible input at buffer
cursor coordinates and CompositionHelper also updates that position. Android-only
CSS now pins this focus anchor at the terminal top-left; hidden tabs are inert.
This avoids using an old cursor row as the browser keyboard scroll target, without
replacing xterm composition/input handlers. Type/format checks, 18 tests and build
passed. Physical tab-switch/keyboard acceptance remains outstanding.


## Web application chrome refresh

Reworked component styling with stable compact tabs, runtime/session badges,
connection-state marker, grouped settings with live font preview/reset, reader
controls and startup loading content. Shared workspace identity and Android input
handlers are unchanged. Type/format and the 18 existing frontend tests pass; they
do not establish visual acceptance. Playwright launch was rejected by automatic
approval review with an upstream 503; desktop/mobile visual testing remains
outstanding. A separate read-only reviewer checked mobile layout invariants. Its follow-up
review hit an account usage limit; the main agent reviewed final integration.
