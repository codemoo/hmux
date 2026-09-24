# Browser input and PWA

Scoped reference for the [web/PWA interface](WEB.md). Repository and web input
contracts remain authoritative; device evidence is distinct from synthetic checks.

## Mobile/PWA invariants

Android: Chrome is the recommended browser; the user confirmed both correct
terminal colors and successful PWA installation on 2026-09-09. Use Chrome's
menu → Add to Home Screen → Install, or HMux's login/settings install button.
iOS: Safari → Share → Add to Home Screen (Open as Web App when shown).
Edge can expose the install button but has not received device acceptance here.
Manifest PNG icons are
192/512px and Apple touch icon 180px. Standalone/browser logins may use separate
storage. PWA updates do not forcibly reload an active terminal. Network is required.

Samsung Internet's home-screen app showed unusual backgrounds on digits and
symbols; the same issue was absent in Chrome. Samsung Internet remains unverified
after the color-preservation mitigation; do not report it as fixed. See
[web troubleshooting](TROUBLESHOOTING.md#webpwa-installation-and-terminal-colors).

Terminal colors live in `theme.ts`. Bold text does not automatically promote ANSI
colors to bright variants. Settings → 터미널 offers HMux Dark (default), Tokyo
Night Storm, Catppuccin Mocha, Dracula and Nord. The choice is device-local,
validated on load and applied immediately to every existing/future terminal,
without reconnecting or clearing output. Chrome stays neutral/dark independently
of the terminal palette. Settings categories support keyboard navigation and
keep account requests tied to the original dialog. Pinned GitHub sources and
licenses are in `web/public/licenses/terminal-themes-NOTICE.md`.

Extended indices 22/52 use muted teal/red fills for
observed tmux diff fills; other extended colors remain standard. Palette changes
affect foregrounds and backgrounds; explicit truecolor output is unchanged.
`color-scheme: only dark` opts out of user-agent auto-dark recoloring. Only xterm
uses `forced-color-adjust: none` to preserve ANSI semantics; app controls remain
eligible for accessibility color adaptation. Vendor overrides may behave differently.

Monatendard Nerd Font Mono Regular/Bold are bundled for the web terminal, with
licenses and all 11,172 modern Hangul syllables. Load both faces before measuring;
Android explicitly registers FontFaces with WOFF2 then TTF fallback. Resume/network
recovery retries loading and redraws terminals. Mobile font defaults to 10px,
desktop 14px, both adjustable 8–24px with separate preferences. The self-hosted
Pretendard Variable v1.3.9 font covers proportional UI and Korean labels; code
blocks and terminal previews use Monatendard. Regular/Bold terminal metrics
are identical, with modern Hangul syllables exactly twice the ASCII cell width.
The monochrome bracket/H SVG mark also supplies the PWA PNG icons; versioned icon paths preserve
the manifest identity. Existing installed launchers may refresh icons on their
own schedule; the application never reinstalls or reloads an active terminal.

`viewport.ts` owns visualViewport height/offset and safe-area geometry. Reserve
insets once, retain top inset during keyboard transitions, reset on rotation, and
subtract already-excluded bottom space. The keyboard-closed workspace reduces the
remaining bottom inset by 12px; login/drawers/dialogs retain the full inset.
Android resize-content shrinks both viewports, so keyboard detection tracks the
expanded height per orientation. Never subtract keyboard height a second time.

Keyboard-visible layout hides normal tabs/footer and exposes a top-right floating
tab button. Auxiliary keys stay compact; pinch zoom is disabled. Remeasure on tab,
dialog, focus, viewport and resume transitions, including settled measurements.

Desktop macOS Safari uses the native-input bridge for the physical Safari 18.6
Hangul trace: `insertText` arrives before keydown229 and selected
`insertReplacementText` updates the local run. The browser owns syllable changes,
including no-op replacements, resyllabification and Backspace. An inline preview
remains local until space, Enter, navigation, a shortcut or paste flushes it once.
Pending text and its short-lived echo preview use the cursor cell's background
and foreground, including RGB, indexed theme colors and inverse video. Colors
refresh on terminal redraw, so black and gray Codex input rows keep their own fill.
This shared behavior also applies to the iPhone bridge.
Modifiers alone do not flush. No remote delete/replace or Hangul reassembly is used.
Desktop textarea geometry and mouse selection remain stock; standard composition
stays with xterm. On 2026-09-10 the user confirmed the deployed Mac Safari input
works and accepted the current behavior ("잘된다. 이정도면 괜찮음."). Automated
regression checks and this user confirmation are separate evidence; no additional
Mac input acceptance is pending. The diagnostic page retains its stock desktop
baseline for comparison.

Android retains stock xterm 6.0.0 input. iOS uses the same native xterm textarea
with a targeted bridge for the physically observed keyCode-0 Hangul path: Safari
updates syllables with deleteContentBackward + insertText and no composition
events, while stock xterm sends raw jamo from keypress. The bridge leaves native
DOM editing intact, shows the pending Korean run inside the terminal and commits
it once at a boundary (space, Enter, navigation/control, paste or active-view blur).
Backspace during that run edits the native value; it is not sent as remote repair.
There is no separate editor, Hangul library, syllable-tail reassembly or NFC rewrite.
Standard composition events use xterm. Safe view transitions flush to the original
live connection first; unexpected disconnection cancels unfinished input so it
cannot leak across reconnects/tabs. The user accepted the current Korean input,
deletion and space-to-echo behavior. See [iOS terminal input](IOS_INPUT.md).

Mobile browser-native selection on rendered rows remains enabled. Long press and
selection-handle drags are excluded from tmux touch scrolling; a native selection
is not replaced by xterm's internal selection. On iOS the existing cursor-positioned
textarea is exposed for native long-press hit testing, with a small touch area and
native callouts. Its gestures bypass desktop selection/refocus/scroll handlers
without preventing browser defaults or rewriting its value. Paste still uses
xterm's clipboard path, flushing pending Hangul first. No second input, clipboard
dialog or custom menu is used. The user confirmed native Paste works; its slightly
slow first menu appearance was accepted without further tuning.
On rendered text, a touch context menu is kept native before its range appears;
long-press/drag clicks do not refocus the input. Only a new short tap resumes it.
On iPhone, selecting output still dismisses the keyboard. Selection/Copy with the
keyboard kept open is not supported by the accepted behavior. The user chose to
keep this behavior and stop that work; no additional confirmation is pending.

Android keeps its top-left input anchor when opening the keyboard. Once the
textarea is focused and the keyboard is visible, `android-native-paste.ts` exposes
that same textarea across the current input row through a visual transform for
native long-press Paste. Blur/keyboard dismissal restores pinned geometry. iOS and Android share
editable gesture protection; Android's xterm input/composition/paste handlers are
unchanged. The user reported that the full-row target appears to work in Chrome
PWA and chose to keep it. This is initial feedback, not exhaustive device coverage.
Inactive-host protections remain. iOS input positioning follows xterm normally.
Tab/dialog transitions do
not automatically focus Android input. Touch scrolling uses local history or tmux
wheel events. Preserve these accepted input/viewport behaviors when maintaining
the clipboard path.

## Accepted mobile behavior

| Platform | Current behavior | Limits / decision |
| --- | --- | --- |
| iPhone | Inline native Korean editing, smooth space handoff, native long-press Paste | User accepted for current use; first Paste menu can be slightly slow |
| iPhone output selection | Browser-native selection/Copy | Keyboard dismisses; user accepted this and stopped keyboard-open selection work |
| Android Chrome PWA | Default xterm input; long press on the current input row with keyboard open | Full-row Paste target appears to work per initial user feedback; keep current behavior |
| Samsung Internet | Color-preservation mitigation exists | Token-background issue remains unverified; Chrome is the confirmed path |

The input caret area on iPhone and current input row on Android expose the real
editable control. Output elsewhere remains browser-selectable terminal text.
No custom clipboard menu or silent clipboard access is used. See
[mobile input troubleshooting](TROUBLESHOOTING.md#mobile-input-and-native-clipboard)
for regressions and [current evidence](VALIDATION.md)
for build/deployment details.

## Source ownership

| File/module | Responsibility |
| --- | --- |
| `web/src/main.ts` | UI composition, tab/connection lifecycle, API and workspace coordination |
| `dom.ts`, `icons.ts` | Typed text-only DOM construction and fixed local SVG icons |
| `conversation-view.ts`, `markdown.ts` | Markdown conversation display, question/code filters and return controls; request/epoch ownership stays in `main.ts` |
| `native-input-preview.ts` | Screen-bounded native pending/echo text and local caret; no input transaction logic |
| `usage-view.ts` | Usage footer, account gauges and usage dialog; quota interpretation stays in `usage.ts` |
| `account-security.ts`, `login-sessions.ts` | Account settings and login-session dialogs with abort/disposal ownership |
| `viewport.ts`, `mobile.ts` | Viewport/keyboard state, font preference bounds |
| xterm 6.0.0 / `ios-native-input.ts`, `ios-native-input.css` | Default input; iPhone native Hangul transaction, echo preview and editable Paste target |
| `native-clipboard.ts` | Native rendered-text selection/Copy and touch gesture ownership |
| `android-native-paste.ts` | Keyboard-visible Android input-row Paste target; pinned keyboard-opening anchor |
| `fonts.ts` | Font loading/registration and family selection |
| `pwa.ts`, `public/sw.js` | Install UI and network-only navigation fallback |
| `terminal-scroll.ts`, `terminal-session.ts` | Touch scroll routing and generation-safe view release |
| `shared-workspace.ts`, `preferences.ts` | Validated workspace shape and guarded local storage |
| `usage.ts`, `types.ts`, `theme.ts` | Usage formatting, data types and selectable terminal palettes |
| `style.css` → `ios-native-input.css` → `chrome.css` → `dialogs.css` | Base/viewport → iOS input → workspace UI → dialog overrides |
| `crates/hmux-gateway` | Auth, bounded gateway/Home protocol, PTYs and account profiles |
| `crates/hmux-home/src/metrics.rs` | Home platform resource collection, including disk allocation |
| `provider-settings.ts`, `crates/hmux-home/src/providers.rs` | Settings → AI 연결: provider CLI status, connect/update jobs and API keys |

See [web maintenance instructions](../web/AGENTS.md). Preserve CSS order and edit
owning rules rather than appending a new conflicting override.
