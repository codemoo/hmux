import test from "node:test";
import assert from "node:assert/strict";
import { scrollSteps } from "../src/terminal-scroll.ts";
test("touch scroll retains sub-cell movement and limits per-event output", () => {
  assert.deepEqual(scrollSteps(5, 12), { lines: 0, remaining: 5 });
  assert.deepEqual(scrollSteps(29, 12), { lines: 2, remaining: 5 });
  assert.deepEqual(scrollSteps(-29, 12), { lines: -2, remaining: -5 });
  assert.equal(scrollSteps(500, 12).lines, 8);
  assert.deepEqual(scrollSteps(17, 0), { lines: 2, remaining: 1 });
});

import { installTerminalScroll } from "../src/terminal-scroll.ts";
test("tmux touch wheel bypasses only the text-input lock and restores it", () => {
  class Wheel extends Event {
    static DOM_DELTA_LINE = 1;
    constructor(type, init) {
      super(type, init);
      Object.assign(this, {
        deltaY: init.deltaY,
        deltaMode: init.deltaMode,
        clientX: init.clientX,
        clientY: init.clientY,
      });
    }
  }
  globalThis.WheelEvent = Wheel;
  const host = new EventTarget();
  host.getBoundingClientRect = () => ({ height: 240 });
  const element = new EventTarget();
  const sent = [];
  const term = {
    rows: 20,
    modes: { mouseTrackingMode: "drag" },
    options: { disableStdin: true },
    element,
    scrollLines: () => {
      throw Error("must route to tmux");
    },
  };
  element.addEventListener("wheel", (e) => {
    assert.equal(term.options.disableStdin, false);
    sent.push(e.deltaY);
  });
  installTerminalScroll(term, host, () => true);
  function touch(type, y) {
    const e = new Event(type, { cancelable: true });
    Object.defineProperty(e, "touches", {
      value: [{ clientX: 40, clientY: y }],
    });
    host.dispatchEvent(e);
    return e;
  }
  touch("touchstart", 50);
  const moved = touch("touchmove", 80);
  assert.equal(moved.defaultPrevented, true);
  assert.deepEqual(sent, [-1, -1]);
  assert.equal(term.options.disableStdin, true);
  touch("touchend", 80);
  const click = new Event("click", { cancelable: true });
  host.dispatchEvent(click);
  assert.equal(click.defaultPrevented, true);
});
test("ordinary terminal scrolls its own history without sending arrow keys", () => {
  const host = new EventTarget();
  host.getBoundingClientRect = () => ({ height: 240 });
  const lines = [];
  installTerminalScroll(
    {
      rows: 20,
      modes: { mouseTrackingMode: "none" },
      scrollLines: (n) => lines.push(n),
    },
    host,
    () => true,
  );
  for (const [type, y] of [
    ["touchstart", 80],
    ["touchmove", 50],
  ]) {
    const e = new Event(type, { cancelable: true });
    Object.defineProperty(e, "touches", {
      value: [{ clientX: 40, clientY: y }],
    });
    host.dispatchEvent(e);
  }
  assert.deepEqual(lines, [2]);
});

test("native selection-handle drag is not converted to tmux scrolling", () => {
  const host = new EventTarget();
  host.getBoundingClientRect = () => ({ height: 240 });
  installTerminalScroll(
    {
      rows: 20,
      modes: { mouseTrackingMode: "none" },
      scrollLines() {
        assert.fail("selection moved tmux");
      },
    },
    host,
    () => true,
    () => true,
  );
  for (const [type, y] of [
    ["touchstart", 80],
    ["touchmove", 20],
  ]) {
    const event = new Event(type, { cancelable: true });
    Object.defineProperty(event, "touches", {
      value: [{ clientX: 20, clientY: y }],
    });
    host.dispatchEvent(event);
    assert.equal(event.defaultPrevented, false);
  }
});
