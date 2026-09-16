import test from "node:test";
import assert from "node:assert/strict";
import { safeTerminalURL, isTerminalCopy } from "../src/desktop-terminal.ts";

test("terminal links only open explicit HTTP(S), without embedded credentials", () => {
  assert.equal(
    safeTerminalURL("https://example.com/a?q=1#part"),
    "https://example.com/a?q=1#part",
  );
  for (const value of [
    "javascript:alert(1)",
    "data:text/html,test",
    "file:///etc/passwd",
    "//example.com",
    "https://user:password@example.com",
    "invalid",
  ])
    assert.equal(safeTerminalURL(value), undefined);
});
test("copy shortcuts bypass PTY only while terminal text is selected", () => {
  const event = {
    code: "KeyC",
    metaKey: true,
    ctrlKey: false,
    altKey: false,
    isComposing: false,
  };
  assert.equal(isTerminalCopy({ hasSelection: () => true }, event, true), true);
  assert.equal(
    isTerminalCopy({ hasSelection: () => false }, event, true),
    false,
  );
  assert.equal(
    isTerminalCopy(
      { hasSelection: () => true },
      { ...event, metaKey: false, ctrlKey: true },
      false,
    ),
    true,
  );
  assert.equal(
    isTerminalCopy(
      { hasSelection: () => true },
      { ...event, isComposing: true },
    ),
    false,
  );
});

test("macOS Control+C still interrupts a command with text selected", () => {
  assert.equal(
    isTerminalCopy(
      { hasSelection: () => true },
      {
        code: "KeyC",
        ctrlKey: true,
        metaKey: false,
        altKey: false,
        isComposing: false,
      },
      true,
    ),
    false,
  );
});
