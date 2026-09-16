import test from "node:test";
import assert from "node:assert/strict";
import { releaseTerminalView } from "../src/terminal-session.ts";
test("release rejects stale callbacks before closing and preserves terminal", () => {
  let closed = 0;
  const term = {
    dispose() {
      assert.fail("view release disposed terminal");
    },
  };
  const tab = { generation: 7, status: "connected", term };
  tab.ws = {
    close() {
      assert.equal(tab.generation, 8);
      closed++;
    },
  };
  releaseTerminalView(tab);
  assert.equal(closed, 1);
  assert.equal(tab.ws, undefined);
  assert.equal(tab.status, "disconnected");
  assert.equal(tab.term, term);
  releaseTerminalView(tab);
  assert.equal(closed, 1);
});
