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

test("theme selection survives reload, updates every terminal and tolerates blocked storage", async () => {
  const { createTerminalAppearance, terminalThemes, terminalTheme } =
    await import("../src/theme.ts");
  const { createPreferences } = await import("../src/preferences.ts");
  const prefs = createPreferences(() => {
    throw new Error("storage blocked");
  });
  const applied = [];
  const appearance = createTerminalAppearance(prefs, (colors) =>
    applied.push(colors.background),
  );
  const terminals = Array.from({ length: 3 }, () => ({
    options: {},
    rows: 24,
    refreshes: [],
    refresh(start, end) {
      this.refreshes.push([start, end]);
    },
  }));
  for (const choice of terminalThemes) {
    appearance.select(choice.id, terminals);
    assert.equal(appearance.current(), choice);
    for (const terminal of terminals) {
      assert.equal(terminal.options.theme.background, choice.colors.background);
      assert.deepEqual(terminal.refreshes.at(-1), [0, 23]);
    }
    assert.equal(
      createTerminalAppearance(prefs, () => {}).current().id,
      choice.id,
    );
    assert.equal(applied.at(-1), choice.colors.background);
  }
  assert.equal(terminalTheme("invalid").id, "hmux-dark");
  assert.equal(terminalTheme(null).id, "hmux-dark");
});

test("every theme supplies both ANSI weights, readable selection and theme-aware pending colors", async () => {
  const { terminalThemes } = await import("../src/theme.ts");
  const { terminalCellColors } = await import("../src/terminal-cell-colors.ts");
  assert.equal(
    new Set(terminalThemes.map((t) => t.id)).size,
    terminalThemes.length,
  );
  for (const { colors } of terminalThemes) {
    for (const key of [
      "black",
      "red",
      "green",
      "yellow",
      "blue",
      "magenta",
      "cyan",
      "white",
    ]) {
      assert.match(colors[key], /^#[0-9a-f]{6}$/i);
      assert.match(
        colors["bright" + key[0].toUpperCase() + key.slice(1)],
        /^#[0-9a-f]{6}$/i,
      );
    }
    assert.notEqual(
      colors.selectionBackground,
      colors.selectionForeground ?? colors.foreground,
    );
    assert.deepEqual(terminalCellColors(undefined, colors), {
      backgroundColor: colors.background,
      color: colors.foreground,
    });
  }
});
