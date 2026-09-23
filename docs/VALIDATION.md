# Validation status

This is the current validation/deployment summary, updated 2026-09-24. It describes
one maintained installation; it does not establish the state of independent deployments.
Detailed dated evidence is preserved in [validation history](archive/VALIDATION_HISTORY_2026-09-24.md).

## Current verified baseline

- Gateway/web and native macOS Home: `cleanup-20260923T154706Z` (runtime commit `fe0c509`). Both binary hashes
  and all 47 public asset hashes verified; gateway active with zero automatic restarts.
- Current release verified no-store/PWA CSP, five anonymous API 401 barriers and
  preservation of all 25 original tmux identities. Browser-only disposable views
  were recreated on reconnect. Existing rollback releases and
  timestamped Home backups remain available.
- `make check`, `make integration` and `make build` passed for the performance
  baseline. Subsequent latency instrumentation passed focused Go tests/race/vet
  and both binary builds; provider-aware loading passed web check/build and 171 tests.
- Native service installation/update succeeded. Earlier historical entries saying
  Home activation was pending are superseded; physical reboot/login behavior was
  not newly tested. Linux unit verification belongs to Linux CI/device evidence.

## Browser and device evidence

- Synthetic Chrome checks: cached workspace before live state, no premature terminal
  connection, account isolation, 32 restored tabs with only one initial xterm,
  initialization on selection, and unchanged usage polling without DOM mutations.
- Claude loading card reviewed at 390×844 with a held synthetic response. Codex and
  neutral fallback labels have regression coverage. Provider attribution for returned
  messages remains server-owned. Live authenticated Claude conversation acceptance
  has not been recorded, despite deployment of the implementation.
- Device-confirmed input behavior and accepted limits remain in
  [browser input](BROWSER_INPUT.md#accepted-mobile-behavior) and [iOS input](IOS_INPUT.md).
  Synthetic responsive checks do not establish physical Safari/iOS/Android behavior.

## Performance evidence and unresolved work

The latest startup sample separated recovery synchronization (1,310 ms), catalog
collection (4,720 ms) and publication (2 ms). Nested spans overlap and must not be
summed. Earlier first-catalog observations were about 14.8 s and 7.4 s; these are
individual observations, not a controlled browser latency benchmark.

Remaining work is to attribute the slow catalog internals and capture intermittent
5-second requests with the new operation-tagged diagnostics. Recent workspace
requests were 61–171 ms with `send_ms=0`. Neither every latency source nor the
cause of every connection interruption is claimed resolved. No measured total
memory target is claimed.

## Ongoing maintenance

Documentation and Go cleanup completed locally on 2026-09-24. Current references
were separated from dated evidence, and Go CLI/gateway/collector/service/workflow
responsibilities were split into focused files without intended behavior changes.
Independent documentation and Go reviews reported no material findings.

`make check` passed (including full Go/collector race/vet, ShellCheck and 171 web
tests). Installer, hooks and isolated session-create integration passed. Isolated
PTY integration initially failed because the sandbox denied `/bin/ps`; its targeted
rerun in the normal host environment passed. Local Markdown paths/headings and
archive discovery checks passed. `make build` passed for web assets, Linux gateway
and macOS Home/helper binaries. The cleanup was subsequently deployed as the
release above. Linux CI exposed a scalar WorkingDirectory quoting error and a
disposable-process test assumption about birth-time resolution; both were fixed
in `fe0c509`. GitHub Actions run `35883961196` passed all three jobs for that runtime commit:
macOS Go/unit/race/vet/integration, Linux native service verification, and web
check/test/build.
