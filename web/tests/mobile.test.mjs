import test from "node:test";
import assert from "node:assert/strict";
import {
  preferredFontSize,
  viewportGeometry,
  createKeyboardTracker,
  remainingBottomInset,
  terminalFontFamily,
} from "../src/mobile.ts";
test("compact mobile default and bounded user preference", () => {
  assert.equal(preferredFontSize(true, null), 10);
  assert.equal(preferredFontSize(false, null), 14);
  assert.equal(preferredFontSize(true, "8"), 8);
  assert.equal(preferredFontSize(true, "broken"), 10);
  assert.ok(terminalFontFamily.includes("Monatendard"));
  assert.deepEqual(viewportGeometry(351.6, 27.2), { height: 352, top: 27 });
});
test("bottom safe area only reserves space not already excluded by visual viewport", () => {
  assert.equal(remainingBottomInset(34, 844, 844, 0), 34);
  assert.equal(remainingBottomInset(34, 844, 810, 0), 0);
  assert.equal(remainingBottomInset(34, 844, 824, 0), 14);
  assert.equal(remainingBottomInset(34, 844, 800, 44), 34);
  assert.equal(remainingBottomInset(34, 844, 500, 0), 0);
  assert.equal(remainingBottomInset(0, 844, 844, 0), 0);
});

test("Android content-resize keyboard detection retains full viewport height as reference", () => {
  const detect = createKeyboardTracker();
  assert.equal(detect("portrait", 800, false), false);
  assert.equal(detect("portrait", 800, true), false);
  assert.equal(detect("portrait", 470, true), true);
  assert.equal(detect("portrait", 470, false), true); // floating tabs take focus
  assert.equal(detect("portrait", 800, true), false); // back dismisses keyboard, focus stays
  assert.equal(detect("portrait", 730, true), false); // browser toolbar is not a keyboard
  assert.equal(detect("landscape", 390, false), false);
  assert.equal(detect("landscape", 210, true), true);
  assert.equal(detect("portrait", 800, false), false);
});
