import test from "node:test";
import assert from "node:assert/strict";
import {
  createDiagnostics,
  diagnosticReason,
  diagnosticRoute,
} from "../src/diagnostics.ts";
const loginA = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const loginB = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
function setup(t, override = {}) {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const memory = new Map();
  const requests = [];
  const storage = {
    getItem: (k) => memory.get(k) || null,
    setItem: (k, v) => memory.set(k, v),
    removeItem: (k) => memory.delete(k),
  };
  let online = true;
  const collector = createDiagnostics({
    csrf: () => "fixture",
    build: "https://private.example/assets/app-abc123.js",
    now: () => 1700000000000,
    storage: () => storage,
    context: () => ({ online, visible: true, standalone: false }),
    fetch: async (path, options) => {
      requests.push({ path, options });
      return { ok: true, status: 202 };
    },
    ...override,
  });
  t.after(() => collector.dispose());
  return {
    collector,
    requests,
    memory,
    storage,
    setOnline: (value) => (online = value),
  };
}
test("payload allowlists fields and never serializes exception/body/URL secrets", async (t) => {
  const { collector, requests } = setup(t);
  collector.bind(loginA);
  collector.record("terminal-failed", {
    reason: "network",
    code: 1006,
    attempt: 2,
    retry_ms: 2000,
    message: "SECRET",
    stack: "SECRET",
    url: "https://SECRET",
    data: "SECRET",
    duration_ms: Infinity,
  });
  collector.record("runtime-error", {
    reason: diagnosticReason(new TypeError("SECRET")),
  });
  collector.record("api-failed", {
    route: diagnosticRoute("/api/state?token=SECRET"),
    reason: "http",
    code: 502,
  });
  await collector.flush();
  assert.equal(requests.length, 1);
  const body = requests[0].options.body;
  assert.ok(!body.includes("SECRET"));
  assert.ok(!body.includes("private.example"));
  assert.equal(JSON.parse(body).build, "app-abc123.js");
  assert.equal(JSON.parse(body).events[1].reason, "TypeError");
  assert.equal(requests[0].options.cache, "no-store");
});
test("late A response cannot acknowledge B queue or reuse A credentials", async (t) => {
  let finish,
    csrf = "A";
  const requests = [];
  const { collector, memory } = setup(t, {
    csrf: () => csrf,
    fetch: async (path, options) => {
      requests.push(options);
      if (requests.length === 1)
        return new Promise((resolve) => (finish = resolve));
      return { ok: true, status: 202 };
    },
  });
  collector.bind(loginA);
  collector.record("offline");
  const first = collector.flush();
  collector.dispose();
  csrf = "B";
  collector.bind(loginB);
  collector.record("resume");
  finish({ ok: true, status: 202 });
  await first;
  assert.equal(requests[0].signal.aborted, true);
  assert.equal(collector.snapshot().events[0].pending, true);
  assert.ok(!memory.has(`hmux.diagnostics.${loginA}`));
  await collector.flush();
  assert.equal(requests[1].headers["X-CSRF-Token"], "B");
  assert.deepEqual(
    JSON.parse(requests[1].body).events.map((e) => e.kind),
    ["resume"],
  );
});
test("failed upload does not recurse and queue remains capped while offline", async (t) => {
  let calls = 0;
  const { collector, setOnline } = setup(t, {
    fetch: async () => {
      calls++;
      throw new Error("SECRET");
    },
  });
  collector.bind(loginA);
  for (let i = 0; i < 200; i++) collector.record(i % 2 ? "offline" : "resume");
  assert.equal(collector.snapshot().events.length, 100);
  await collector.flush();
  assert.equal(calls, 1);
  assert.equal(collector.snapshot().events.length, 100);
  setOnline(false);
  await collector.flush();
  assert.equal(calls, 1);
  collector.dispose();
  t.mock.timers.tick(120000);
  assert.equal(calls, 1);
});
test("an in-flight upload acknowledges only its batch, not concurrent appends", async (t) => {
  let finish;
  const { collector } = setup(t, {
    fetch: async () => new Promise((resolve) => (finish = resolve)),
  });
  collector.bind(loginA);
  collector.record("offline");
  const sending = collector.flush();
  collector.record("resume");
  finish({ ok: true, status: 202 });
  await sending;
  assert.deepEqual(
    collector.snapshot().events.map((e) => e.pending),
    [false, true],
  );
});
test("refresh restores unsent sanitized events with original build and dedupe identifiers", async (t) => {
  const { collector, storage } = setup(t);
  collector.bind(loginA);
  collector.record("offline");
  const old = collector.snapshot();
  const requests = [];
  const next = createDiagnostics({
    csrf: () => "fixture",
    storage: () => storage,
    build: "app-newbuild.js",
    now: () => 1700000001000,
    context: () => ({ online: true, visible: true, standalone: true }),
    fetch: async (path, options) => {
      requests.push(JSON.parse(options.body));
      return { ok: true, status: 202 };
    },
  });
  t.after(() => next.dispose());
  next.bind(loginA);
  next.record("resume");
  await next.flush();
  await next.flush();
  assert.equal(requests[0].client, old.client);
  assert.equal(requests[0].build, "app-abc123.js");
  assert.equal(requests[0].events[0].sequence, old.events[0].sequence);
  assert.equal(requests[1].build, "app-newbuild.js");
});
test("blocked/corrupt storage and unauthorized upload never break application behavior", async (t) => {
  const { collector } = setup(t, {
    storage: () => {
      throw new Error("blocked");
    },
    fetch: async () => ({ ok: false, status: 401 }),
  });
  collector.bind(loginA);
  collector.record("offline");
  await collector.flush();
  collector.record("resume");
  assert.equal(collector.snapshot().events.length, 1);
  collector.dispose();
  collector.bind(loginB);
  collector.record("resume");
  assert.equal(collector.snapshot().events.length, 1);
});

test("sequence high-water mark survives expiry of all local records while server keeps seven days", async (t) => {
  const { collector, storage } = setup(t);
  collector.bind(loginA);
  collector.record("offline");
  const old = collector.snapshot();
  const requests = [];
  const next = createDiagnostics({
    csrf: () => "fixture",
    storage: () => storage,
    build: "app-new.js",
    now: () => 1700000000000 + 25 * 60 * 60 * 1000,
    context: () => ({ online: true, visible: true, standalone: false }),
    fetch: async (path, options) => {
      requests.push(JSON.parse(options.body));
      return { ok: true, status: 202 };
    },
  });
  t.after(() => next.dispose());
  next.bind(loginA);
  assert.equal(next.snapshot().events.length, 0);
  next.record("resume");
  await next.flush();
  assert.equal(requests[0].client, old.client);
  assert.ok(requests[0].events[0].sequence > old.events[0].sequence);
});
