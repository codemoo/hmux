import test from "node:test";
import assert from "node:assert/strict";
import { createSessionAPI } from "../src/session-api.ts";

const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
};
const ok = (body) => ({ ok: true, json: async () => body });

test("late old-login 401 cannot expire the new login, even if fetch ignores abort", async () => {
  const pending = deferred();
  let calls = 0,
    expired = 0,
    oldSignal;
  const api = createSessionAPI({
    csrf: () => "fixture",
    unauthorized: () => expired++,
    fetch: async (_path, options) => {
      if (++calls === 1) {
        oldSignal = options.signal;
        return pending.promise;
      }
      return ok({ account: "new" });
    },
  });
  const old = api.request("/api/state");
  api.reset();
  assert.equal(oldSignal.aborted, true);
  assert.deepEqual(await api.request("/api/session"), { account: "new" });
  pending.resolve({ ok: false, status: 401 });
  await assert.rejects(old, { name: "AbortError" });
  assert.equal(expired, 0);
});

test("delayed successful response body cannot cross login boundaries", async () => {
  const body = deferred();
  const entered = deferred();
  const api = createSessionAPI({
    csrf: () => "",
    unauthorized: () => assert.fail("unexpected logout"),
    fetch: async () => ({
      ok: true,
      json: () => {
        entered.resolve();
        return body.promise;
      },
    }),
  });
  const old = api.request("/api/state");
  await entered.promise;
  api.reset();
  body.resolve({ catalog: { sessions: [{ id: "$901", created_at: 1 }] } });
  await assert.rejects(old, { name: "AbortError" });
});

test("current authentication failure expires once; transient failures preserve login", async () => {
  let expired = 0;
  let status = 502;
  const api = createSessionAPI({
    csrf: () => "",
    unauthorized: () => {
      expired++;
      api.reset();
    },
    fetch: async () => ({
      ok: false,
      status,
      text: async () => "temporary failure",
    }),
  });
  await assert.rejects(api.request("/api/state"), /temporary failure/);
  assert.equal(expired, 0);
  status = 401;
  await assert.rejects(api.request("/api/login", {}), /temporary failure/);
  assert.equal(expired, 0);
  await assert.rejects(api.request("/api/session"), { name: "AbortError" });
  assert.equal(expired, 1);
});

test("dialog cancellation does not cancel the account, and stale fetch rejection stays an abort", async () => {
  const pending = deferred();
  const parent = new AbortController();
  let calls = 0;
  const api = createSessionAPI({
    csrf: () => "",
    unauthorized: () => assert.fail("unexpected logout"),
    fetch: async () => (++calls === 1 ? pending.promise : ok({ ok: true })),
  });
  const request = api.request("/api/sessions", undefined, parent.signal);
  parent.abort();
  pending.reject(new TypeError("network lost"));
  await assert.rejects(request, { name: "AbortError" });
  assert.deepEqual(await api.request("/api/session"), { ok: true });
});

test("diagnostics report current request metadata without error contents and cannot affect API errors", async () => {
  const reports = [];
  const api = createSessionAPI({
    csrf: () => "",
    unauthorized: () => {},
    failure: (event) => {
      reports.push(event);
      throw Error("logger failure");
    },
    fetch: async () => ({ ok: false, status: 503, text: async () => "SECRET" }),
  });
  await assert.rejects(api.request("/api/state"), /Temporarily unavailable/);
  assert.equal(reports[0].status, 503);
  assert.equal(reports[0].reason, "http");
  assert.ok(!JSON.stringify(reports).includes("SECRET"));
  await assert.rejects(
    api.request("/api/diagnostics"),
    /Temporarily unavailable/,
  );
  assert.equal(reports.length, 1);
});

const busy = (retry = "0") =>
  new Response("Service Unavailable", {
    status: 503,
    headers: { "Retry-After": retry },
  });

test("a busy read-only action recovers without reporting a failure", async () => {
  const requests = [],
    failures = [];
  const api = createSessionAPI({
    csrf: () => "fixture",
    unauthorized: () => assert.fail("unexpected logout"),
    failure: (event) => failures.push(event),
    fetch: async (_path, options) => {
      requests.push(options.body);
      return requests.length < 3 ? busy() : ok({ messages: [] });
    },
  });
  const body = {
    operation: "conversation",
    session: { id: "$1", created_at: 1 },
  };
  const pending = api.request("/api/action", body);
  body.session.id = "$2";
  assert.deepEqual(await pending, { messages: [] });
  assert.equal(requests.length, 3);
  assert.ok(requests.every((value) => JSON.parse(value).session.id === "$1"));
  assert.deepEqual(failures, []);
});

test("busy retries are finite and longer server backoffs are respected", async () => {
  for (const retry of ["0", "20"]) {
    let calls = 0;
    const failures = [];
    const api = createSessionAPI({
      csrf: () => "fixture",
      unauthorized: () => assert.fail("unexpected logout"),
      failure: (event) => failures.push(event),
      fetch: async () => {
        calls++;
        return busy(retry);
      },
    });
    await assert.rejects(
      api.request("/api/action", { operation: "conversation" }),
      /Temporarily unavailable/,
    );
    assert.equal(calls, retry === "0" ? 3 : 1);
    assert.equal(failures.length, 1);
    assert.equal(failures[0].status, 503);
  }
});

test("mutations and unknown actions are never replayed after a busy response", async () => {
  for (const body of [
    { operation: "create", payload: { name: "work" } },
    { operation: "workspace", payload: { change: { operation_id: "test" } } },
    { operation: "provider-job-start" },
    { operation: "alias" },
    { operation: "future-operation" },
  ]) {
    let calls = 0;
    const api = createSessionAPI({
      csrf: () => "fixture",
      unauthorized: () => {},
      fetch: async () => {
        calls++;
        return busy();
      },
    });
    await assert.rejects(api.request("/api/action", body));
    assert.equal(calls, 1);
  }
});

test("tab cancellation and account reset cancel retry waits without replay", async () => {
  for (const reset of [false, true]) {
    let calls = 0;
    const failures = [];
    const parent = new AbortController();
    const api = createSessionAPI({
      csrf: () => "fixture",
      unauthorized: () => {},
      failure: (event) => failures.push(event),
      fetch: async () => {
        calls++;
        return busy("1");
      },
    });
    const pending = api.request(
      "/api/action",
      { operation: "conversation" },
      parent.signal,
    );
    await new Promise((resolve) => setImmediate(resolve));
    if (reset) api.reset();
    else parent.abort();
    await assert.rejects(pending, { name: "AbortError" });
    assert.equal(calls, 1);
    assert.deepEqual(failures, []);
  }
});
