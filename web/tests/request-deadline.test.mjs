import test from "node:test";
import assert from "node:assert/strict";
import { withRequestDeadline } from "../src/request-deadline.ts";

function stalledBody(signal) {
  return new Promise((resolve, reject) => {
    if (signal.aborted) reject(signal.reason);
    else
      signal.addEventListener("abort", () => reject(signal.reason), {
        once: true,
      });
  });
}

test("deadline aborts a stalled response body and permits a subsequent request", async () => {
  await assert.rejects(
    withRequestDeadline(
      async (signal) => {
        await Promise.resolve(); // Headers have already arrived.
        return stalledBody(signal);
      },
      undefined,
      10,
    ),
    { name: "TimeoutError" },
  );
  assert.equal(await withRequestDeadline(async () => "retried"), "retried");
});

test("account disposal cancels its request without cancelling the next account", async () => {
  const oldAccount = new AbortController();
  const pending = withRequestDeadline(stalledBody, oldAccount.signal);
  oldAccount.abort();
  await assert.rejects(pending, { name: "AbortError" });
  assert.equal(
    await withRequestDeadline(async (signal) => signal.aborted),
    false,
  );
});

test("already cancelled caller and successful completion preserve cancellation semantics", async () => {
  const caller = new AbortController();
  caller.abort();
  await assert.rejects(withRequestDeadline(stalledBody, caller.signal), {
    name: "AbortError",
  });
  let completedSignal;
  await withRequestDeadline(
    async (signal) => {
      completedSignal = signal;
    },
    undefined,
    5,
  );
  await new Promise((resolve) => setTimeout(resolve, 15));
  assert.equal(completedSignal.aborted, false);
});
