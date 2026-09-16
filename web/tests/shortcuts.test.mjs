import test from "node:test";
import assert from "node:assert/strict";
import { workspaceShortcut } from "../src/shortcuts.ts";
const base = {
  altKey: false,
  shiftKey: false,
  ctrlKey: false,
  metaKey: false,
  isComposing: false,
  code: "ArrowLeft",
};
test("HMux chords avoid browser tabs, history and IME", () => {
  assert.equal(
    workspaceShortcut({ ...base, altKey: true, shiftKey: true }),
    "previous",
  );
  assert.equal(
    workspaceShortcut({
      ...base,
      altKey: true,
      shiftKey: true,
      code: "ArrowRight",
    }),
    "next",
  );
  assert.equal(
    workspaceShortcut({ ...base, altKey: true, code: "KeyL" }),
    "sidebar",
  );
  assert.equal(
    workspaceShortcut({ ...base, altKey: true, code: "KeyW" }),
    "close",
  );
  assert.equal(
    workspaceShortcut({ ...base, altKey: true, code: "KeyQ" }),
    "logout",
  );
  for (const keys of [
    { altKey: true },
    { altKey: true, shiftKey: true, code: "KeyQ" },
    { altKey: true, metaKey: true, code: "KeyQ" },
    { altKey: true, shiftKey: true, code: "KeyW" },
    { altKey: true, isComposing: true, code: "KeyW" },
    { altKey: true, shiftKey: true, code: "KeyL" },
    { metaKey: true, code: "Digit1" },
    { ctrlKey: true, code: "Tab" },
    { altKey: true, shiftKey: true, isComposing: true },
    { altKey: true, shiftKey: true, metaKey: true },
  ])
    assert.equal(workspaceShortcut({ ...base, ...keys }), undefined);
});

test("Alt digits select tabs by physical number key and reject other modifiers", () => {
  for (let number = 1; number <= 9; number++) {
    const keys = { ...base, altKey: true, code: `Digit${number}` };
    assert.deepEqual(workspaceShortcut(keys), { tabIndex: number - 1 });
    for (const modifier of ["shiftKey", "ctrlKey", "metaKey", "isComposing"])
      assert.equal(workspaceShortcut({ ...keys, [modifier]: true }), undefined);
  }
  assert.equal(
    workspaceShortcut({ ...base, altKey: true, code: "Digit0" }),
    undefined,
  );
  assert.equal(
    workspaceShortcut({ ...base, altKey: true, shiftKey: true, code: "KeyB" }),
    undefined,
  );
});

test("sidebar alias uses physical Backquote for English and Korean won layouts", () => {
  for (const key of ["`", "₩", "~", "Process"]) {
    assert.equal(
      workspaceShortcut({ ...base, altKey: true, code: "Backquote", key }),
      "sidebar",
    );
  }
  for (const modifier of ["shiftKey", "ctrlKey", "metaKey", "isComposing"])
    assert.equal(
      workspaceShortcut({
        ...base,
        altKey: true,
        code: "Backquote",
        [modifier]: true,
      }),
      undefined,
    );
});
