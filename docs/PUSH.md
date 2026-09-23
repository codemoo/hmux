# Completion notifications

Scoped reference for the [web/PWA interface](WEB.md). Repository and web input
contracts remain authoritative; device evidence is distinct from synthetic checks.

## Codex completion notifications

Settings → 완료 알림 enables Web Push for this browser/PWA and this login.
Permission is requested only after clicking 알림 켜기. Use 테스트 알림 to check
OS delivery. iPhone/iPad require iOS/iPadOS 16.4 or later and an installed Home
Screen web app; desktop/Android require a browser supporting Push API.

Notifications show the tab's alias/name and completion status, without prompts,
responses, paths or terminal output. Clicking focuses the existing app or opens
HMux and selects the exact `{id, created_at}` session. A notification belonging
to another or expired login cannot open a tab under the current account.

One Home observer reads authoritative `task_started` / `task_complete` records
from the exact bound Codex rollout. Shared catalog fetches (normally five seconds)
feed one separate worker with one latest pending snapshot; slow notification work
does not block catalog publication or cancel the connector on discovery/send
failure. The worker skips Claude metadata and redundant Codex state-tail scans,
checks cancellation between bounded file chunks/records, and waits five seconds
after an over-budget scan. Peer write deadlines include waiting for the shared
writer. It does not infer completion from idle output or CPU. The first scan,
changed/ambiguous bindings and oversized/truncated history establish a baseline
without replaying old work. A turn already running at baseline can still notify
when it completes. Discovery errors leave terminal operation available. The
connector must be running and Home awake; this is not a durable offline event
queue or a notification for Claude/shell completion.

The gateway checks each subscribed login against its account's authoritative
shared workspace and sends only for matching open tabs. Visible/focused clients
report only their selected session; a live presence lease suppresses that login's
notification for the same tab. Leases expire after 45 seconds if a browser closes
without reporting blur. Logout, revocation, credential-policy invalidation and
seven-day login expiry stop future sends. Re-login requires explicit opt-in;
subscriptions are never silently transferred to another account. Disabling
notifications affects this login's device. Expired provider subscriptions (HTTP
404/410) are removed. Events and outbound work are bounded, deduplicated, and have
a two-minute push lifetime; delivery also depends on browser/OS push service.
Before displaying a queued notification, the service worker verifies the current
login with the gateway. Unreachable authentication or a different login suppresses
the notification rather than exposing another account’s tab name.

The first upgraded gateway run creates `<credentials-path>.push.json` (0600)
with its persistent VAPID key pair and login-bound subscriptions. An owner-only
`.lock` file prevents overlapping gateways from rewriting that state. Keep this file
private and outside release archives, and preserve it across upgrades; losing
the key requires devices to subscribe again. No provider account or API key is
required. Outbound HTTPS is restricted to Apple, Google/FCM, Mozilla and Windows
push-service endpoints with private-address and redirect checks. Payloads use
standard encrypted Web Push and VAPID. A backend and Home connector update are
required in addition to the frontend; restarting these does not end tmux work.
