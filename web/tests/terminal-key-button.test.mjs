import test from "node:test";
import assert from "node:assert/strict";
import { bindTerminalKeyButton } from "../src/terminal-key-button.ts";

function fixture() {
  const button = new EventTarget();
  button.disabled = false;
  let activations = 0;
  bindTerminalKeyButton(button, () => activations++);
  const point = (x = 20, y = 20, identifier = 1) => ({
    clientX: x,
    clientY: y,
    identifier,
  });
  const event = (type, touches = [], changedTouches = []) => {
    const event = new Event(type, { cancelable: true });
    Object.assign(event, { touches, changedTouches });
    button.dispatchEvent(event);
    return event;
  };
  return { button, point, event, count: () => activations };
}

test("accessory touch activates once and prevents focus-changing compatibility clicks", () => {
  const f = fixture();
  const pointer = new Event("pointerdown", { cancelable: true });
  f.button.onpointerdown(pointer);
  assert.equal(pointer.defaultPrevented, true);
  const start = f.event("touchstart", [f.point()]);
  assert.equal(
    start.defaultPrevented,
    false,
    "horizontal scrolling stays native",
  );
  const end = f.event("touchend", [], [f.point()]);
  assert.equal(end.defaultPrevented, true);
  assert.equal(f.count(), 1);
  f.button.onclick({ detail: 1 });
  assert.equal(f.count(), 1, "compatibility click cannot send Ctrl+C twice");
  f.button.onclick({ detail: 0 });
  assert.equal(f.count(), 2, "keyboard/assistive activation remains available");
});

test("swipes, canceled touches, multitouch and disabled keys never send commands", () => {
  for (const gesture of ["swipe", "end-moved", "cancel", "multi", "disabled"]) {
    const f = fixture();
    f.event("touchstart", [f.point()]);
    if (gesture === "swipe") f.event("touchmove", [f.point(50)]);
    if (gesture === "cancel") f.event("touchcancel");
    if (gesture === "multi")
      f.event("touchstart", [f.point(), f.point(30, 20, 2)]);
    if (gesture === "disabled") f.button.disabled = true;
    f.event("touchend", [], [f.point(gesture === "end-moved" ? 50 : 20)]);
    f.button.onclick({ detail: 1 });
    assert.equal(f.count(), 0, gesture);
  }
});

test("ordinary mouse clicks and repeated distinct taps remain usable", () => {
  const f = fixture();
  f.button.onclick({ detail: 1 });
  assert.equal(f.count(), 1);
  for (let i = 0; i < 3; i++) {
    f.event("touchstart", [f.point()]);
    f.event("touchend", [], [f.point()]);
  }
  assert.equal(f.count(), 4);
});
