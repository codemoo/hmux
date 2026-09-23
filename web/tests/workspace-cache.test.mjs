import test from "node:test";
import assert from "node:assert/strict";
import {
  workspaceCacheKey,
  encodeWorkspaceCache,
  decodeWorkspaceCache,
} from "../src/workspace-cache.ts";
const id = { id: "$1", created_at: 42 };
const session = {
  ...id,
  name: "작업",
  alias: "표시",
  runtime: "claude",
  current_path: "/private",
  state: "working",
  token: "secret",
};
test("cache whitelists display metadata and preserves tab identity", () => {
  const raw = encodeWorkspaceCache([session], [id], id, 1000);
  assert.doesNotMatch(raw, /private|secret|working|current_path/);
  const v = decodeWorkspaceCache(raw, 1001);
  assert.equal(v.sessions[0].name, "작업");
  assert.deepEqual(v.tabs, [id]);
  assert.deepEqual(v.active, id);
});
test("cache separates accounts and profiles", () => {
  assert.notEqual(workspaceCacheKey("a", ""), workspaceCacheKey("b", ""));
  assert.notEqual(workspaceCacheKey("a", "p"), workspaceCacheKey("a", ""));
});
test("cache rejects malformed, expired, future and oversized input", () => {
  const raw = encodeWorkspaceCache([session], [id], id, 1000);
  for (const bad of [
    "{",
    "null",
    raw.repeat(10000),
    JSON.stringify({ ...JSON.parse(raw), saved: 2000 }),
    JSON.stringify({
      ...JSON.parse(raw),
      tabs: [{ id: "$1", created_at: -1 }],
    }),
  ])
    assert.equal(decodeWorkspaceCache(bad, 1001), undefined);
  assert.equal(decodeWorkspaceCache(raw, 1000 + 8 * 24 * 3600000), undefined);
});
test("cache bounds retained sessions and tabs", () => {
  const raw = encodeWorkspaceCache(
    Array(300).fill(session),
    Array(40).fill(id),
    id,
    1000,
  );
  const v = decodeWorkspaceCache(raw, 1000);
  assert.equal(v.sessions.length, 256);
  assert.equal(v.tabs.length, 32);
});
