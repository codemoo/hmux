import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { installAndroidNativePaste } from "../src/android-native-paste.ts";

test("Android editable gestures preserve native defaults and do not intercept input/paste", () => {
  const host = new EventTarget();
  const css = new Map();
  const classes = new Set();
  const textarea = {
    value: "한글",
    selectionStart: 2,
    selectionEnd: 2,
    classList: { add: (n) => classes.add(n), remove: (n) => classes.delete(n) },
    style: {
      setProperty: (n, v) => css.set(n, v),
      removeProperty: (n) => css.delete(n),
    },
  };
  let render;
  let disposed = false;
  const term = {
    textarea,
    cols: 40,
    rows: 10,
    element: { querySelector: () => ({ clientWidth: 400, clientHeight: 200 }) },
    buffer: { active: { cursorX: 38, cursorY: 9, baseY: 20, viewportY: 20 } },
    onRender: (cb) => {
      render = cb;
      return {
        dispose() {
          disposed = true;
        },
      };
    },
  };
  const dispose = installAndroidNativePaste(term, host);
  assert.equal(css.get("--paste-x"), "0px");
  assert.equal(css.get("--paste-y"), "156px");
  assert.equal(css.get("--paste-width"), "400px");
  assert.equal(css.get("--paste-height"), "44px");
  assert.equal(css.has("top"), false);
  assert.equal(css.has("left"), false);
  const downstream = [];
  for (const type of [
    "touchstart",
    "touchmove",
    "touchend",
    "mousedown",
    "click",
    "contextmenu",
    "input",
    "paste",
    "keydown",
    "compositionstart",
  ]) {
    host.addEventListener(type, () => downstream.push(type));
    const event = new Event(type, { cancelable: true });
    Object.defineProperty(event, "target", { value: textarea });
    host.dispatchEvent(event);
    assert.equal(event.defaultPrevented, false);
  }
  assert.deepEqual(downstream, [
    "input",
    "paste",
    "keydown",
    "compositionstart",
  ]);
  assert.equal(textarea.value, "한글");
  assert.equal(textarea.selectionStart, 2);
  const outside = new Event("contextmenu");
  host.dispatchEvent(outside);
  assert.equal(downstream.at(-1), "contextmenu");
  term.buffer.active.cursorX = 2;
  render();
  assert.equal(css.get("--paste-x"), "0px");
  term.buffer.active.cursorY = 0;
  render();
  assert.equal(css.get("--paste-y"), "0px");
  dispose();
  assert.equal(disposed, true);
  assert.equal(classes.size, 0);
  assert.equal(css.size, 0);
});
test("Android paste positioning activates only after focus and keyboard visibility", () => {
  const css = readFileSync(
    new URL("../src/style.css", import.meta.url),
    "utf8",
  );
  assert.match(
    css,
    /\.android \.xterm \.xterm-helper-textarea\s*\{[^}]*top: 0 !important;[^}]*left: 0 !important;/,
  );
  assert.match(
    css,
    /\.android\.keyboard-visible \.xterm \.android-native-paste-target:focus\s*\{[^}]*transform: translate/,
  );
});
