// Network-only by design: no credentials, API replies, conversations or terminal
// bytes are cached. The offline page contains no account or session information.
self.addEventListener("install", (event) =>
  event.waitUntil(self.skipWaiting()),
);
self.addEventListener("activate", (event) =>
  event.waitUntil(self.clients.claim()),
);
self.addEventListener("fetch", (event) => {
  if (event.request.mode !== "navigate") return;
  event.respondWith(
    fetch(event.request).catch(
      () =>
        new Response(
          `<!doctype html>
<html lang="ko"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="theme-color" content="#17191d"><title>HMux · 연결 대기</title>
<style>body{margin:0;background:#17191d;color:#e5e7eb;font:16px/1.6 system-ui;display:grid;place-items:center;min-height:100dvh}main{padding:32px;max-width:360px}h1{font-size:28px}p{color:#a0a6b1}a{color:#9aadc6}</style>
<main><h1>HMux</h1><p>네트워크 연결을 확인해주세요.<br>Home에서 실행 중인 작업은 계속됩니다.</p><a href="/">다시 연결</a></main></html>`,
          {
            status: 503,
            headers: {
              "Content-Type": "text/html; charset=utf-8",
              "Cache-Control": "no-store",
              "Content-Security-Policy":
                "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'",
            },
          },
        ),
    ),
  );
});

const sessionIDPattern = /^\$[0-9]{1,12}$/;
const loginIDPattern = /^[A-Za-z0-9_-]{1,128}$/;
const eventIDPattern = /^[A-Za-z0-9_-]{1,128}$/;

function notificationFromPayload(value) {
  if (!value || typeof value !== "object") return;
  if (
    !["codex-complete", "test"].includes(value.type) ||
    !loginIDPattern.test(value.login_id || "") ||
    !eventIDPattern.test(value.event_id || "")
  )
    return;
  if (value.type === "test") {
    if (value.session !== undefined) return;
    return {
      body: "알림이 정상적으로 연결되었습니다.",
      data: null,
      tag: `hmux-test-${value.event_id}`,
    };
  }
  const session = value.session;
  const tabName =
    typeof value.tab_name === "string"
      ? value.tab_name
          .replace(/[\u0000-\u001f\u007f]+/g, " ")
          .replace(/\s+/g, " ")
          .trim()
      : "";
  if (
    !session ||
    typeof session !== "object" ||
    !sessionIDPattern.test(session.id || "") ||
    !Number.isSafeInteger(session.created_at) ||
    session.created_at <= 0 ||
    !tabName ||
    tabName.length > 120
  )
    return;
  return {
    title: `Codex 완료 · ${tabName}`,
    body: `${tabName} 탭의 작업이 완료됐습니다.`,
    data: {
      session: { id: session.id, created_at: session.created_at },
      login_id: value.login_id,
    },
    tag: `hmux-complete-${value.event_id}`,
  };
}

self.addEventListener("push", (event) => {
  let value;
  try {
    value = event.data?.json();
  } catch {
    return;
  }
  const notification = notificationFromPayload(value);
  if (!notification) return;
  event.waitUntil(
    (async () => {
      let response;
      try {
        response = await fetch("/api/session", {
          credentials: "same-origin",
          cache: "no-store",
        });
      } catch {
        // Ownership cannot be verified while offline. Do not display a previous
        // account's tab name merely because the push service queued a message.
        return;
      }
      if (response) {
        // Any HTTP response definitively identifies reachable authentication
        // state. Fail closed on errors, malformed JSON and account mismatch.
        if (!response.ok) return;
        let session;
        try {
          session = await response.json();
        } catch {
          return;
        }
        if (session.login_id !== value.login_id) return;
      }
      await self.registration.showNotification(notification.title || "HMux", {
        body: notification.body,
        icon: "/icons/hmux-graphite-192.png",
        badge: "/icons/hmux-graphite-192.png",
        tag: notification.tag,
        data: notification.data,
        timestamp: Date.now(),
      });
    })(),
  );
});

function rootAppClients(clients) {
  return clients.filter((client) => {
    try {
      const url = new URL(client.url);
      return (
        url.origin === self.location.origin &&
        (url.pathname === "/" || url.pathname === "/index.html")
      );
    } catch {
      return false;
    }
  });
}

function sendOpenRequest(client, target) {
  return new Promise((resolve) => {
    const channel = new MessageChannel();
    let settled = false;
    const finish = (acknowledged) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      channel.port1.close();
      resolve(acknowledged);
    };
    channel.port1.onmessage = (event) =>
      finish(event.data?.type === "hmux-push-open-ack");
    const timer = setTimeout(() => finish(false), 350);
    try {
      client.postMessage(
        {
          type: "hmux-push-open",
          session: { ...target.session },
          login_id: target.login_id,
        },
        [channel.port2],
      );
    } catch {
      finish(false);
    }
  });
}

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const target = event.notification.data;
  if (!target) {
    event.waitUntil(self.clients.openWindow("/"));
    return;
  }
  if (
    !target.session ||
    !sessionIDPattern.test(target.session.id || "") ||
    !Number.isSafeInteger(target.session.created_at) ||
    target.session.created_at <= 0 ||
    !loginIDPattern.test(target.login_id || "")
  )
    return;
  const url =
    "/?push_session=" +
    encodeURIComponent(target.session.id) +
    "&push_created=" +
    target.session.created_at +
    "&push_login=" +
    encodeURIComponent(target.login_id);
  event.waitUntil(
    (async () => {
      const matches = await self.clients.matchAll({
        type: "window",
        includeUncontrolled: true,
      });
      const clients = rootAppClients(matches).sort(
        (left, right) => Number(right.focused) - Number(left.focused),
      );
      if (!clients.length) return self.clients.openWindow(url);
      // Activate only one client; broadcasting would switch every open window.
      for (const client of clients.slice(0, 8)) {
        if (await sendOpenRequest(client, target)) return client.focus();
      }

      // Old or differently authenticated root apps do not acknowledge. Navigate
      // one root app to the exact target so a click never becomes a no-op.
      const fallback = clients[0];
      if (typeof fallback.navigate === "function") {
        const navigated = await fallback.navigate(url);
        return (navigated || fallback).focus();
      }
      return self.clients.openWindow(url);
    })(),
  );
});
