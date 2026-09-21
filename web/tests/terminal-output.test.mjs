import test from "node:test";
import assert from "node:assert/strict";
import { createTerminalOutput } from "../src/terminal-output.ts";

test("abandoned socket output retains its budget until xterm drains before reconnect", () => {
  const callbacks = [];
  let drains = 0;
  const output = createTerminalOutput(
    (data, done) => callbacks.push(done),
    () => drains++,
  );
  const chunk = new Uint8Array(512 << 10);
  assert.equal(output.enqueue(chunk), true);
  assert.equal(output.enqueue(chunk), true);
  assert.equal(output.enqueue(new Uint8Array(1)), false);
  // A closed socket does not reset this terminal-owned queue. A reconnect must
  // remain gated until both callbacks run, even after its cooldown expires.
  assert.equal(output.pending(), 1 << 20);
  callbacks.shift()();
  assert.equal(output.pending(), 512 << 10);
  assert.equal(drains, 0);
  callbacks.shift()();
  assert.equal(output.pending(), 0);
  assert.equal(drains, 1);
  assert.equal(output.enqueue(chunk), true);
  callbacks.shift()();
  assert.equal(output.pending(), 0);
  assert.equal(drains, 2);
});
