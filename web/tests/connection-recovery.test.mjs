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
  now += 10_000;
  assert.equal(recovery.failed("network").retryMs, 1000);
  for (let i = 0; i < 20; i++) recovery.failed("network");
  assert.equal(recovery.delay(), 15_000);
});

test("capacity and output pressure recover automatically without bypassing cooldown on resume", () => {
  let now = 0;
  const recovery = createConnectionRecovery(
    () => now,
    () => 0,
  );
  assert.equal(recovery.failed("limit").retryMs, 10_000);
  assert.match(recovery.description(), /한도/);
  recovery.resume();
  assert.equal(recovery.delay(), 10_000);
  assert.equal(recovery.failed("output-overflow").retryMs, 10_000);
  assert.equal(recovery.delay(), 10_000);
  recovery.released();
  recovery.resume();
  assert.equal(recovery.delay(), 10_000);
  now += 10_000;
  assert.equal(recovery.delay(), 0);
  for (let i = 0; i < 20; i++) recovery.failed("output-overflow");
  assert.equal(recovery.delay(), 60_000);
  recovery.reset();
  assert.equal(recovery.delay(), 0);
  assert.equal(recovery.description(), "");
});

test("foreground/network resume expedites transient failures; ordinary polling cannot", () => {
  const recovery = createConnectionRecovery(
    () => 0,
    () => 0,
  );
  for (const kind of ["network", "timeout", "protocol"]) {
    recovery.failed(kind);
    assert.ok(recovery.delay() > 0);
    recovery.released();
    assert.ok(recovery.delay() > 0);
    recovery.resume();
    assert.equal(recovery.delay(), 0);
  }
});

test("intentionally releasing a healthy short connection clears accumulated failures", () => {
  const recovery = createConnectionRecovery(
    () => 0,
    () => 0,
  );
  for (let i = 0; i < 10; i++) recovery.failed("network");
  recovery.ready();
  recovery.released();
  recovery.ready();
  assert.equal(recovery.failed("network").retryMs, 1000);
});

test("releasing a tab cancels open and retry timers before stale callbacks can run", async () => {
  let fired = 0,
    closed = 0,
    heartbeatDisposed = 0;
  const tab = {
    generation: 1,
    status: "connecting",
    recovery: createConnectionRecovery(),
    retryTimer: setTimeout(() => fired++, 10),
    openTimer: setTimeout(() => fired++, 10),
    heartbeat: { dispose: () => heartbeatDisposed++ },
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
  assert.equal(heartbeatDisposed, 1);
  assert.equal(tab.heartbeat, undefined);
  assert.equal(tab.status, "disconnected");
});
