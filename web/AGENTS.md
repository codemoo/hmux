# Web HMux maintenance

The repository product/security specification and root AGENTS.md apply here.
`docs/WEB.md` is the current web behavior/operations reference. Historical validation
logs are not current implementation instructions. Web/PWA is the only UI.

- Preserve same-origin `manifest-src` and `worker-src` in the gateway CSP;
  HTTP 200 assets alone do not verify PWA installability. Keep default-src restricted.
- Android Chrome PWA installation/colors are user-confirmed. Samsung Internet
  token backgrounds remain unverified; do not generalize that issue to Android
  or change IME/viewport code for it. Preserve only-dark/terminal color semantics.
- Keep this a TypeScript/xterm.js client and a shared gateway/Home connection.
  Rust runtime release acceptance follows `docs/RUST_MIGRATION.md`; preserve
  browser contracts. Reuse Home catalog, conversation, quota and recovery services;
  do not infer provider IDs in tabs.
- Keep `{id, created_at}` identity checks, profile isolation and shared tab ordering.
  Selected tabs, terminal theme and font preferences are device-local; account profiles are not shell isolation.
- One live terminal view per visible browser, eight across the gateway. Releasing
  a browser view must never end the original tmux/provider process.
- `viewport.ts` owns safe-area/keyboard geometry. Use visualViewport height once;
  preserve retained top inset, Android keyboard tracking, and the pinned Android
  helper textarea. Do not add autofocus on Android tab/dialog transitions.
  Android Chrome PWA uses a full-input-row Paste target (initial user acceptance);
  do not revert to the ineffective cursor-sized target. It may translate only while
  focused and keyboard-visible; preserve the pinned anchor during keyboard opening
  and dismissal. Native editable
  gesture ownership is shared in `native-clipboard.ts`; do not fork platform copies.
- Android retains default xterm input. iOS's targeted native-input bridge is based
  on the supplied physical trace (keyCode 0, no composition*, delete + insert).
  Keep native DOM editing and a visible inline pending run; do not add Hangul
  reassembly, rolling-tail or remote replacement. Standard composition stays with
  xterm. Rejected diagnostic adapters remain fixtures only. Require real device
  evidence for additional workarounds; synthetic tests are regression checks only.
- Desktop macOS Safari uses the shared native-input bridge for the supplied
  Safari 18.6 input-before-keydown229 and selected insertReplacementText trace.
  Current Hangul input is user-accepted (2026-09-10). Preserve browser-owned
  composition, local preview and boundary flush; keep desktop geometry/gestures
  and standard composition with xterm. Do not reopen accepted input work without
  new evidence or a user request. The desktop diagnostic remains a stock baseline.
- Mobile selection/copy uses browser-native ranges and context menus through
  `native-clipboard.ts`; do not reintroduce clipboard dialogs/buttons. Preserve
  long-press/selection-handle gesture ownership. Desktop mouse-tracking drag uses
  `desktop-terminal.ts` to defer primary clicks and let xterm own forced local
  selection; preserve click/wheel forwarding and the Cmd/Ctrl+C selection guard.
  HTTP(S) links require the URL popover's explicit open action; retain scheme
  validation, textContent, noopener/noreferrer and disposal on tab/dialog changes.
  `terminal-links.ts` owns the shared popover; `mobile-terminal-links.ts` owns short
  touch activation only. Preserve native long press, selection, editable targets
  and scroll. Its non-bubbling hover lookup depends on pinned xterm 6 synchronous
  providers; verify repeat taps, wrapped URLs, OSC8 and no PTY mouse output on upgrades.
  iPhone Hangul input, space handoff and native Paste are user-accepted. Output
  selection dismisses the keyboard; the user accepted that limitation and stopped
  keyboard-open selection work. Preserve this behavior unless explicitly revisited.
  Never infer physical-device menu behavior from event tests alone.
- `theme.ts` owns the validated device-local terminal palette. Apply changes to
  all xterms without reconnect/reset, keep native pending-input colors in sync,
  and retain pinned upstream palette notices. UI chrome remains independently dark.
- `fonts.ts` owns font loading. Keep Monatendard Regular/Bold, Korean coverage,
  WOFF2/TTF compatibility and included licenses.
- CSS order is `style.css` (base/viewport), `ios-native-input.css` (iOS input),
  `chrome.css` (workspace UI), `dialogs.css` (dialog components). Edit the owning
  rule instead of appending another conflicting override. Preserve keyboard-visible
  hiding and footer nowrap.
- Use the maintained vector Bedl frames in `public/bedl`. Preserve their provenance
  and bundled notices; do not ship abandoned frame variants.
- Settings login-session responses belong to the originating dialog/account;
  abort on close/disposal and ignore late responses. Session listing/revocation
  is server-owned and user-scoped. Never treat public session IDs as credentials.
- `account-security.ts` owns the per-dialog TOTP switch and reauthentication form.
  Keep passwords/codes transient, abort and clear them on disposal, and ignore late
  responses. Refresh the current account's session list after changing policy.
  Login's password-first TOTP challenge must never enter the workspace or imply
  an authenticated session; only the server decides whether TOTP is required.
- `attachments.ts` owns transient attachment UI and target epochs; `file-upload.ts`
  owns the bounded WebSocket stream. Keep file names browser-local, exact original
  tab/generation checks, native input flush, quoted paste without Enter, and immediate
  cancellation on logout/tab close. Uploads have a three-hour Home lifetime.
- Render external/session/transcript values with textContent. Keep the service
  worker network-only; do not cache credentials, transcripts or terminal bytes.
- Run `npm run check --prefix web`, `npm test --prefix web`, and
  `npm run build --prefix web`. Gateway changes also require relevant Rust
  backend tests and compatibility checks for shared contracts.
  Opt-in live tests may use only isolated `hmux-e2e-*` resources.
- Report automated checks separately from browser/device acceptance. Do not call
  a typecheck a mobile UI test. Preserve timestamped configuration/release backups.

## Documentation maintenance

- `docs/WEB.md` owns current behavior/operations, `docs/IOS_INPUT.md` owns iPhone
  input details, and `docs/VALIDATION.md` owns dated evidence and the current release.
- Preserve the difference between initial user feedback, accepted limitations and
  automated checks. An accepted limitation is not an unfinished confirmation request.
- Archive superseded experiments with an explicit non-current label. Never restore
  rejected code or reopen stopped UX work because an old log says “pending”.
- Documentation-only cleanup does not require rebuilding or redeploying the app.
