import test from "node:test";
import assert from "node:assert/strict";
import {
  checkBootstrapRequired,
  installBootstrapSetup,
} from "../src/bootstrap-setup.ts";

class Node {
  constructor(tag, doc) {
    this.tagName = tag;
    this.ownerDocument = doc;
    this.children = [];
    this.attrs = {};
    this.isConnected = true;
    this.value = "";
    this.checked = false;
    this.disabled = false;
  }
  append(...children) {
    for (const child of children) {
      child.parent = this;
      child.setConnected(this.isConnected);
    }
    this.children.push(...children);
  }
  replaceChildren(...children) {
    for (const child of this.children) child.setConnected(false);
    this.children = [];
    this.append(...children);
  }
  setConnected(value) {
    this.isConnected = value;
    for (const child of this.children) child.setConnected(value);
  }
  setAttribute(key, value) {
    this.attrs[key] = value;
  }
  querySelectorAll(selector) {
    const tags = selector.split(",").map((value) => value.trim());
    return this.children.flatMap((child) => [
      ...(tags.includes(child.tagName) ? [child] : []),
      ...child.querySelectorAll(selector),
    ]);
  }
}
const root = () => {
  const doc = { createElement: (tag) => new Node(tag, doc) };
  return new Node("section", doc);
};
const find = (node, tag, name) =>
  node.querySelectorAll(tag).find((entry) => !name || entry.name === name);
const submit = (form) => form.onsubmit({ preventDefault() {} });

test("setup status accepts only an explicit requirement and keeps legacy fallback", async () => {
  const calls = [];
  const required = await checkBootstrapRequired(async (path, options) => {
    calls.push({ path, options });
    return { ok: true, status: 200, json: async () => ({ required: true }) };
  });
  assert.equal(required, true);
  assert.equal(calls[0].path, "/api/setup/status");
  assert.equal(calls[0].options.credentials, "same-origin");
  assert.equal(calls[0].options.cache, "no-store");
  assert.equal(
    await checkBootstrapRequired(async () => ({ ok: false, status: 404 })),
    false,
  );
  assert.equal(
    await checkBootstrapRequired(async () => {
      throw new TypeError("offline");
    }),
    false,
  );
});

test("setup sends a transient begin payload and clears password fields before TOTP", async () => {
  const mount = root();
  const calls = [];
  installBootstrapSetup(
    mount,
    async (path, body) => {
      calls.push({ path, body });
      return {
        complete: false,
        enrollment_id: "enrollment",
        totp_secret: "MANUAL-KEY",
        totp_uri: "otpauth://totp/secret",
      };
    },
    () => assert.fail("must wait for verification"),
  );
  const form = find(mount, "form");
  find(mount, "input", "token").value = "one-time-setup-code";
  find(mount, "input", "username").value = "first-admin";
  find(mount, "input", "password").value = "private password";
  find(mount, "input", "password_confirm").value = "private password";
  await submit(form);
  assert.deepEqual(calls[0], {
    path: "/api/setup/begin",
    body: {
      token: "one-time-setup-code",
      username: "first-admin",
      password: "private password",
      password_confirm: "private password",
      totp_enabled: true,
    },
  });
  assert.equal(find(mount, "input", "code").value, "");
  assert.equal(find(mount, "input").value, "MANUAL-KEY");
  const values = mount.querySelectorAll("input").map((entry) => entry.value);
  assert.ok(!values.includes("one-time-setup-code"));
  assert.ok(!values.includes("private password"));
});

test("setup verifies TOTP once, then clears all transient values on completion and disposal", async () => {
  const mount = root();
  const calls = [];
  let completed = 0;
  const dispose = installBootstrapSetup(
    mount,
    async (path, body) => {
      calls.push({ path, body });
      if (path.endsWith("begin"))
        return {
          complete: false,
          enrollment_id: "enrollment",
          totp_secret: "KEY",
        };
      return { complete: true };
    },
    () => completed++,
  );
  const begin = find(mount, "form");
  find(mount, "input", "token").value = "setup-code";
  find(mount, "input", "username").value = "admin";
  find(mount, "input", "password").value = "private password";
  find(mount, "input", "password_confirm").value = "private password";
  await submit(begin);
  const verify = find(mount, "form");
  find(mount, "input", "code").value = "123456";
  await submit(verify);
  assert.deepEqual(calls[1], {
    path: "/api/setup/complete",
    body: { token: "setup-code", enrollment_id: "enrollment", code: "123456" },
  });
  assert.equal(completed, 1);
  dispose();
  assert.equal(mount.children.length, 0);
});

test("starting over clears the old enrollment before a replacement setup request", async () => {
  const mount = root();
  const calls = [];
  installBootstrapSetup(
    mount,
    async (path, body) => {
      calls.push({ path, body });
      if (path.endsWith("begin"))
        return {
          complete: false,
          enrollment_id:
            calls.length === 1 ? "old-enrollment" : "new-enrollment",
          totp_secret: "KEY",
        };
      return { complete: true };
    },
    () => {},
  );
  const begin = find(mount, "form");
  find(mount, "input", "token").value = "old-code";
  find(mount, "input", "username").value = "admin";
  find(mount, "input", "password").value = "private password";
  find(mount, "input", "password_confirm").value = "private password";
  await submit(begin);
  find(mount, "button").onclick();
  assert.equal(find(mount, "input", "token").value, "");
  assert.ok(
    !mount.querySelectorAll("input").some((entry) => entry.value === "KEY"),
  );
  const replacement = find(mount, "form");
  find(mount, "input", "token").value = "new-code";
  find(mount, "input", "username").value = "admin";
  find(mount, "input", "password").value = "private password";
  find(mount, "input", "password_confirm").value = "private password";
  await submit(replacement);
  const verify = find(mount, "form");
  find(mount, "input", "code").value = "123456";
  await submit(verify);
  assert.deepEqual(calls[2], {
    path: "/api/setup/complete",
    body: {
      token: "new-code",
      enrollment_id: "new-enrollment",
      code: "123456",
    },
  });
});

test("setup rejects byte-short passwords before making a request", async () => {
  const mount = root();
  let called = false;
  installBootstrapSetup(
    mount,
    async () => (called = true),
    () => {},
  );
  const form = find(mount, "form");
  find(mount, "input", "token").value = "setup-code";
  find(mount, "input", "username").value = "admin";
  find(mount, "input", "password").value = "short";
  find(mount, "input", "password_confirm").value = "short";
  await submit(form);
  assert.equal(called, false);
  assert.ok(
    mount
      .querySelectorAll("p")
      .some((entry) => entry.textContent.includes("8–128")),
  );
});
