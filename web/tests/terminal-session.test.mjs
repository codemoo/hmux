import test from "node:test";
import assert from "node:assert/strict";
import {
  releaseTerminalView,
  syncTerminalSize,
} from "../src/terminal-session.ts";
test("release rejects stale callbacks before closing and preserves terminal", () => {
  let closed = 0;
  const term = {
    dispose() {
      assert.fail("view release disposed terminal");
    },
  };
  const tab = { generation: 7, status: "connected", term };
  tab.ws = {
    close() {
      assert.equal(tab.generation, 8);
      closed++;
    },
  };
  releaseTerminalView(tab);
  assert.equal(closed, 1);
  assert.equal(tab.ws, undefined);
  assert.equal(tab.status, "disconnected");
  assert.equal(tab.term, term);
  releaseTerminalView(tab);
  assert.equal(closed, 1);
});

test("viewport changes keep every PTY size inside the gateway contract", async () => {
  const { terminalSize } = await import("../src/terminal-session.ts");
  assert.deepEqual(terminalSize(80, 1), { cols: 80, rows: 2 });
  assert.deepEqual(terminalSize(600, 300), { cols: 500, rows: 250 });
  assert.deepEqual(terminalSize(120, 40), { cols: 120, rows: 40 });
  assert.deepEqual(terminalSize(NaN, Infinity), { cols: 2, rows: 2 });
});

test("ready resynchronizes a viewport resize missed during terminal startup", () => {
  const frames = [];
  const tab = {
    status: "connecting",
    term: { cols: 60, rows: 20 },
    ws: { readyState: 1, send: (frame) => frames.push(JSON.parse(frame)) },
  };
  // Home opened with 20 rows; the keyboard then closed before ready arrived.
  tab.term.rows = 55;
  assert.equal(syncTerminalSize(tab), false);
  assert.equal(frames.length, 0);
  tab.status = "connected";
  assert.equal(syncTerminalSize(tab), true);
  assert.deepEqual(frames, [{ type: "resize", cols: 60, rows: 55 }]);
  // No new local xterm resize event is needed for an explicit redraw either.
  assert.equal(syncTerminalSize(tab), true);
  assert.deepEqual(frames[1], frames[0]);
  tab.ws.readyState = 3;
  assert.equal(syncTerminalSize(tab), false);
  assert.equal(frames.length, 2);
});
