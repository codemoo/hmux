import { msg } from "./i18n.ts";
import { createTextFactory } from "./dom.ts";
import { terminalThemes, type TerminalTheme } from "./theme.ts";

export function installThemePicker(
  parent: HTMLElement,
  current: () => TerminalTheme,
  select: (id: string) => void,
) {
  const text = createTextFactory(parent.ownerDocument);
  const fieldset = text("fieldset", "", "theme-picker");
  fieldset.append(text("legend", msg("Color theme", "색상 테마")));
  const grid = text("div", "", "theme-grid");
  for (const theme of terminalThemes) {
    const label = text("label", "", "theme-option");
    const input = text("input");
    input.type = "radio";
    input.name = "terminal-theme";
    input.value = theme.id;
    input.checked = theme.id === current().id;
    input.onchange = () => {
      if (input.checked) select(theme.id);
    };
    const swatches = text("span", "", "theme-swatches");
    swatches.setAttribute("aria-hidden", "true");
    swatches.style.backgroundColor = theme.colors.background!;
    for (const color of [
      "red",
      "green",
      "yellow",
      "blue",
      "magenta",
      "cyan",
    ] as const) {
      const swatch = text("i");
      swatch.style.backgroundColor = theme.colors[color]!;
      swatches.append(swatch);
    }
    const title = text("span", theme.name, "theme-name");
    label.append(input, swatches, title);
    grid.append(label);
  }
  fieldset.append(grid);
  parent.append(fieldset);
}
