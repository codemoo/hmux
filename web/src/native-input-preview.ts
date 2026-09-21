import type { Terminal } from "@xterm/xterm";
import { terminalCellColors } from "./terminal-cell-colors.ts";

// Presentation only: the browser's textarea remains the source of input bytes.
// A contained layer cannot enlarge xterm's scroll area, even for a very long run.
export function createNativeInputPreview(
  term: Terminal,
  screen: HTMLElement,
  caret: boolean,
) {
  const doc = screen.ownerDocument;
  const layer = doc.createElement("div");
  layer.className = "native-input-layer";
  layer.setAttribute("aria-hidden", "true");
  layer.hidden = true;
  const flow = doc.createElement("div");
  flow.className = "native-input-flow";
  const run = doc.createElement("span");
  run.className = "ios-native-composition";
  flow.append(run);
  if (caret) {
    const cursor = doc.createElement("span");
    cursor.className = "native-input-caret";
    flow.append(cursor);
  }
  layer.append(flow);
  screen.append(layer);
  const hide = () => {
    layer.hidden = true;
    if (caret) screen.classList.remove("native-input-pending");
  };
  return {
    get visible() {
      return !layer.hidden;
    },
    setText(value: string) {
      run.textContent = value;
      if (!value) hide();
    },
    position(row: number, col: number) {
      const buffer = term.buffer.active;
      const viewRow = row - buffer.viewportY;
      if (
        !run.textContent ||
        viewRow < 0 ||
        viewRow >= term.rows ||
        !screen.clientWidth ||
        !screen.clientHeight
      ) {
        hide();
        return;
      }
      const colors = terminalCellColors(
        buffer.getLine(row)?.getCell(Math.min(col, term.cols - 1)),
        term.options.theme,
      );
      const cellHeight = screen.clientHeight / term.rows;
      Object.assign(flow.style, {
        color: colors.color,
        // Indent only the first line. Wrapped lines use the full terminal width.
        textIndent: `${(Math.min(col, term.cols) * screen.clientWidth) / term.cols}px`,
        fontFamily: term.options.fontFamily,
        fontSize: `${term.options.fontSize}px`,
        fontWeight: String(term.options.fontWeight ?? "normal"),
        letterSpacing: `${term.options.letterSpacing ?? 0}px`,
        lineHeight: `${cellHeight}px`,
      });
      Object.assign(run.style, colors);
      flow.style.setProperty("--native-caret-height", `${cellHeight}px`);
      layer.style.top = `${viewRow * cellHeight}px`;
      layer.hidden = false;
      // Scroll only the local preview below its anchor. Never paint over earlier
      // terminal rows or move the real buffer/browser viewport to expose a tail.
      const availableHeight = screen.clientHeight - viewRow * cellHeight;
      flow.style.top = `${Math.min(0, availableHeight - flow.offsetHeight)}px`;
      if (caret) {
        screen.style.setProperty(
          "--native-cursor-bg",
          colors.backgroundColor ?? "",
        );
        screen.style.setProperty("--native-cursor-fg", colors.color ?? "");
        screen.classList.add("native-input-pending");
      }
    },
    hide,
    remove() {
      hide();
      layer.remove();
    },
  };
}
