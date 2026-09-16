# Historical iOS input and clipboard validation

Superseded checkpoint log, not current instructions or acceptance status.
See [current verification](../VALIDATION.md#current-web-verification-2026-09-09)
and [current iOS behavior](../IOS_INPUT.md). Pending/unaccepted statements below
describe each release at that time, not the final user decision.

## Superseded checkpoints (2026-09-09)

[Web HMux](../WEB.md) owns current behavior and operations. Earlier UI experiments
are retained in [historical web validation](WEB_UI_HISTORY_2026-09-09.md).
The dated sections above describe their own checkpoints, not current deployment
status.

### User-confirmed device results

- Android tab-switch/keyboard anchor fix and iOS font/top-safe-area fixes work.
- Android Chrome renders terminal colors correctly. The user confirmed successful
  Chrome PWA installation after the manifest CSP fix.
- Samsung Internet's home-screen app showed unusual digit/symbol backgrounds.
  Its behavior after the color-preservation mitigation remains unconfirmed; use
  Chrome for the verified Android path. Browser recoloring is a suspected cause,
  not an established diagnosis. xterm 6 already prevents whole-cell dim opacity;
  do not revive that disproven explanation as the cause.

### Automated checks and deployment

Frontend format/types, production build and 44 tests passed after the iOS composition-snapshot correction. Coverage includes
workspace, scrolling, input, PWA assets, atomic/retryable fonts, connection release
and usage formatting. Targeted Go tests and race checks passed for the cleanup;
the subsequent PWA CSP regression and gateway tests also passed, with a Linux build.

The live web release is restored to the original direct-input build from
2026-09-09 11:53:37 UTC after the user rejected both deletion follow-ups.
Local source contains the unaccepted deletion candidate and must not be deployed
as a verified fix. The failed releases are retained for diagnosis. Public HTML matches the build, the gateway is active, and
unauthenticated API access returns 401. Document, manifest and worker responses
explicitly allow same-origin manifests/workers while retaining restrictive defaults.
Previously, `default-src 'none'` blocked the manifest despite its HTTP 200 response;
that installation blocker is fixed and user-confirmed.

### iOS direct input and deletion candidate

The user confirmed direct terminal input mostly worked, then reported deletion
replacing earlier characters and removing whole syllables instead of vowels/final
consonants. That report supersedes broad acceptance of the initial direct-input
candidate. The separate editor remains removed; all local composing text is visible
inline, including deletion results and long compositions near viewport edges.

The correction retains native DOM context and holds the final Hangul syllable
locally across compositionend. Native input alone dispatches deletion. es-hangul
2.4.0 handles narrowly detected whole-grapheme deletion of an uncommitted Hangul
suffix, with deferred reseeding and local reassembly; no remote prefix is rewritten.
Connected blur finalizes the visible suffix into its original session before a tab
switch. Disconnected release cancels it. See [iOS input design](../IOS_INPUT.md).

Twenty input state/adapter regression tests now cover event-order variants,
repeated deletion, compound vowels, post-deletion typing, NFD context, retained
prefix safety, selected deletion, single control dispatch, cancellation, connected
blur, grapheme-safe compaction and preview visibility. All 44 web tests, formatting,
types and production build passed. The new dependency has no runtime dependencies;
its MIT license ships with the frontend. npm reported zero known vulnerabilities
at installation. Main/independent source review identified deletion ownership,
rolling-tail, reseeding and compaction concerns; the concrete findings were addressed.

The frontend was deployed without restarting the gateway. Public HTML/JS/CSS and
library license match the build; PWA CSP is preserved. These are simulated event
and deployment checks, not physical WebKit IME validation. The user's iPhone must
still verify repeated deletion/retyping, selection, native keyboard event behavior,
and tab/keyboard transitions before the reported deletion bugs are called resolved.

### Remaining limits

The Home disk sampler and used/total UI are built; the gateway schema is deployed.
The running Home connector has not been updated because local process inspection
was blocked by automatic approval-review infrastructure failures. Disk may remain
unknown until that connector is updated. Do not report live disk collection based
only on frontend or Linux deployment.

Playwright/Chrome automation was unavailable due to browser connection and approval
infrastructure failures. Device results above are user acceptance, not automated
browser tests. New styling/resume paths beyond those confirmations still need
physical-device acceptance. No screenshot request is pending for the Chrome color
issue, which the user confirmed absent.

### iOS composition accumulation regression

The user reported accumulated intermediate jamo after the deletion candidate.
The affected 12:14:44 UTC release was rolled back to 11:53:37 UTC before repair.
A regression test reproduces compositionstart after the native DOM/selection has
already advanced. Concatenating a guessed composition prefix with compositionend
payload incorrectly appends replacement text. The DOM adapter now passes the full
native snapshot for composition update/end; payloads are not treated as append
operations. Two new tests cover repeated starts and replacement-only end sequences.
The corrected frontend was deployed at 12:20:08 UTC, retaining the earlier direct
input release for rollback. Public HTML/JS/CSS match; all 44 web tests and build
passed. Actual iPhone acceptance remains pending; the previous deletion release
must not be described as user-accepted.

### Repeated accumulation report: rolled back again

The user confirmed that the 12:20:08 UTC snapshot change still accumulates jamo.
Restored the 11:53:37 UTC direct-input release (index-B2hjfHPi.js), which the user
had reported as mostly working before the deletion changes. Public HTML was
compared byte-for-byte with that release and the service is active. This restores
the earlier behavior; it does not fix its known deletion bugs. Local source/tests
still describe an unaccepted candidate. Do not redeploy it based on simulated
input tests: the actual iPhone event sequence has not been captured or reproduced.

### Additive input diagnostics (12:26:26 UTC)

An isolated input diagnostic page was deployed while preserving the restored
production index byte-for-byte. Public baseline app identity, diagnostic HTML and
referenced JS/CSS bytes were verified. The diagnostic uses a frozen baseline
adapter, observes native/terminal events and local synthetic wire output, and
exports only when requested. No real tmux session is attached or modified. Existing
44 tests/type/format checks and the multi-entry build passed. This is preparation
for physical iPhone evidence, not another claimed input fix. The latest input
candidate remains unaccepted and was excluded from this deployment.

### Clipboard usability release (12:36:48 UTC)

Added a toolbar clipboard action and mobile Select/Copy and Paste buttons. Copy
uses a native read-only textarea for long-press range selection and preserves soft
wrapped spaces. Paste offers a native editable textarea/clipboard read and explicit
confirmation; it uses xterm framing, a 64 KiB limit and captured tab/generation
checks. No Enter is appended and no original tmux session is manipulated for copy.

Production `ios-input.ts` was restored byte-for-byte from the original mostly
working direct-input source. The rejected candidate and its 20 tests are now
explicitly isolated under diagnostics; passing those tests is not acceptance.
Three clipboard helper tests plus the existing suite pass (47 total), with
format/type checks and build. Public app/diagnostic HTML and referenced assets
match the build, PWA CSP remains valid, and unauthenticated API access returns 401.
The gateway was not restarted. Physical iPhone copy/paste and selection-handle
acceptance is pending. The original iOS deletion bug remains unresolved.

### Native clipboard interaction candidate (12:42:17 UTC)

The user rejected clipboard dialogs as unnatural. Removed the dialogs and toolbar/
auxiliary buttons. Mobile xterm rendered text now permits browser-native selection;
long press/selection-handle movement is not converted into tmux scroll. Browser
Copy is preserved instead of substituted with xterm's internal selection; unselected
contextmenu retains xterm's native textarea Paste path. Intentional short taps still
focus mobile input. Desktop selection and restored iOS input adapter are unchanged.

Three native clipboard event tests and a selection-drag scroll regression replaced
clipboard-dialog helper tests. All 48 tests, format/types and build passed. Public
app HTML/JS/CSS match the deployed release; the gateway remains active. These tests
verify event ownership, not actual iOS selection handles or system Paste availability.
Physical-device acceptance remains pending; do not claim native menu behavior was
visually verified.

### Native Paste target correction (12:46:38 UTC)

User confirmed Copy works only with the keyboard down and Paste still fails. The
prior native clipboard candidate is not accepted for those paths. xterm's desktop
contextmenu helper was still expected to provide mobile Paste, but the input was
pinned away from the cursor and the helper can overwrite IME state. Mobile now
stops that helper without preventing the system menu, and iOS exposes the same
focused textarea as a cursor-sized editable hit target. Its value/selection are not
rewritten by positioning. Native target listeners are disposed with the tab.

All 49 tests, formatting/types and build passed; public app assets match. Physical
iPhone Paste and keyboard-open selection remain unverified. Cursor-target placement
uses a CSS transform; keyboard/viewport behavior needs device regression checking.
Do not claim keyboard-open selection or actual system Paste acceptance from these
event/geometry tests.

### Default xterm input baseline (12:52:37 UTC)

At the user's request, removed the production custom iOS IME adapter and
cursor-sized native input target. Input/composition/deletion/paste now follow
xterm 6.0.0 defaults. The native selection bridge and accepted Android input
anchor/viewport behavior remain. The diagnostic page also uses default xterm;
rejected adapters remain isolated research fixtures and are not bundled.

All 48 automated tests, formatting/type checks and the production build passed.
Deployed frontend release `20260909T125237Z` with a timestamped rollback reference,
without restarting the gateway. Public app and diagnostic HTML and all six
referenced JS/CSS assets match the build byte-for-byte. The service is active,
PWA CSP directives remain present and unauthenticated state access returns 401.

This establishes the requested baseline. Physical iPhone Korean composition,
repeated deletion, keyboard-open selection and native Paste remain unverified;
synthetic tests are not evidence that those reported problems are fixed.

### Default-input jamo separation investigation (13:08:05 UTC)

The user reports jamo separation on the default baseline. Inspected xterm 6.0.0
input/composition source and upstream issue 3836 / unmerged PR 5704. Missing
`insertReplacementText` handling is a candidate cause; no physical-device event
report is available yet. No production input workaround was introduced.

Diagnostic records now include composed/trusted flags and deferred textarea
snapshots after xterm timers. Added a native read-only report selection action so
the user can copy JSON without transferring a downloaded Blob. All 48 tests,
format/type checks and build passed. Deployed `20260909T130805Z` with the app HTML
unchanged, rollback reference preserved and no gateway restart. Public app and
diagnostic HTML and all six referenced assets match; PWA CSP remains present and
unauthenticated API access returns 401. This is diagnostic preparation, not a
verified Korean input fix.

Follow-up diagnostic release `20260909T131024Z` uses two timer tasks for the
deferred snapshot: browser event dispatch may checkpoint microtasks between the
capture listener and xterm's listener. Formatting/type checks and build passed;
both public HTML entries and all six referenced assets match the final build.
Independent source/upstream review found PR 5704 has changes requested and known
preview/duplication defects; it was not adopted. An affected-iPhone trace remains
necessary to distinguish replacement events from standard composition behavior.

### Physical-trace-based iPhone native input correction (13:24:49 UTC)

The user supplied trusted iPhone native and terminal event records. Native Hangul
composition uses keyCode-0 keys and deleteContentBackward + insertText without
composition events. The textarea correctly forms syllables while stock xterm
emits raw jamo before beforeinput. Repeated Backspace leaves the textarea stale;
subsequent typing continues editing old syllables. The earlier replacement-event
upstream hypothesis does not match this report.

Added an iOS-only native transaction bridge using the existing xterm textarea.
It leaves Safari DOM editing/deletion uncancelled, suppresses premature keypress
transmission, shows the pending run inline and commits once before a boundary.
There is no syllable reassembly, remote text repair or additional input field.
Standard composition handoff preserves native DOM. Safe view transitions flush
to the original live session; unexpected disconnect cancels stale pending state.
Android input and viewport handling remain unchanged. The diagnostic uses the
same bridge and now records keypress/charCode.

Independent review identified standard-composition handoff, keydown-less boundary
and lifecycle defects; those were addressed with focused regressions. All 56
tests, formatting/type checks and build passed. EventTarget tests model the
provided sequence and boundary behavior; they do not prove real DOM capture or
physical-device acceptance. Playwright launch was blocked by sandbox cache access,
then automatic approval review rejected escalation because its upstream stream
disconnected. No browser validation is claimed and the rejection was not bypassed.

Deployed release `20260909T132449Z` with a timestamped rollback reference and no
gateway restart. Public app/diagnostic HTML and six referenced assets match the
build. PWA CSP remains present, unauthenticated API returns 401 and the gateway is
active. Physical iPhone acceptance of the correction is still pending.

### Space-to-echo visual handoff (13:28:46 UTC)

User reported the native-input correction works substantially better, but space
causes a flash. The pending preview was hidden synchronously before the remote
echo rendered. Space now retains a visual-only copy at its original buffer anchor
until matching echoed cells render, with a 700 ms fallback and explicit cleanup.
No extra terminal input/local echo bytes are introduced. Added regressions for
unrelated/partial/matching renders, cancellation, next composition and timeout.
All 58 tests, formatting/type checks and build passed. Deployed `20260909T132846Z`
without gateway restart, preserving rollback. Public app/diagnostic HTML and all
six referenced assets match; PWA CSP and unauthenticated API 401 were checked.
Actual iPhone visual acceptance remains pending; no browser run is claimed.

### Native iPhone Paste hit target (13:32:28 UTC)

User accepted the space handoff as clean and requested native long-press Paste.
xterm's existing textarea had opacity 0 / z-index -5. An iOS-only class exposes
that same editable element at xterm's cursor position with a small touch area;
the glyphs remain transparent because the accepted inline preview shows them.
Textarea gestures bypass xterm desktop selection and mobile scroll/refocus
handlers without preventing native defaults. Paste itself remains xterm-owned,
with pending Hangul flushed before it. No input values or selections are rewritten
by gesture handling and no custom menu or clipboard read was added.

All 59 tests, formatting/type checks and build passed. The new test checks gesture
ownership, native default preservation, composition retention and paste handoff;
it does not prove iOS system menu appearance. Deployed `20260909T133228Z` with
rollback preserved and no gateway restart. Both public HTML entries and six assets
match; PWA CSP and API 401 were verified. Physical-device menu acceptance is pending.

### Keyboard-open native text selection candidate (13:39:42 UTC)

User accepted native Paste and requested selection/Copy while the keyboard is open.
Mobile rendered-text gestures now retain native contextmenu ownership even before
Safari publishes a selected range. Compatibility mousedown is suppressed throughout
a held touch and briefly after release; a long press/drag's synthesized click does
not refocus xterm. A new short tap resumes input. Browser defaults remain untouched,
and the previously accepted iOS editable Paste target/Hangul bridge are unchanged.

All 61 tests, format/type checks and build passed. New regressions cover contextmenu
before selection, a hold longer than one second, delayed selection/copy and short
tap recovery. These are event-ownership tests, not physical-device menu validation.
Deployed `20260909T133942Z` with rollback preserved and gateway active/no restart.
Public app/diagnostic HTML and six assets match; PWA CSP and API 401 were verified.
Physical iPhone keyboard-open selection acceptance remains pending.

### Android Chrome PWA follow-up (13:51:15–13:53:56 UTC)

The first Android target (`20260909T135115Z`) exposed a small cursor-adjacent area
only while focused and keyboard-visible. The user reported no effect. Release
`20260909T135356Z` widened it to the full current input row and a 44 px touch band
clamped inside the terminal. Native gesture protection is shared with iOS, with
no Android IME rewrite or custom clipboard menu. The user said it appeared to work
and chose to keep it; this is initial feedback, not broad compatibility acceptance.
All 63 automated tests/build checks passed and deployed assets matched. Current
status and accepted limits belong in the non-archived documents linked above.
