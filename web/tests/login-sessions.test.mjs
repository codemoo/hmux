import test from "node:test";
import assert from "node:assert/strict";
import { installLoginSessions } from "../src/login-sessions.ts";

class Node {
  constructor(tag, doc) {
    this.tagName = tag;
    this.ownerDocument = doc;
    this.children = [];
    this.attrs = {};
  }
  append(...children) {
    for (const child of children) child.parent = this;
    this.children.push(...children);
  }
  remove() {
    this.parent.children = this.parent.children.filter(
      (child) => child !== this,
    );
  }
  replaceChildren(...children) {
    this.children = children;
  }
  setAttribute(key, value) {
    this.attrs[key] = value;
  }
  querySelectorAll(tag) {
    return this.children.flatMap((child) => [
      ...(child.tagName === tag ? [child] : []),
      ...child.querySelectorAll(tag),
    ]);
  }
}
const makeRoot = () => {
  const doc = { createElement: (tag) => new Node(tag, doc) };
  return new Node("section", doc);
};
const tick = () => new Promise((resolve) => setImmediate(resolve));
const entry = (id, current = false) => ({
  id,
  current,
  browser: "Safari · macOS",
  ip: "192.0.2.1",
  location: "Example City",
  created_at: "2026-09-12T00:00:00Z",
  last_seen_at: "2026-09-12T01:00:00Z",
  expires_at: "2026-09-19T00:00:00Z",
});
const revokeButtons = (root) =>
  root.querySelectorAll("button").filter((n) => n.className.includes("revoke"));

test("closing settings aborts outstanding list and ignores its late response", async () => {
  const root = makeRoot();
  let resolve, signal;
  const dispose = installLoginSessions(
    root,
    (_path, _body, s) => {
      signal = s;
      return new Promise((r) => (resolve = r));
    },
    () => assert.fail("unexpected logout"),
  );
  dispose();
  assert.equal(signal.aborted, true);
  resolve({ sessions: [entry("old-user")] });
  await tick();
  assert.equal(revokeButtons(root).length, 0);
});

test("remote revoke sends only selected ID, blocks duplicate clicks and refreshes list", async () => {
  const root = makeRoot();
  const calls = [];
  let finish;
  let listed = 0;
  const dispose = installLoginSessions(
    root,
    async (path, body) => {
      calls.push([path, body]);
      if (path.endsWith("/revoke")) return new Promise((r) => (finish = r));
      return {
        sessions:
          ++listed === 1
            ? [entry("self", true), entry("remote")]
            : [entry("self", true)],
      };
    },
    () => assert.fail("remote revocation logged out current browser"),
  );
  await tick();
  const button = revokeButtons(root)[1];
  const pending = button.onclick();
  await button.onclick();
  assert.equal(calls.filter(([p]) => p.endsWith("/revoke")).length, 1);
  assert.deepEqual(calls[1], ["/api/sessions/revoke", { id: "remote" }]);
  finish({ ok: true });
  await pending;
  assert.equal(revokeButtons(root).length, 1);
  dispose();
});

test("current revoke logs out only after success; failure leaves retry available", async () => {
  const root = makeRoot();
  let fail = true,
    loggedOut = 0;
  const dispose = installLoginSessions(
    root,
    async (path) => {
      if (!path.endsWith("/revoke")) return { sessions: [entry("self", true)] };
      if (fail) throw new Error("Could not persist revocation");
      return { ok: true };
    },
    () => loggedOut++,
  );
  await tick();
  const button = revokeButtons(root)[0];
  await button.onclick();
  assert.equal(loggedOut, 0);
  assert.equal(button.disabled, false);
  fail = false;
  await button.onclick();
  assert.equal(loggedOut, 1);
  dispose();
});

test("late self-revoke cannot sign out a later user after panel disposal", async () => {
  const root = makeRoot();
  let finish;
  const dispose = installLoginSessions(
    root,
    async (path) => {
      if (path.endsWith("/revoke")) return new Promise((r) => (finish = r));
      return { sessions: [entry("self", true)] };
    },
    () => assert.fail("cross-user logout"),
  );
  await tick();
  const pending = revokeButtons(root)[0].onclick();
  dispose();
  finish({ ok: true });
  await pending;
});

test("committed revoke remains successful when the list refresh fails", async () => {
  const root = makeRoot();
  let loaded = false;
  const dispose = installLoginSessions(
    root,
    async (path) => {
      if (path.endsWith("/revoke")) return { ok: true };
      if (loaded) throw new Error("offline");
      loaded = true;
      return { sessions: [entry("remote")] };
    },
    () => assert.fail("unexpected logout"),
  );
  await tick();
  await revokeButtons(root)[0].onclick();
  assert.equal(revokeButtons(root).length, 0);
  assert.ok(
    root
      .querySelectorAll("p")
      .some((node) => node.textContent.includes("Signed out")),
  );
  dispose();
});

test("current-browser revoke announces logout intent before waiting on the server", async () => {
  const root = makeRoot();
  let finish,
    started = 0,
    loggedOut = 0;
  const dispose = installLoginSessions(
    root,
    async (path) => {
      if (path.endsWith("/revoke"))
        return new Promise((resolve) => {
          finish = resolve;
        });
      return { sessions: [entry("self", true)] };
    },
    () => loggedOut++,
    () => started++,
  );
  await tick();
  const pending = revokeButtons(root)[0].onclick();
  assert.equal(started, 1);
  assert.equal(loggedOut, 0);
  finish({ ok: true });
  await pending;
  assert.equal(loggedOut, 1);
  dispose();
});
