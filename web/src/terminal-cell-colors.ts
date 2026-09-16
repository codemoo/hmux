import type { IBufferCell, ITheme } from "@xterm/xterm";
import { theme as defaultTheme } from "./theme.ts";

const ansiKeys = [
  "black",
  "red",
  "green",
  "yellow",
  "blue",
  "magenta",
  "cyan",
  "white",
  "brightBlack",
  "brightRed",
  "brightGreen",
  "brightYellow",
  "brightBlue",
  "brightMagenta",
  "brightCyan",
  "brightWhite",
] as const;

// Buffer colors describe the painted input row, unlike the page background.
// Use public xterm cell attributes so truecolor, indexed fills and inverse video
// all work without reading renderer internals or changing the input transaction.
export function terminalCellColors(
  cell: IBufferCell | undefined,
  theme?: ITheme,
) {
  const resolve = (foreground: boolean) => {
    const fallback = foreground ? "foreground" : "background";
    if (cell) {
      const value = foreground ? cell.getFgColor() : cell.getBgColor();
      const rgb = foreground ? cell.isFgRGB() : cell.isBgRGB();
      const palette = foreground ? cell.isFgPalette() : cell.isBgPalette();
      if (rgb) return `#${value.toString(16).padStart(6, "0")}`;
      if (palette && value >= 0 && value < 256) {
        if (value < 16) {
          const key = ansiKeys[value];
          return theme?.[key] ?? defaultTheme[key];
        }
        return (
          theme?.extendedAnsi?.[value - 16] ??
          defaultTheme.extendedAnsi![value - 16]
        );
      }
    }
    return theme?.[fallback] ?? defaultTheme[fallback];
  };
  const inverse = !!cell?.isInverse();
  return { backgroundColor: resolve(inverse), color: resolve(!inverse) };
}
