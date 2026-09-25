import test from "node:test";
import assert from "node:assert/strict";
import { installProviderSettings } from "../src/provider-settings.ts";

class Element {
  constructor(tag, doc) {
    this.tagName = tag;
    this.ownerDocument = doc;
    this.children = [];
    this.attrs = {};
    this.dataset = {};
    this.open = false;
    this.value = "";
  }
  append(...children) {
    this.children.push(...children);
  }
  replaceChildren(...children) {
    this.children = children;
    this._text = "";
  }
  setAttribute(name, value) {
    this.attrs[name] = value;
  }
  get textContent() {
    return (
      (this._text || "") + this.children.map((n) => n.textContent).join("")
    );
  }
  set textContent(value) {
    this._text = value;
    this.children = [];
  }
  querySelectorAll(tag) {
    return this.children.flatMap((n) => [
      ...(n.tagName === tag ? [n] : []),
      ...n.querySelectorAll(tag),
    ]);
  }
}
const provider = (extra = {}) => ({
  id: "codex",
  label: "Codex",
  installed: true,
  version: "1.0",
  auth: "account",
  key_hint: "",
  profile: true,
  profile_id: "work-codex",
  ...extra,
});
const tick = () => new Promise((resolve) => setImmediate(resolve));
function fixture(
  initial,
  mutate = () => {
    throw Error("unexpected mutation");
  },
) {
  const doc = { createElement: (tag) => new Element(tag, doc) };
  const root = new Element("section", doc);
  const calls = [],
    started = [];
  const dispose = installProviderSettings(
    root,
    async (_path, body) => {
      calls.push(body);
      if (body.operation === "providers") return { providers: [initial] };
      if (body.operation === "provider-job") return { job: { state: "none" } };
      return mutate(body);
    },
    (id) => started.push(id),
  );
  return { root, calls, started, dispose };
}
const button = (root, label) =>
  root.querySelectorAll("button").find((n) => n.textContent === label);

test("existing Home login is ready without a key or another login; alternate auth is disclosed", async () => {
  const f = fixture(provider());
  await tick();
  assert.match(f.root.textContent, /No API key is needed/);
  const change = f.root
    .querySelectorAll("details")
    .find((n) => n.className === "provider-auth-options");
  assert.equal(change.open, false);
  const keyPanel = f.root
    .querySelectorAll("details")
    .find((n) => n.className === "provider-key-panel");
  assert.equal(keyPanel.open, false);
  change.open = true;
  assert.deepEqual(
    f.calls.map((c) => c.operation),
    ["providers", "provider-job"],
  );
  button(f.root, "Start new session").onclick();
  assert.deepEqual(f.started, ["work-codex"]);
  f.dispose();
});

test("adding an existing CLI uses only the registration action and returned profile identity", async () => {
  const f = fixture(provider({ profile: false, profile_id: "" }), (body) => {
    assert.deepEqual(body, {
      operation: "provider-job-start",
      payload: { provider: "codex", action: "use-existing" },
    });
    return { providers: [provider({ profile_id: "server-owned-id" })] };
  });
  await tick();
  assert.equal(button(f.root, "Start new session"), undefined);
  button(f.root, "Add to session menu").onclick();
  await tick();
  button(f.root, "Start new session").onclick();
  assert.deepEqual(f.started, ["server-owned-id"]);
  assert.equal(
    f.calls.filter((c) => c.operation === "provider-job-start").length,
    1,
  );
  f.dispose();
});

test("API key authentication does not offer a conflicting login and removal is explicit", async () => {
  const f = fixture(
    provider({ auth: "api-key", key_hint: "…abcd" }),
    (body) => {
      assert.deepEqual(body, {
        operation: "provider-key",
        payload: { provider: "codex", key: "" },
      });
      return { providers: [provider({ auth: "none", key_hint: "" })] };
    },
  );
  await tick();
  assert.equal(button(f.root, "Sign in to CLI again"), undefined);
  assert.equal(button(f.root, "Sign in to CLI"), undefined);
  assert.match(f.root.textContent, /billed separately/);
  button(f.root, "Remove saved API key").onclick();
  await tick();
  assert.ok(button(f.root, "Sign in to CLI"));
  f.dispose();
});

test("unknown-source keys are not advertised as removable and drafts clear on disposal", async () => {
  const f = fixture(provider({ auth: "api-key", key_hint: "" }));
  await tick();
  assert.equal(button(f.root, "Remove saved API key"), undefined);
  assert.match(f.root.textContent, /Manage it on Home/);
  const input = f.root.querySelectorAll("input")[0];
  input.value = "synthetic-draft";
  f.dispose();
  assert.equal(input.value, "");
});
