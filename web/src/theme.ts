import type { ITheme } from "@xterm/xterm";

// tmux emits indexed 22/52 for diff fills. Keep the other xterm cube/grays
// unchanged; use muted cool fills for these two saturated dark colors.
// Indexed colors are shared by foregrounds and backgrounds, as in any palette.
const cube = [0, 95, 135, 175, 215, 255];
const hex = (value: number) => value.toString(16).padStart(2, "0");
export const extendedAnsi = Array.from({ length: 240 }, (_, offset) => {
  const index = offset + 16;
  if (index === 22) return "#18302b";
  if (index === 52) return "#321f2c";
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

export type TerminalTheme = { id: string; name: string; colors: ITheme };

// Upstream ANSI 0–15 / selection / cursor colors are retained. Provenance and
// license texts are in public/licenses/terminal-themes-NOTICE.md. HMux only
// adapts indexed diff fills, as documented above; truecolor output is untouched.
export const terminalThemes: TerminalTheme[] = [
  {
    id: "hmux-dark",
    name: "HMux Dark",
    colors: {
      extendedAnsi,
      background: "#17191d",
      foreground: "#dce0e6",
      cursor: "#dce0e6",
      cursorAccent: "#17191d",
      selectionBackground: "#3d4858",
      black: "#1e2025",
      red: "#d89c9f",
      green: "#a2b49a",
      yellow: "#cbb797",
      blue: "#9aadc6",
      magenta: "#b3a1c5",
      cyan: "#a6b7b2",
      white: "#c4c8cf",
      brightBlack: "#8b929f",
      brightRed: "#e6adb0",
      brightGreen: "#b5c5ae",
      brightYellow: "#ded0b5",
      brightBlue: "#bac8dc",
      brightMagenta: "#cbbdd9",
      brightCyan: "#bacccb",
      brightWhite: "#f0f1f3",
    },
  },
  {
    id: "tokyo-night-storm",
    name: "Tokyo Night Storm",
    colors: {
      extendedAnsi,
      background: "#24283b",
      foreground: "#c0caf5",
      cursor: "#c0caf5",
      cursorAccent: "#24283b",
      selectionBackground: "#2e3c64",
      selectionForeground: "#c0caf5",
      black: "#1d202f",
      red: "#f7768e",
      green: "#9ece6a",
      yellow: "#e0af68",
      blue: "#7aa2f7",
      magenta: "#bb9af7",
      cyan: "#7dcfff",
      white: "#a9b1d6",
      brightBlack: "#414868",
      brightRed: "#ff899d",
      brightGreen: "#9fe044",
      brightYellow: "#faba4a",
      brightBlue: "#8db0ff",
      brightMagenta: "#c7a9ff",
      brightCyan: "#a4daff",
      brightWhite: "#c0caf5",
    },
  },
  {
    id: "catppuccin-mocha",
    name: "Catppuccin Mocha",
    colors: {
      extendedAnsi,
      background: "#1e1e2e",
      foreground: "#cdd6f4",
      cursor: "#f5e0dc",
      cursorAccent: "#1e1e2e",
      selectionBackground: "#f5e0dc",
      selectionForeground: "#1e1e2e",
      black: "#45475a",
      red: "#f38ba8",
      green: "#a6e3a1",
      yellow: "#f9e2af",
      blue: "#89b4fa",
      magenta: "#f5c2e7",
      cyan: "#94e2d5",
      white: "#bac2de",
      brightBlack: "#585b70",
      brightRed: "#f38ba8",
      brightGreen: "#a6e3a1",
      brightYellow: "#f9e2af",
      brightBlue: "#89b4fa",
      brightMagenta: "#f5c2e7",
      brightCyan: "#94e2d5",
      brightWhite: "#a6adc8",
    },
  },
  {
    id: "dracula",
    name: "Dracula",
    colors: {
      extendedAnsi,
      background: "#282a36",
      foreground: "#f8f8f2",
      cursor: "#f8f8f2",
      cursorAccent: "#282a36",
      selectionBackground: "#44475a",
      selectionForeground: "#ffffff",
      black: "#21222c",
      red: "#ff5555",
      green: "#50fa7b",
      yellow: "#f1fa8c",
      blue: "#bd93f9",
      magenta: "#ff79c6",
      cyan: "#8be9fd",
      white: "#f8f8f2",
      brightBlack: "#6272a4",
      brightRed: "#ff6e6e",
      brightGreen: "#69ff94",
      brightYellow: "#ffffa5",
      brightBlue: "#d6acff",
      brightMagenta: "#ff92df",
      brightCyan: "#a4ffff",
      brightWhite: "#ffffff",
    },
  },
  {
    id: "nord",
    name: "Nord",
    colors: {
      extendedAnsi,
      background: "#2e3440",
      foreground: "#d8dee9",
      cursor: "#d8dee9",
      cursorAccent: "#2e3440",
      selectionBackground: "#4c566a",
      black: "#3b4252",
      red: "#bf616a",
      green: "#a3be8c",
      yellow: "#ebcb8b",
      blue: "#81a1c1",
      magenta: "#b48ead",
      cyan: "#88c0d0",
      white: "#e5e9f0",
      brightBlack: "#4c566a",
      brightRed: "#bf616a",
      brightGreen: "#a3be8c",
      brightYellow: "#ebcb8b",
      brightBlue: "#81a1c1",
      brightMagenta: "#b48ead",
      brightCyan: "#8fbcbb",
      brightWhite: "#eceff4",
    },
  },
];

export const theme: ITheme = terminalThemes[0].colors;

export function terminalTheme(id: string | null | undefined): TerminalTheme {
  return terminalThemes.find((value) => value.id === id) ?? terminalThemes[0];
}

const preferenceKey = "hmux.terminal.theme";
export function createTerminalAppearance(
  preferences: {
    get(key: string): string | null;
    set(key: string, value: string): void;
  },
  updateSurface: (colors: ITheme) => void,
) {
  let selected = terminalTheme(preferences.get(preferenceKey));
  updateSurface(selected.colors);
  return {
    current: () => selected,
    select(
      id: string,
      terminals: Iterable<{
        options: { theme?: ITheme };
        rows: number;
        refresh(start: number, end: number): void;
      }>,
    ) {
      selected = terminalTheme(id);
      preferences.set(preferenceKey, selected.id);
      updateSurface(selected.colors);
      for (const terminal of terminals) {
        terminal.options.theme = selected.colors;
        terminal.refresh(0, terminal.rows - 1);
      }
    },
  };
}
