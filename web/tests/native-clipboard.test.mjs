import test from "node:test";
import assert from "node:assert/strict";
import {
  hasNativeSelection,
  installNativeClipboard,
} from "../src/native-clipboard.ts";
function setup() {
  const host = new EventTarget();
  const node = {};
  let selection = null;
  let focused = 0;
  host.ownerDocument = { getSelection: () => selection };
  host.contains = (n) => n === node;
  const term = {
    element: { classList: { add() {} } },
    focus() {
      focused++;
    },
  };
  installNativeClipboard(term, host);
  return { host, node, set: (s) => (selection = s), focus: () => focused };
}
test("native range copy retains browser default and bypasses xterm selection", () => {
  const f = setup();
  f.set({ isCollapsed: false, anchorNode: f.node, focusNode: f.node });
  assert.equal(hasNativeSelection(f.host), true);
  let xterm = 0;
  f.host.addEventListener("copy", () => xterm++);
  const copy = new Event("copy", { cancelable: true });
  f.host.dispatchEvent(copy);
  assert.equal(copy.defaultPrevented, false);
  assert.equal(xterm, 0);
  const menu = new Event("contextmenu", { cancelable: true });
  f.host.dispatchEvent(menu);
  assert.equal(menu.defaultPrevented, false);
});
test("unselected contextmenu uses xterm default input path", () => {
  const f = setup();
  let menus = 0;
  f.host.addEventListener("contextmenu", () => menus++);
  f.host.dispatchEvent(new Event("contextmenu"));
  assert.equal(menus, 1);
  f.host.dispatchEvent(new Event("touchstart"));
  let mouse = 0;
  f.host.addEventListener("mousedown", () => mouse++);
  f.host.dispatchEvent(new Event("mousedown"));
  assert.equal(mouse, 0);
  f.host.dispatchEvent(new Event("click"));
  assert.equal(f.focus(), 1);
  f.set({ isCollapsed: false, anchorNode: f.node, focusNode: f.node });
  f.host.dispatchEvent(new Event("click"));
  assert.equal(f.focus(), 1);
});
test("selection outside terminal does not hijack copy or focus", () => {
  const f = setup();
  f.set({ isCollapsed: false, anchorNode: {}, focusNode: {} });
  assert.equal(hasNativeSelection(f.host), false);
});

test("touch context menu preserves native ownership before a range exists with keyboard focus", () => {
  const f = setup();
  f.host.dispatchEvent(new Event("touchstart"));
  let desktopMenu = 0;
  f.host.addEventListener("contextmenu", () => desktopMenu++);
  const menu = new Event("contextmenu", { cancelable: true });
  f.host.dispatchEvent(menu);
  assert.equal(menu.defaultPrevented, false);
  assert.equal(desktopMenu, 0);
});
test("long press does not refocus the textarea before Safari publishes its selection", () => {
  const originalNow = Date.now;
  let now = 1000;
  Date.now = () => now;
  try {
    const f = setup();
    f.host.dispatchEvent(new Event("touchstart"));
    now += 1500;
    let mouse = 0;
    f.host.addEventListener("mousedown", () => mouse++);
    f.host.dispatchEvent(new Event("mousedown"));
    assert.equal(mouse, 0);
    f.host.dispatchEvent(new Event("touchend"));
    const click = new Event("click", { cancelable: true });
    f.host.dispatchEvent(click);
    assert.equal(click.defaultPrevented, false);
    assert.equal(f.focus(), 0);
    f.set({ isCollapsed: false, anchorNode: f.node, focusNode: f.node });
    let copies = 0;
    f.host.addEventListener("copy", () => copies++);
    const copy = new Event("copy", { cancelable: true });
    f.host.dispatchEvent(copy);
    assert.equal(copy.defaultPrevented, false);
    assert.equal(copies, 0);
    // A new intentional short tap resumes input.
    f.set(null);
    now += 100;
    f.host.dispatchEvent(new Event("touchstart"));
    now += 50;
    f.host.dispatchEvent(new Event("touchend"));
    f.host.dispatchEvent(new Event("click"));
    assert.equal(f.focus(), 1);
  } finally {
    Date.now = originalNow;
  }
});
