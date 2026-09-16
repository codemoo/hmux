import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
test("manifest supplies standalone identity and real PNG icon dimensions", () => {
  const manifest = JSON.parse(
    readFileSync(new URL("../public/manifest.json", import.meta.url)),
  );
  assert.equal(manifest.display, "standalone");
  assert.equal(manifest.scope, "/");
  assert.equal(manifest.id, "/");
  for (const icon of manifest.icons) {
    const data = readFileSync(new URL("../public" + icon.src, import.meta.url));
    assert.equal(data.toString("ascii", 1, 4), "PNG");
    assert.equal(
      `${data.readUInt32BE(16)}x${data.readUInt32BE(20)}`,
      icon.sizes,
    );
  }
});
test("service worker never intercepts API and only returns anonymous offline navigation", async () => {
  const handlers = {};
  const source = readFileSync(
    new URL("../public/sw.js", import.meta.url),
    "utf8",
  );
  const self = {
    addEventListener: (name, fn) => (handlers[name] = fn),
    skipWaiting: async () => {},
    clients: { claim: async () => {} },
  };
  vm.runInNewContext(source, {
    self,
    Response,
    fetch: async () => {
      throw Error("offline");
    },
  });
  handlers.fetch({
    request: { mode: "cors", url: "https://hmux.test/api/state" },
    respondWith: () => assert.fail("API intercepted"),
  });
  let pending;
  handlers.fetch({
    request: { mode: "navigate" },
    respondWith: (value) => (pending = value),
  });
  const response = await pending;
  assert.equal(response.status, 503);
  assert.equal(response.headers.get("cache-control"), "no-store");
  const html = await response.text();
  assert.ok(html.includes("다시 연결"));
  assert.ok(!html.includes("heesoo"));
  assert.ok(!/caches\./.test(source));
});
