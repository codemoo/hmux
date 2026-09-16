import test from "node:test";
import assert from "node:assert/strict";
import { installAccountSecurity } from "../src/account-security.ts";
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

const control = (root) => ({
  toggle: root
    .querySelectorAll("button")
    .find((n) => n.className === "security-switch"),
  form: root.querySelectorAll("form")[0],
  password: root.querySelectorAll("input").find((n) => n.name === "password"),
  code: root.querySelectorAll("input").find((n) => n.name === "code"),
});
const submit = (c) => c.form.onsubmit({ preventDefault() {} });

test("toggle waits for confirmed reauth; submits no account selector and clears secrets", async () => {
  const root = makeRoot();
  const calls = [];
  let finish;
  let changed = 0;
  const dispose = installAccountSecurity(
    root,
    async (path, body) => {
      calls.push({ path, body });
      if (body) return new Promise((r) => (finish = r));
      return { totp_enabled: true };
    },
    () => changed++,
  );
  await tick();
  const c = control(root);
  assert.equal(c.toggle.attrs["aria-checked"], "true");
  c.toggle.onclick();
  assert.equal(c.form.hidden, false);
  c.password.value = "private password";
  c.code.value = "123456";
  const pending = submit(c);
  await submit(c);
  assert.equal(calls.length, 2);
  assert.equal(c.toggle.attrs["aria-checked"], "true");
  assert.deepEqual(calls[1], {
    path: "/api/account/security",
    body: { totp_enabled: false, password: "private password", code: "123456" },
  });
  finish({ totp_enabled: false });
  await pending;
  assert.equal(changed, 1);
  assert.equal(c.toggle.attrs["aria-checked"], "false");
  assert.equal(c.form.hidden, true);
  assert.equal(c.password.value, "");
  assert.equal(c.code.value, "");
  dispose();
});

test("closing settings aborts and ignores late security settings from prior account", async () => {
  const root = makeRoot();
  let finish, signal;
  const dispose = installAccountSecurity(
    root,
    (_p, _b, s) => {
      signal = s;
      return new Promise((r) => (finish = r));
    },
    () => assert.fail(),
  );
  dispose();
  finish({ totp_enabled: true });
  await tick();
  assert.equal(signal.aborted, true);
  assert.equal(control(root).toggle.disabled, true);
});

test("late mutation after disposal cannot update a later account or retain credentials", async () => {
  const root = makeRoot();
  let finish;
  const dispose = installAccountSecurity(
    root,
    async (_p, b) =>
      b ? new Promise((r) => (finish = r)) : { totp_enabled: false },
    () => assert.fail("late callback"),
  );
  await tick();
  const c = control(root);
  c.toggle.onclick();
  c.password.value = "secret";
  c.code.value = "123456";
  const pending = submit(c);
  dispose();
  assert.equal(c.password.value, "");
  assert.equal(c.code.value, "");
  finish({ totp_enabled: true });
  await pending;
  assert.equal(c.toggle.attrs["aria-checked"], "false");
});

test("uncertain mutation re-reads committed state; wrong credentials stay editable", async () => {
  for (const committed of [true, false]) {
    const root = makeRoot();
    let reads = 0,
      changed = 0;
    const dispose = installAccountSecurity(
      root,
      async (_p, b) => {
        if (b) throw Error("Request failed");
        return { totp_enabled: ++reads === 1 ? true : !committed };
      },
      () => changed++,
    );
    await tick();
    const c = control(root);
    c.toggle.onclick();
    c.password.value = "secret";
    c.code.value = "123456";
    await submit(c);
    assert.equal(c.password.value, "");
    assert.equal(c.code.value, "");
    assert.equal(c.toggle.disabled, false);
    assert.equal(c.toggle.attrs["aria-checked"], String(!committed));
    assert.equal(c.form.hidden, committed);
    assert.equal(changed, Number(committed));
    dispose();
  }
});
