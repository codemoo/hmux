import test from "node:test";
import assert from "node:assert/strict";
import { extendedAnsi } from "../src/theme.ts";

test("diff palette only changes the two observed indexed fills", () => {
  assert.equal(extendedAnsi.length, 240);
  const cube = [0, 95, 135, 175, 215, 255];
  const expected = [];
  for (const r of cube)
    for (const g of cube)
      for (const b of cube)
        expected.push(
          "#" + [r, g, b].map((v) => v.toString(16).padStart(2, "0")).join(""),
        );
  for (let i = 0; i < 24; i++)
    expected.push("#" + (8 + 10 * i).toString(16).padStart(2, "0").repeat(3));
  assert.deepEqual(
    extendedAnsi.flatMap((color, i) =>
      color.toLowerCase() === expected[i] ? [] : [i + 16],
    ),
    [22, 52],
  );
  for (const index of [22, 52]) {
    const channels = extendedAnsi[index - 16]
      .slice(1)
      .match(/../g)
      .map((value) => parseInt(value, 16));
    assert.ok(Math.max(...channels) < 60, "diff fills stay dark");
  }
});
