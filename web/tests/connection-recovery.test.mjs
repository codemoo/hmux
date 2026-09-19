import test from "node:test";
import assert from "node:assert/strict";
import { createConnectionRecovery } from "../src/connection-recovery.ts";
import { releaseTerminalView } from "../src/terminal-session.ts";

test("brief successful connections do not reset repeated failure backoff", () => {
  let now = 0;
  const recovery = createConnectionRecovery(
    () => now,
    () => 0,
  );
  assert.equal(recovery.failed("network").retryMs, 1000);
  now += 1000;
  assert.equal(recovery.delay(), 0);
  recovery.ready();
  now += 200;
  assert.equal(recovery.failed("network").retryMs, 2000);
  now += 2000;
  recovery.ready();
  now += 30_000;
  assert.equal(recovery.failed("network").retryMs, 1000);
  for (let i = 0; i < 20; i++) recovery.failed("network");
  assert.equal(recovery.delay(), 60_000);
});

test("limit rejection waits; output pressure requires explicit retry", () => {
  const recovery = createConnectionRecovery(
    () => 0,
    () => 0,
  );
  assert.equal(recovery.failed("limit").retryMs, 10_000);
  assert.match(recovery.description(), /한도/);
  assert.equal(recovery.failed("output-overflow").retryMs, null);
  assert.equal(recovery.delay(), Infinity);
  recovery.released();
  assert.equal(recovery.delay(), Infinity);
  recovery.reset();
  assert.equal(recovery.delay(), 0);
  assert.equal(recovery.description(), "");
});

test("releasing a tab cancels open and retry timers before stale callbacks can run", async () => {
  let fired = 0,
    closed = 0;
  const tab = {
    generation: 1,
    status: "connecting",
    recovery: createConnectionRecovery(),
    retryTimer: setTimeout(() => fired++, 10),
    openTimer: setTimeout(() => fired++, 10),
    ws: {
      close() {
        assert.equal(tab.generation, 2);
        closed++;
      },
    },
  };
  releaseTerminalView(tab);
  await new Promise((r) => setTimeout(r, 25));
  assert.equal(fired, 0);
  assert.equal(closed, 1);
  assert.equal(tab.retryTimer, undefined);
  assert.equal(tab.openTimer, undefined);
  assert.equal(tab.status, "disconnected");
});
