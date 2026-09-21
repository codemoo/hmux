import test from "node:test";
import assert from "node:assert/strict";
import { createTerminalHeartbeat } from "../src/terminal-heartbeat.ts";

test("a silent socket expires once even if WebSocket still reports OPEN", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  let expired = 0;
  const heartbeat = createTerminalHeartbeat(() => expired++);
  t.mock.timers.tick(19_999);
  assert.equal(expired, 0);
  t.mock.timers.tick(1);
  assert.equal(expired, 1);
  heartbeat.received();
  t.mock.timers.tick(40_000);
  assert.equal(expired, 1);
});

test("heartbeat or terminal output keeps an idle healthy view alive; disposal rejects late messages", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  let expired = 0;
  const heartbeat = createTerminalHeartbeat(() => expired++);
  for (let i = 0; i < 12; i++) {
    t.mock.timers.tick(5000);
    heartbeat.received();
  }
  assert.equal(expired, 0);
  heartbeat.dispose();
  heartbeat.received();
  t.mock.timers.tick(60_000);
  assert.equal(expired, 0);
});
