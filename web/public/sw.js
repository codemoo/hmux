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
<html lang="ko"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="theme-color" content="#100f0f"><title>HMux · 연결 대기</title>
<style>body{margin:0;background:#100f0f;color:#cecdc3;font:16px/1.6 system-ui;display:grid;place-items:center;min-height:100dvh}main{padding:32px;max-width:360px}h1{font-size:28px}p{color:#9f9d96}a{color:#4385be}</style>
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
