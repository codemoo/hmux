import test from "node:test";
import assert from "node:assert/strict";
import { terminalCellColors } from "../src/terminal-cell-colors.ts";
import { theme } from "../src/theme.ts";

const cell = (bgMode, bg, fgMode = "default", fg = 0, inverse = false) => ({
  getBgColor: () => bg,
  getFgColor: () => fg,
  isBgRGB: () => bgMode === "rgb",
  isFgRGB: () => fgMode === "rgb",
  isBgPalette: () => bgMode === "palette",
  isFgPalette: () => fgMode === "palette",
  isInverse: () => Number(inverse),
});

test("pending input follows default, RGB and indexed terminal backgrounds", () => {
  assert.equal(terminalCellColors(undefined).backgroundColor, theme.background);
  assert.equal(terminalCellColors(cell("rgb", 0)).backgroundColor, "#000000");
  assert.equal(
    terminalCellColors(cell("rgb", 0x303030)).backgroundColor,
    "#303030",
  );
  assert.equal(
    terminalCellColors(cell("palette", 236)).backgroundColor,
    "#303030",
  );
  assert.equal(
    terminalCellColors(cell("palette", 22)).backgroundColor,
    "#18302b",
  );
  assert.equal(
    terminalCellColors(cell("palette", 8), { brightBlack: "#444444" })
      .backgroundColor,
    "#444444",
  );
  assert.equal(
    terminalCellColors(cell("palette", 16), { extendedAnsi: ["#112233"] })
      .backgroundColor,
    "#112233",
  );
  assert.equal(
    terminalCellColors(cell("default", 0), { background: "#222222" })
      .backgroundColor,
    "#222222",
  );
});

test("inverse video uses the cell foreground as its painted background", () => {
  assert.deepEqual(
    terminalCellColors(cell("rgb", 0xeeeeee, "rgb", 0x202020, true)),
    {
      backgroundColor: "#202020",
      color: "#eeeeee",
    },
  );
  assert.deepEqual(terminalCellColors(cell("default", 0, "default", 0, true)), {
    backgroundColor: theme.foreground,
    color: theme.background,
  });
});
