# iOS terminal input

## Accepted behavior and limits

The user accepted direct Korean input/deletion, the smooth space-to-echo handoff
and native long-press Paste. The first Paste menu can appear slightly slowly;
that small delay was accepted, and its exact cause was not measured.

Selecting terminal output dismisses the iPhone keyboard. Keeping the keyboard open
through selection/Copy was not achieved; the user chose to use the current behavior
and stop further changes. Do not describe this as supported or still awaiting user
acceptance. No separate editor, clipboard dialog or emulated context menu is wanted.

## Current implementation

The user supplied a trusted physical iPhone trace on the default xterm 6.0.0
baseline. It disproves the initial insertReplacementText hypothesis: this device
uses keyCode-0 Hangul keys followed by deleteContentBackward + insertText. Native
textarea values compose correctly; xterm emits raw jamo before beforeinput, through
its keypress path. Backspace is handled by xterm and leaves the native value stale,
which then contaminates subsequent native composition. The pasted trace's wire
Backspace representation was a space, so its exact original control byte is not
inferred. The absence of DOM deletion is directly visible.

`web/src/ios-native-input.ts` keeps the existing native textarea and lets Safari
edit it without canceling native deletion. It prevents premature xterm handling
only for the observed iPhone Hangul path and displays the native pending run at
the terminal cursor. A boundary commits that run once via the public term.input
API before the boundary key/paste. No Hangul reassembly, rolling syllable tail,
remote replacement or separate input field is used. Standard composition is
handled by stock xterm. Android's accepted input/viewport anchor remains.

The pending run and retained echo preview use the underlying cursor cell colors,
including gray input rows, instead of a fixed black fill. This changes presentation
only; native editing and boundary handoff remain the same.

The visual run sits in a screen-sized, paint-contained layer. Only its first line
is indented to the terminal cursor; subsequent lines use the full terminal width.
The preview is clipped to the area from the cursor row to the screen bottom.
Long runs scroll within that local area to keep their newest line visible, without
covering earlier terminal rows or moving the terminal buffer/browser viewport.
Resize also refreshes the existing textarea position, except during standard
composition; the iOS Paste target is capped at the terminal right/bottom edges.
A single local caret replaces the underlying xterm cursor decoration while the run is visible; echo copies never contain a caret.
Scrolling away hides the preview, and deletion, blur, cancellation and standard
composition handoff restore normal cursor presentation. The same layout serves
iOS and macOS Safari. These visual changes do not alter native IME transactions.

The provided sequence is captured in focused regression tests, including native
replacement/deletion, visible previews, boundary ordering, safe view-transition
flush and disconnected-view cancellation. Synthetic tests verify event handling;
user acceptance is recorded separately above.

To remove the reported flash on space, the input bridge retains a visual-only copy
at the original cursor until onRender finds
the same echoed text in terminal cells (including wide cells/wrapping). Unrelated
or partial output does not dismiss it. Wide-glyph wrap padding is skipped when
matching the echo. A 700 ms cap, new composition, blur and
teardown remove stale copies. This does not insert local terminal bytes or resend
input. The user confirmed this visual handoff is clean.

## Rejected experiments

`web/src/diagnostics/baseline-ios-input.ts` preserves the initial direct-input experiment;
`web/src/diagnostics/unaccepted-ios-input.ts` preserves the rejected rolling-tail design.
Neither is imported by production or the current diagnostic page. Their synthetic
unit tests did not reproduce the user's actual WebKit event sequences and do not
constitute acceptance. es-hangul is used only by that research fixture, not the
production input bundle. Both es-hangul and Hangul.js manipulate Hangul strings;
neither replaces Safari's native IME or fixes event ordering automatically.

## Physical-device capture

`/input-diagnostic.html` compares a native textarea and the current terminal
implementation, with no tmux connection. Type the supplied phrases, repeatedly
delete, then type again. Records
include event type/inputType, composing/composed/trusted state, textarea snapshots
before handlers, in a microtask and after xterm's timer, and actual xterm onData
output. Recording is bounded and stays in page memory. “기록 표시·복사” stops
recording and selects JSON in a native read-only textarea for system Copy.
Download explicitly exports JSON; no server upload/storage occurs. A `blob:` URL
is a browser-local
object URL, not a server file. Downloaded reports contain test text and must not be
committed. They can be saved through the phone's Share → Save to Files flow.

Further changes should use actual affected-device reports; generated DOM events
cannot establish the keyboard's real ordering. The diagnostic labels the current
iOS bridge separately from stock xterm and includes keypress/charCode. It does not
install Android's app-specific Paste geometry; use the real Chrome PWA when
checking that path.

## Earlier upstream hypothesis (not adopted)

The user reports jamo separation on the default baseline. xterm 6.0.0
`CoreBrowserTerminal._inputEvent` forwards `insertText` but does not handle
`insertReplacementText`. Upstream [issue 3836](https://github.com/xtermjs/xterm.js/issues/3836)
reports iPhone/iPad Korean separation; [PR 5704](https://github.com/xtermjs/xterm.js/pull/5704)
describes WebKit replacing an initial jamo with composed syllables through
`insertReplacementText`, without standard composition events. At the time of
investigation that PR was open,
unmerged and reported macOS Tauri testing only. The supplied iPhone trace instead
shows deleteContentBackward + insertText, so this PR does not describe the
observed mechanism and was not applied.

References: [xterm.js](https://github.com/xtermjs/xterm.js),
[es-hangul](https://github.com/toss/es-hangul),
[Hangul.js](https://github.com/e-/Hangul.js),
[MDN beforeinput caveats](https://developer.mozilla.org/en-US/docs/Web/API/Element/beforeinput_event).

Historical attempts and deployment checkpoints are in
[the archived input log](archive/IOS_INPUT_HISTORY_2026-09-09.md).
