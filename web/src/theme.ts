import type { ITheme } from "@xterm/xterm";

// tmux emits indexed 22/52 for diff fills. Keep the other xterm cube/grays
// unchanged; use muted Flexoki fills for these two saturated dark colors.
// Indexed colors are shared by foregrounds and backgrounds, as in any palette.
const cube = [0, 95, 135, 175, 215, 255];
const hex = (value: number) => value.toString(16).padStart(2, "0");
export const extendedAnsi = Array.from({ length: 240 }, (_, offset) => {
  const index = offset + 16;
  if (index === 22) return "#1F271B";
  if (index === 52) return "#321D1D";
  if (index >= 232) return "#" + hex(8 + (index - 232) * 10).repeat(3);
  return (
    "#" +
    [
      cube[Math.floor(offset / 36)],
      cube[Math.floor(offset / 6) % 6],
      cube[offset % 6],
    ]
      .map(hex)
      .join("")
  );
});

export const theme: ITheme = {
  extendedAnsi,
  background: "#100F0F",
  foreground: "#CECDC3",
  cursor: "#CECDC3",
  selectionBackground: "#403E3C",
  black: "#100F0F",
  red: "#AF3029",
  green: "#66800B",
  yellow: "#AD8301",
  blue: "#205EA6",
  magenta: "#A02F6F",
  cyan: "#24837B",
  white: "#878580",
  brightBlack: "#6F6E69",
  brightRed: "#D14D41",
  brightGreen: "#879A39",
  brightYellow: "#D0A215",
  brightBlue: "#4385BE",
  brightMagenta: "#CE5D97",
  brightCyan: "#3AA99F",
  brightWhite: "#CECDC3",
};
