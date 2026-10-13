import test from "node:test";
import assert from "node:assert/strict";
import {
  createTabCompletions,
  parseCompletionState,
} from "../src/tab-completions.ts";

const session = { id: "$1", created_at: 100 };
const eventID = (n) => n.toString(16).padStart(64, "0");
const state = (sequence = 1, read_revision = 0, unread = true) => ({
  version: 1,
  sequence,
  read_revision,
  entries: [
    {
      session,
      event_id: eventID(sequence),
      completed_at: "2026-10-13T00:00:00Z",
      unread,
    },
  ],
});
const view = (changes = {}) => ({
  session,
  visible: true,
  focused: true,
  ready: true,
  ...changes,
});
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
};

test("completion snapshot rejects malformed, duplicate and oversized identities", () => {
  assert.ok(parseCompletionState(state()));
  for (const bad of [
    null,
    {},
    { ...state(), sequence: NaN },
    { ...state(), read_revision: -1 },
    { ...state(), entries: [state().entries[0], state().entries[0]] },
    {
      ...state(),
      entries: [
        { ...state().entries[0], session: { ...session, created_at: 0 } },
      ],
    },
    { ...state(), entries: Array(257).fill(state().entries[0]) },
    { ...state(), entries: [{ ...state().entries[0], event_id: "invalid" }] },
  ]) {
    assert.equal(parseCompletionState(bad), undefined);
  }
});

test("cached selection, hidden page, lost focus and unrendered terminal cannot acknowledge", async () => {
  const calls = [];
  const tracker = createTabCompletions(
    async (...args) => {
      calls.push(args);
    },
    () => {},
  );
  tracker.apply(state());
  for (const changes of [
    { visible: false },
    { focused: false },
    { ready: false },
    { session: { ...session, created_at: 101 } },
    { event_id: eventID(2) },
  ]) {
    await tracker.read(view(changes));
  }
  assert.equal(calls.length, 0);
  assert.equal(tracker.unread(session), true);
});

test("read waits for server success and stale poll cannot restore cleared marker", async () => {
  const reply = deferred();
  const calls = [];
  const tracker = createTabCompletions(
    async (path, body) => {
      calls.push({ path, body });
      return reply.promise;
    },
    () => {},
  );
  tracker.apply(state());
  const pending = tracker.read(view());
  await tracker.read(view());
  assert.equal(calls.length, 1);
  assert.equal(tracker.unread(session), true);
  assert.deepEqual(calls[0], {
    path: "/api/completions/read",
    body: { session, event_id: eventID(1) },
  });
  reply.resolve(state(1, 1, false));
  await pending;
  assert.equal(tracker.unread(session), false);
  tracker.apply(state(1, 0, true));
  assert.equal(tracker.unread(session), false);
});

test("older ACK cannot hide new completion and a different device shares server read state", async () => {
  const reply = deferred();
  const first = createTabCompletions(
    () => reply.promise,
    () => {},
  );
  const second = createTabCompletions(
    async () => state(2, 2, false),
    () => {},
  );
  first.apply(state());
  second.apply(state());
  const pending = first.read(view());
  first.apply(state(2, 0, true));
  second.apply(state(2, 0, true));
  reply.resolve(state(1, 1, false));
  await pending;
  assert.equal(first.unread(session), true);
  await second.read(view());
  first.apply(state(2, 2, false));
  assert.equal(first.unread(session), false);
  assert.equal(second.unread(session), false);
});

test("failed read remains unread and existing next poll can retry without a new loop", async () => {
  let attempts = 0;
  const tracker = createTabCompletions(
    async () => {
      if (++attempts === 1) throw new Error("offline");
      return state(1, 1, false);
    },
    () => {},
  );
  tracker.apply(state());
  await tracker.read(view());
  assert.equal(tracker.unread(session), true);
  tracker.apply(state());
  await tracker.read(view());
  assert.equal(tracker.unread(session), false);
  assert.equal(attempts, 2);
});

test("logout aborts ACK and ignores replies from the previous account", async () => {
  const reply = deferred();
  let signal;
  let renders = 0;
  const old = createTabCompletions(
    async (path, body, s) => {
      signal = s;
      return reply.promise;
    },
    () => renders++,
  );
  old.apply(state());
  const pending = old.read(view());
  old.dispose();
  assert.equal(signal.aborted, true);
  reply.resolve(state(1, 1, false));
  await pending;
  assert.equal(renders, 1);
  assert.equal(old.unread(session), false);
  const next = createTabCompletions(
    async () => state(1, 1, false),
    () => {},
  );
  next.apply(state());
  assert.equal(next.unread(session), true);
});
