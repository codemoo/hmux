import test from "node:test";
import assert from "node:assert/strict";
import { validateWorkspace, sameIdentity } from "../src/shared-workspace.ts";
const a = { id: "$1", created_at: 100 };
const b = { id: "$2", created_at: 101 };
const snapshot = {
  version: 1,
  initialized: true,
  revision: 3,
  tabs: [b, a],
  selected: a,
};
test("shared layout preserves exact identity and order", () => {
  assert.equal(validateWorkspace(snapshot), snapshot);
  assert.equal(sameIdentity(a, { ...a, created_at: 101 }), false);
});
test("invalid layout does not get applied", () => {
  for (const change of [
    { version: 2 },
    { revision: -1 },
    { revision: 2 ** 53 },
    { tabs: [a, a] },
    { tabs: [a], selected: b },
    { tabs: [{ ...a, id: "shell; command" }] },
    { tabs: [{ ...a, created_at: 0 }] },
  ]) {
    assert.throws(() => validateWorkspace({ ...snapshot, ...change }));
  }
});
