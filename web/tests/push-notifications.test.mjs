import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";
import { MessageChannel } from "node:worker_threads";
import {
  installPushNotifications,
  installPushPresence,
  pushTargetFromMessage,
  pushTargetFromURL,
} from "../src/push-notifications.ts";

const tick = () => new Promise((resolve) => setImmediate(resolve));

class Node {
  constructor(tag) {
    this.tagName = tag;
    this.children = [];
    this.attrs = {};
    this.hidden = false;
    this.disabled = false;
    this.textContent = "";
    this.className = "";
  }
  append(...children) {
    this.children.push(...children);
  }
  setAttribute(name, value) {
    this.attrs[name] = value;
  }
  querySelectorAll(tag) {
    return this.children.flatMap((child) => [
      ...(child.tagName === tag ? [child] : []),
      ...child.querySelectorAll(tag),
    ]);
  }
}

function installBrowser({ ready = Promise.resolve() } = {}) {
  const document = { createElement: (tag) => new Node(tag) };
  const window = { Notification: {}, PushManager: {} };
  const Notification = {
    permission: "granted",
    requestPermission: async () => "granted",
  };
  const navigator = {
    userAgent: "Test",
    platform: "Test",
    maxTouchPoints: 0,
    serviceWorker: { ready },
  };
  Object.defineProperties(globalThis, {
    document: { value: document, configurable: true },
    window: { value: window, configurable: true },
    Notification: { value: Notification, configurable: true },
    navigator: { value: navigator, configurable: true },
  });
}

const pushState = (overrides = {}) => ({
  public_key: "AQID",
  login_id: "login_A",
  enabled: false,
  endpoint: "",
  ...overrides,
});

const subscription = (endpoint, unsubscribe = async () => true) => ({
  endpoint,
  unsubscribe,
  toJSON: () => ({
    endpoint,
    expirationTime: null,
    keys: { p256dh: "p256dh", auth: "auth" },
  }),
});

test("push targets require exact finite session lifetime and account identity", () => {
  assert.deepEqual(
    pushTargetFromURL(
      "https://hmux.test/?push_session=%242&push_created=1700000000&push_login=login_A",
    ),
    {
      session: { id: "$2", created_at: 1700000000 },
      login_id: "login_A",
    },
  );
  assert.deepEqual(
    pushTargetFromURL(
      "https://hmux.test/?push_session=%240&push_created=1&push_login=login_A",
    )?.session,
    { id: "$0", created_at: 1 },
  );
  for (const url of [
    "https://hmux.test/?push_session=name&push_created=1&push_login=login_A",
    "https://hmux.test/?push_session=%242&push_created=1.5&push_login=login_A",
    "https://hmux.test/?push_session=%242&push_created=1&push_login=bad%20login",
  ])
    assert.equal(pushTargetFromURL(url), undefined);
  assert.equal(
    pushTargetFromMessage({
      type: "hmux-push-open",
      session: { id: "$2", created_at: Infinity },
      login_id: "login_A",
    }),
    undefined,
  );
});

test("disposing an account panel aborts reads and blocks a late account state", async () => {
  installBrowser();
  const root = new Node("section");
  let finish;
  let signal;
  const dispose = installPushNotifications(
    root,
    (_path, _body, requestSignal) => {
      signal = requestSignal;
      return new Promise((resolve) => (finish = resolve));
    },
    "login_A",
  );
  dispose();
  finish({
    public_key: "key",
    login_id: "login_B",
    enabled: true,
    endpoint: "https://push.invalid/old",
  });
  await tick();
  assert.equal(signal.aborted, true);
  assert.match(root.querySelectorAll("p")[1].textContent, /Checking/);
});

test("disposing during explicit opt-in cannot subscribe the next account", async () => {
  let releaseReady;
  const ready = new Promise((resolve) => (releaseReady = resolve));
  installBrowser({ ready });
  const root = new Node("section");
  const calls = [];
  const dispose = installPushNotifications(
    root,
    async (path, body) => {
      calls.push([path, body]);
      return {
        public_key: "AQID",
        login_id: "login_A",
        enabled: false,
        endpoint: "",
      };
    },
    "login_A",
  );
  await tick();
  const enable = root.querySelectorAll("button")[0];
  const pending = enable.onclick();
  dispose();
  let subscribed = 0;
  releaseReady({
    pushManager: {
      getSubscription: async () => null,
      subscribe: async () => {
        subscribed++;
      },
    },
  });
  await pending;
  assert.equal(subscribed, 0);
  assert.deepEqual(
    calls.map(([path]) => path),
    ["/api/push"],
  );
});

test("toggle success and test failure outcomes survive final rendering", async () => {
  let local = null;
  const registration = {
    pushManager: {
      getSubscription: async () => local,
      subscribe: async () =>
        (local = subscription("https://push.invalid/current")),
    },
  };
  installBrowser({ ready: Promise.resolve(registration) });
  const root = new Node("section");
  const dispose = installPushNotifications(
    root,
    async (path) => {
      if (path === "/api/push") return pushState();
      if (path === "/api/push/test") throw new Error("test delivery failed");
      return { ok: true };
    },
    "login_A",
  );
  await tick();
  await root.querySelectorAll("button")[0].onclick();
  assert.match(
    root.querySelectorAll("p")[1].textContent,
    /Turned on completion notifications/,
  );
  await root.querySelectorAll("button")[2].onclick();
  assert.equal(
    root.querySelectorAll("p")[1].textContent,
    "test delivery failed",
  );
  dispose();
});

test("server disable remains visible when local browser unsubscribe fails", async () => {
  const local = subscription("https://push.invalid/current", async () => {
    throw new Error("browser cleanup failed");
  });
  installBrowser({
    ready: Promise.resolve({
      pushManager: {
        getSubscription: async () => local,
        subscribe: async () => assert.fail("unexpected subscribe"),
      },
    }),
  });
  const root = new Node("section");
  const dispose = installPushNotifications(
    root,
    async (path) =>
      path === "/api/push"
        ? pushState({
            enabled: true,
            endpoint: "https://push.invalid/current",
          })
        : { ok: true },
    "login_A",
  );
  await tick();
  await tick();
  const toggle = root.querySelectorAll("button")[0];
  await toggle.onclick();
  assert.equal(toggle.textContent, "Turn on notifications");
  assert.match(
    root.querySelectorAll("p")[1].textContent,
    /Server notifications are off/,
  );
  dispose();
});

test("enabled server state can reconnect a missing local subscription", async () => {
  let local = null;
  installBrowser({
    ready: Promise.resolve({
      pushManager: {
        getSubscription: async () => local,
        subscribe: async () =>
          (local = subscription("https://push.invalid/reconnected")),
      },
    }),
  });
  const posts = [];
  const root = new Node("section");
  const dispose = installPushNotifications(
    root,
    async (path, body) => {
      if (path === "/api/push")
        return pushState({
          enabled: true,
          endpoint: "https://push.invalid/missing",
        });
      posts.push([path, body]);
      return { ok: true };
    },
    "login_A",
  );
  await tick();
  await tick();
  const reconnect = root.querySelectorAll("button")[1];
  assert.equal(reconnect.hidden, false);
  await reconnect.onclick();
  assert.equal(local.endpoint, "https://push.invalid/reconnected");
  assert.equal(posts[0][0], "/api/push/subscribe");
  assert.match(
    root.querySelectorAll("p")[1].textContent,
    /Turned on completion notifications/,
  );
  dispose();
});

test("late old-panel subscription cleanup finishes before a new panel subscribes", async () => {
  let current = null;
  let finishFirstSubscribe;
  let subscribeCount = 0;
  const events = [];
  const oldSubscription = subscription("https://push.invalid/old", async () => {
    events.push("old-unsubscribe");
    current = null;
    return true;
  });
  const newSubscription = subscription("https://push.invalid/new");
  const registration = {
    pushManager: {
      getSubscription: async () => current,
      subscribe: async () => {
        subscribeCount++;
        if (subscribeCount === 1)
          return new Promise((resolve) => {
            finishFirstSubscribe = () => {
              events.push("old-subscribe-resolved");
              current = oldSubscription;
              resolve(oldSubscription);
            };
          });
        events.push("new-subscribe");
        current = newSubscription;
        return newSubscription;
      },
    },
  };
  installBrowser({ ready: Promise.resolve(registration) });
  const posts = [];
  const api = async (path, body) => {
    if (path === "/api/push") return pushState();
    posts.push([path, body]);
    return { ok: true };
  };
  const oldRoot = new Node("section");
  const disposeOld = installPushNotifications(oldRoot, api, "login_A");
  await tick();
  const oldPending = oldRoot.querySelectorAll("button")[0].onclick();
  await tick();
  disposeOld();

  const newRoot = new Node("section");
  const disposeNew = installPushNotifications(newRoot, api, "login_A");
  await tick();
  const newPending = newRoot.querySelectorAll("button")[0].onclick();
  await tick();
  assert.equal(subscribeCount, 1);
  finishFirstSubscribe();
  await Promise.all([oldPending, newPending]);
  assert.deepEqual(events, [
    "old-subscribe-resolved",
    "old-unsubscribe",
    "new-subscribe",
  ]);
  assert.equal(current.endpoint, "https://push.invalid/new");
  assert.deepEqual(
    posts.map(([path, body]) => [path, body.endpoint]),
    [["/api/push/subscribe", "https://push.invalid/new"]],
  );
  disposeNew();
});

test("presence sends the exact active identity and nulls it on blur", async () => {
  const listeners = new Map();
  const document = {
    visibilityState: "visible",
    hasFocus: () => true,
    addEventListener: (name, fn) => listeners.set(`d:${name}`, fn),
    removeEventListener: (name) => listeners.delete(`d:${name}`),
  };
  const window = {
    addEventListener: (name, fn) => listeners.set(`w:${name}`, fn),
    removeEventListener: (name) => listeners.delete(`w:${name}`),
    setInterval: () => 7,
    clearInterval: () => {},
  };
  const calls = [];
  const presence = installPushPresence(
    async (_path, body) => calls.push(body),
    () => ({ id: "$7", created_at: 1700000007 }),
    { document, window, clientID: () => "client_7" },
  );
  await tick();
  assert.deepEqual(calls[0], {
    client_id: "client_7",
    session: { id: "$7", created_at: 1700000007 },
  });
  document.hasFocus = () => false;
  listeners.get("w:blur")();
  await tick();
  assert.deepEqual(calls[1], { client_id: "client_7", session: null });
  presence.dispose();
  assert.equal(listeners.size, 0);
});

async function serviceWorkerHarness(
  fetchImpl = async () =>
    new Response(JSON.stringify({ login_id: "login_9" }), { status: 200 }),
  locale = "en",
) {
  const source = await readFile(
    new URL("../public/sw.js", import.meta.url),
    "utf8",
  );
  const handlers = {};
  const shown = [];
  const posted = [];
  const opened = [];
  const client = {
    url: "https://hmux.test/",
    focused: true,
    postMessage: (value, ports) => {
      posted.push(value);
      ports[0].postMessage({ type: "hmux-push-open-ack" });
    },
    focus: async () => client,
  };
  const self = {
    addEventListener: (name, handler) => (handlers[name] = handler),
    skipWaiting: async () => {},
    location: {
      origin: "https://hmux.test",
      href: `https://hmux.test/sw.js?lang=${locale}`,
    },
    registration: {
      showNotification: async (title, options) =>
        shown.push({ title, options }),
    },
    clients: {
      claim: async () => {},
      matchAll: async () => [client],
      openWindow: async (url) => opened.push(url),
    },
  };
  vm.runInNewContext(source, {
    self,
    fetch: fetchImpl,
    Response,
    MessageChannel,
    URL,
    setTimeout,
    clearTimeout,
  });
  return { handlers, shown, posted, opened };
}

test("service worker accepts only fixed notification fields and exact identity", async () => {
  const harness = await serviceWorkerHarness();
  const push = async (payload) => {
    let pending;
    harness.handlers.push({
      data: { json: () => payload },
      waitUntil: (value) => (pending = value),
    });
    await pending;
  };
  await push({
    type: "codex-complete",
    session: { id: "$9", created_at: 1700000009 },
    login_id: "login_9",
    event_id: "event_9",
    tab_name: "Research",
    title: "attacker title",
    body: "transcript contents",
    url: "https://attacker.invalid/",
  });
  assert.equal(harness.shown.length, 1);
  assert.equal(harness.shown[0].title, "Codex complete · Research");
  assert.equal(
    harness.shown[0].options.body,
    "Work in the Research tab is complete.",
  );
  assert.deepEqual(JSON.parse(JSON.stringify(harness.shown[0].options.data)), {
    session: { id: "$9", created_at: 1700000009 },
    login_id: "login_9",
  });
  await push({
    type: "codex-complete",
    session: { id: "Research", created_at: 1700000009 },
    login_id: "login_9",
    event_id: "event_10",
    tab_name: "Research",
  });
  assert.equal(harness.shown.length, 1);
});

test("service worker uses selected Korean while keeping the same fixed push fields", async () => {
  const harness = await serviceWorkerHarness(undefined, "ko");
  let pending;
  harness.handlers.push({
    data: {
      json: () => ({
        type: "codex-complete",
        session: { id: "$9", created_at: 1700000009 },
        login_id: "login_9",
        event_id: "event_ko",
        tab_name: "Research",
        title: "untrusted title",
      }),
    },
    waitUntil: (value) => (pending = value),
  });
  await pending;
  assert.equal(harness.shown[0].title, "Codex 완료 · Research");
  assert.equal(
    harness.shown[0].options.body,
    "Research 탭의 작업이 완료됐습니다.",
  );
});

test("service worker suppresses a queued push for a known different login", async () => {
  const harness = await serviceWorkerHarness(
    async () =>
      new Response(JSON.stringify({ login_id: "login_other" }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
  );
  let pending;
  harness.handlers.push({
    data: {
      json: () => ({
        type: "test",
        login_id: "login_A",
        event_id: "event_A",
      }),
    },
    waitUntil: (value) => (pending = value),
  });
  await pending;
  assert.equal(harness.shown.length, 0);
});

test("notification click messages an existing window to preserve pending input", async () => {
  const harness = await serviceWorkerHarness();
  let pending;
  harness.handlers.notificationclick({
    notification: {
      data: {
        session: { id: "$4", created_at: 1700000004 },
        login_id: "login_4",
      },
      close() {},
    },
    waitUntil: (value) => (pending = value),
  });
  await pending;
  assert.deepEqual(JSON.parse(JSON.stringify(harness.posted)), [
    {
      type: "hmux-push-open",
      session: { id: "$4", created_at: 1700000004 },
      login_id: "login_4",
    },
  ]);
  assert.deepEqual(harness.opened, []);
});

test("notification click opens only the fixed same-origin exact-session URL", async () => {
  let pending;
  // Rebuild the small harness without an existing window.
  const source = await readFile(
    new URL("../public/sw.js", import.meta.url),
    "utf8",
  );
  const handlers = {};
  const opened = [];
  const self = {
    addEventListener: (name, handler) => (handlers[name] = handler),
    skipWaiting: async () => {},
    location: { origin: "https://hmux.test" },
    registration: { showNotification: async () => {} },
    clients: {
      claim: async () => {},
      matchAll: async () => [],
      openWindow: async (url) => opened.push(url),
    },
  };
  vm.runInNewContext(source, {
    self,
    fetch,
    Response,
    MessageChannel,
    URL,
    setTimeout,
    clearTimeout,
  });
  handlers.notificationclick({
    notification: {
      data: {
        session: { id: "$12", created_at: 1700000012 },
        login_id: "login_12",
        url: "https://attacker.invalid/",
      },
      close() {},
    },
    waitUntil: (value) => (pending = value),
  });
  await pending;
  assert.deepEqual(opened, [
    "/?push_session=%2412&push_created=1700000012&push_login=login_12",
  ]);
});

test("notification click ignores diagnostics and navigates a nonresponding old root app", async () => {
  const source = await readFile(
    new URL("../public/sw.js", import.meta.url),
    "utf8",
  );
  const handlers = {};
  const calls = [];
  const diagnostic = {
    url: "https://hmux.test/input-diagnostic.html",
    focused: true,
    postMessage: () => calls.push("diagnostic-message"),
  };
  const root = {
    url: "https://hmux.test/",
    focused: false,
    postMessage: () => calls.push("root-message"),
    navigate: async (url) => {
      calls.push(`navigate:${url}`);
      return root;
    },
    focus: async () => calls.push("root-focus"),
  };
  const self = {
    addEventListener: (name, handler) => (handlers[name] = handler),
    skipWaiting: async () => {},
    location: { origin: "https://hmux.test" },
    registration: { showNotification: async () => {} },
    clients: {
      claim: async () => {},
      matchAll: async () => [diagnostic, root],
      openWindow: async () => assert.fail("unexpected new window"),
    },
  };
  vm.runInNewContext(source, {
    self,
    fetch,
    Response,
    MessageChannel,
    URL,
    setTimeout: (callback) => {
      queueMicrotask(callback);
      return 1;
    },
    clearTimeout: () => {},
  });
  let pending;
  handlers.notificationclick({
    notification: {
      data: {
        session: { id: "$0", created_at: 1700000012 },
        login_id: "login_12",
      },
      close() {},
    },
    waitUntil: (value) => (pending = value),
  });
  await pending;
  assert.deepEqual(calls, [
    "root-message",
    "navigate:/?push_session=%240&push_created=1700000012&push_login=login_12",
    "root-focus",
  ]);
});

test("service worker suppresses notifications when login cannot be verified", async () => {
  for (const fetcher of [
    async () => {
      throw new Error("offline");
    },
    async () => new Response("", { status: 401 }),
    async () => new Response("", { status: 503 }),
  ]) {
    const harness = await serviceWorkerHarness(fetcher);
    let pending;
    harness.handlers.push({
      data: {
        json: () => ({
          type: "codex-complete",
          login_id: "login_9",
          event_id: "event_9",
          tab_name: "Private tab",
          session: { id: "$0", created_at: 42 },
        }),
      },
      waitUntil: (value) => (pending = value),
    });
    await pending;
    assert.equal(harness.shown.length, 0);
  }
});
