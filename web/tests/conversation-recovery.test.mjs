import test from "node:test";
import assert from "node:assert/strict";
import { loadConversation } from "../src/conversation-recovery.ts";
import { createSessionAPI, APIRequestError } from "../src/session-api.ts";
const ready = { status: "ready", messages: [], truncated: false };
const missing = { status: "unavailable", messages: [], truncated: false };
const options = (request, extra = {}) => ({
  request,
  signal: new AbortController().signal,
  retrying: () => {},
  wait: async () => {},
  ...extra,
});

test("one foreground cycle heals network, missing binding and busy responses without nested retries", async () => {
  let calls = 0;
  const requests = [],
    waits = [],
    notices = [];
  const api = createSessionAPI({
    csrf: () => "synthetic",
    unauthorized: () => assert.fail("unexpected expiry"),
    fetch: async (_path, init) => {
      requests.push(JSON.parse(init.body));
      calls++;
      if (calls === 1)
        return new Response("busy", {
          status: 503,
          headers: { "Retry-After": "2" },
        });
      return new Response(JSON.stringify(calls === 2 ? missing : ready));
    },
  });
  const identity = { id: "$7", created_at: 17 };
  const result = await loadConversation(
    options(
      (signal) =>
        api.request(
          "/api/action",
          { operation: "conversation", session: identity },
          signal,
        ),
      {
        wait: async (ms) => waits.push(ms),
        retrying: (n, max) => notices.push([n, max]),
      },
    ),
  );
  assert.equal(result.status, "ready");
  assert.equal(calls, 3);
  assert.deepEqual(waits, [2000, 2000]);
  assert.deepEqual(notices, [
    [2, 3],
    [3, 3],
  ]);
  assert.ok(
    requests.every(
      (r) =>
        r.operation === "conversation" &&
        r.session.id === "$7" &&
        r.session.created_at === 17 &&
        r.payload === undefined,
    ),
  );
});

test("network and individual deadline failures recover; persistent missing binding has a finite end", async () => {
  let calls = 0;
  assert.deepEqual(
    await loadConversation(
      options(async () => {
        calls++;
        if (calls === 1) throw new TypeError("fetch failed");
        if (calls === 2) throw new DOMException("deadline", "TimeoutError");
        return ready;
      }),
    ),
    ready,
  );
  assert.equal(calls, 3);
  calls = 0;
  assert.deepEqual(
    await loadConversation(
      options(async () => {
        calls++;
        return missing;
      }),
    ),
    missing,
  );
  assert.equal(calls, 3);
});

test("ambiguous bindings, protocol errors and permanent HTTP failures are never replayed", async () => {
  for (const failure of [
    new SyntaxError("bad JSON"),
    new APIRequestError("forbidden", 403),
    new APIRequestError("bad", 400),
    new APIRequestError("invalid Home protocol", 502),
    new Error("protocol"),
    new DOMException("expiry", "AbortError"),
  ]) {
    let calls = 0;
    await assert.rejects(
      loadConversation(
        options(async () => {
          calls++;
          throw failure;
        }),
      ),
      (e) => e === failure,
    );
    assert.equal(calls, 1);
  }
  let calls = 0;
  const ambiguous = { ...missing, status: "ambiguous" };
  assert.deepEqual(
    await loadConversation(
      options(async () => {
        calls++;
        return ambiguous;
      }),
    ),
    ambiguous,
  );
  assert.equal(calls, 1);
});

test("long or invalid server backoffs stop this cycle instead of being shortened", async () => {
  for (const delay of [20_000, Infinity]) {
    let calls = 0;
    const failure = new APIRequestError("busy", 503, delay);
    await assert.rejects(
      loadConversation(
        options(
          async () => {
            calls++;
            throw failure;
          },
          { wait: async () => assert.fail("backoff shortened") },
        ),
      ),
      (e) => e === failure,
    );
    assert.equal(calls, 1);
  }
});

test("leaving the reader cancels pending recovery and ignores a late success", async () => {
  const owner = new AbortController();
  let calls = 0,
    waitSignal;
  const waiting = new Promise((resolve) => {
    waitSignal = resolve;
  });
  const pending = loadConversation(
    options(
      async () => {
        calls++;
        return missing;
      },
      {
        signal: owner.signal,
        wait: async (_ms, signal) => {
          waitSignal();
          return new Promise((_resolve, reject) =>
            signal.addEventListener("abort", () => reject(signal.reason), {
              once: true,
            }),
          );
        },
      },
    ),
  );
  await waiting;
  owner.abort();
  await assert.rejects(pending, { name: "AbortError" });
  assert.equal(calls, 1);
  let resolve;
  const late = new Promise((done) => {
    resolve = done;
  });
  const changed = new AbortController();
  const obsolete = loadConversation(
    options(async () => late, { signal: changed.signal }),
  );
  changed.abort();
  resolve(ready);
  await assert.rejects(obsolete, { name: "AbortError" });
});

test("account expiry during recovery expires once and never replays into a new login", async () => {
  let calls = 0,
    expired = 0;
  const api = createSessionAPI({
    csrf: () => "fixture",
    unauthorized: () => {
      expired++;
      api.reset();
    },
    fetch: async () => {
      calls++;
      return new Response("failure", { status: calls === 1 ? 504 : 401 });
    },
  });
  await assert.rejects(
    loadConversation(
      options((signal) =>
        api.request("/api/action", { operation: "conversation" }, signal),
      ),
    ),
    { name: "AbortError" },
  );
  assert.equal(calls, 2);
  assert.equal(expired, 1);
});

test("browser fetch AbortError on request timeout retains timeout recovery; total deadline stops the cycle", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  let calls = 0;
  const api = createSessionAPI({
    csrf: () => "fixture",
    unauthorized: () => {},
    fetch: async (_path, init) => {
      calls++;
      if (calls === 2) return new Response(JSON.stringify(ready));
      return new Promise((_resolve, reject) =>
        init.signal.addEventListener(
          "abort",
          () => reject(new DOMException("fetch aborted", "AbortError")),
          { once: true },
        ),
      );
    },
  });
  const pending = loadConversation(
    options((signal) =>
      api.request("/api/action", { operation: "conversation" }, signal),
    ),
  );
  t.mock.timers.tick(30_000);
  assert.equal((await pending).status, "ready");
  assert.equal(calls, 2);
  let reads = 0;
  const infinite = loadConversation(
    options(async (signal) => {
      reads++;
      return new Promise((_resolve, reject) =>
        signal.addEventListener("abort", () => reject(signal.reason), {
          once: true,
        }),
      );
    }),
  );
  t.mock.timers.tick(45_000);
  await assert.rejects(infinite, { name: "TimeoutError" });
  assert.equal(reads, 1);
});

test("malformed conversation payload stops recovery immediately", async () => {
  for (const value of [
    null,
    {},
    { status: "ready", messages: null, truncated: false },
  ]) {
    let calls = 0;
    await assert.rejects(
      loadConversation(
        options(async () => {
          calls++;
          return value;
        }),
      ),
      /Invalid conversation response/,
    );
    assert.equal(calls, 1);
  }
});
