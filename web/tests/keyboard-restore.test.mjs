import test from "node:test";
import assert from "node:assert/strict";
import { createKeyboardRestore } from "../src/keyboard-restore.ts";

test("dismissal redraws once after repeated settled geometry observations", () => {
  const restore = createKeyboardRestore();
  const tab = { generation: 1 };
  restore.observe(false, tab);
  assert.equal(restore.take(tab), undefined);
  restore.observe(true, tab);
  restore.observe(true, tab);
  assert.equal(
    restore.take(tab),
    undefined,
    "do not redraw a reduced viewport",
  );
  restore.observe(false, tab);
  restore.observe(false, tab);
  assert.equal(restore.take(tab), tab);
  restore.observe(false, tab);
  assert.equal(restore.take(tab), undefined, "resize observers must not loop");
});

test("stale dismissal cannot redraw another tab, connection or hidden view", () => {
  for (const transition of ["tab", "reconnect", "hidden"]) {
    const restore = createKeyboardRestore();
    const tab = { generation: 1 };
    restore.observe(true, tab);
    restore.observe(false, tab);
    const other = transition === "tab" ? { generation: 1 } : tab;
    if (transition === "reconnect") tab.generation++;
    restore.observe(false, transition === "hidden" ? undefined : other);
    restore.observe(false, tab);
    assert.equal(restore.take(tab), undefined, transition);
  }
});

test("reopening cancels the pending redraw and supports a later dismissal", () => {
  const restore = createKeyboardRestore();
  const tab = { generation: 1 };
  restore.observe(true, tab);
  restore.observe(false, tab);
  restore.observe(true, tab);
  assert.equal(restore.take(tab), undefined);
  restore.observe(false, tab);
  assert.equal(restore.take(tab), tab);
});

test("keyboard interaction in a dialog has no terminal redraw owner", () => {
  const restore = createKeyboardRestore();
  const tab = { generation: 1 };
  restore.observe(true, undefined);
  restore.observe(false, tab);
  assert.equal(restore.take(tab), undefined);
});

test("tab or generation changes while the keyboard is visible invalidate its owner", () => {
  for (const transition of ["tab", "reconnect", "dialog"]) {
    const restore = createKeyboardRestore();
    const tab = { generation: 1 };
    restore.observe(true, tab);
    if (transition === "reconnect") tab.generation++;
    restore.observe(
      true,
      transition === "tab"
        ? { generation: 1 }
        : transition === "dialog"
          ? undefined
          : tab,
    );
    restore.observe(true, tab);
    restore.observe(false, tab);
    assert.equal(restore.take(tab), undefined, transition);
    restore.observe(true, tab);
    restore.observe(false, tab);
    assert.equal(restore.take(tab), tab, "a later opening gets a fresh owner");
  }
});
