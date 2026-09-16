import test from "node:test";
import assert from "node:assert/strict";
import { createTerminalFonts } from "../src/fonts.ts";

test("Android font registration is atomic, retryable and shared between loads", async () => {
  const oldDoc = globalThis.document,
    oldFace = globalThis.FontFace;
  const added = [];
  let fail = true;
  let constructed = 0;
  globalThis.document = {
    fonts: { add: (face) => added.push(face), load: async () => [{}] },
  };
  globalThis.FontFace = class {
    constructor(name, source, options) {
      this.weight = options.weight;
      constructed++;
    }
    async load() {
      if (fail && this.weight === "700") throw Error("network");
      return this;
    }
  };
  try {
    const fonts = createTerminalFonts(true);
    await assert.rejects(fonts.load());
    assert.equal(added.length, 0);
    assert.ok(!fonts.family().includes("HMux Android Mono"));
    fail = false;
    await Promise.all([fonts.load(), fonts.load()]);
    assert.equal(added.length, 2);
    assert.equal(constructed, 4);
    assert.ok(fonts.family().includes("HMux Android Mono"));
    await fonts.load();
    assert.equal(added.length, 2);
  } finally {
    globalThis.document = oldDoc;
    globalThis.FontFace = oldFace;
  }
});
