import test from "node:test";
import assert from "node:assert/strict";
import { LinkTap } from "../src/mobile-terminal-links.ts";

const point = (x = 20, y = 20, id = 1) => ({
  identifier: id,
  clientX: x,
  clientY: y,
});
test("only a short stationary single-finger gesture activates links", () => {
  const tap = new LinkTap();
  tap.begin([point()], 100);
  assert.equal(tap.end([point(23, 24)], 449), true);
  tap.begin([point()], 100);
  assert.equal(tap.end([point()], 450), false);
  assert.equal(tap.end([point()], 200), false);
});
test("moving out and back, multitouch, and changed finger never activate", () => {
  const tap = new LinkTap();
  for (const points of [
    [point(26)],
    [point(), point(22, 20, 2)],
    [point(20, 20, 2)],
  ]) {
    tap.begin([point()], 100);
    tap.move(points);
    assert.equal(tap.end([point()], 200), false);
  }
  tap.begin([point(), point(22, 20, 2)], 100);
  assert.equal(tap.end([point()], 200), false);
  tap.begin([point()], 100);
  assert.equal(tap.end([point(26)], 200), false);
});
test("contextmenu, output changes and lifecycle cancellation invalidate pending taps", () => {
  const tap = new LinkTap();
  tap.begin([point()], 100);
  tap.cancel();
  assert.equal(tap.end([point()], 200), false);
  tap.begin([point()], 300);
  assert.equal(tap.end([point()], 400), true);
});
