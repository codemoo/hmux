import type { Terminal } from "@xterm/xterm";
import { preserveNativeEditableGestures } from "./native-clipboard.ts";
import { terminalCellColors } from "./terminal-cell-colors.ts";

const hangul = /^[\u1100-\u11ff\u3130-\u318f\ua960-\ua97f\uac00-\ud7ff]+$/u;

// iPhone trace: keyCode 0 -> keypress -> deleteContentBackward + insertText,
// with no composition events. Keep that native edit transaction local. Never
// compose jamo ourselves, retain a syllable tail, or repair already-sent bytes.
export function installIOSNativeInput(
  term: Terminal,
  host: HTMLElement,
  enabled: () => boolean,
) {
  return installNativeInput(term, host, enabled, false);
}

// Physical macOS Safari 18.6 trace: input precedes keydown229; selected
// insertReplacementText edits compose Hangul without composition events.
export function installMacSafariNativeInput(
  term: Terminal,
  host: HTMLElement,
  enabled: () => boolean,
) {
  return installNativeInput(term, host, enabled, true);
}

function installNativeInput(
  term: Terminal,
  host: HTMLElement,
  enabled: () => boolean,
  macSafari: boolean,
) {
  const textarea = term.textarea!;
  if (!macSafari) textarea.classList.add("ios-native-paste-target");
  const doc = host.ownerDocument;
  const screen = term.element!.querySelector<HTMLElement>(".xterm-screen")!;
  const preview = doc.createElement("span");
  preview.className = "ios-native-composition";
  preview.hidden = true;
  screen.append(preview);
  let pending = false;
  let base = 0;
  let standardComposition = false;
  let standardCommit: string | null = null;
  let lastNativeValue = "";
  let echoPreview: HTMLElement | undefined;
  let echoTimer: ReturnType<typeof setTimeout> | undefined;
  let echoAnchor:
    { row: number; col: number; cols: number; text: string } | undefined;
  const clearEcho = () => {
    clearTimeout(echoTimer);
    echoTimer = undefined;
    echoPreview?.remove();
    echoPreview = undefined;
    echoAnchor = undefined;
  };
  const retainUntilEcho = (value: string) => {
    clearEcho();
    if (preview.hidden || !value) return;
    const buffer = term.buffer.active;
    echoAnchor = {
      row: buffer.baseY + buffer.cursorY,
      col: buffer.cursorX,
      cols: term.cols,
      text: value,
    };
    echoPreview = doc.createElement("span");
    echoPreview.className = preview.className;
    echoPreview.style.cssText = preview.style.cssText;
    echoPreview.textContent = value;
    screen.append(echoPreview);
    // A password prompt or TUI may not echo. Never leave stale visual text.
    echoTimer = setTimeout(clearEcho, 700);
  };
  const reconcileEcho = () => {
    if (!echoAnchor || !echoPreview) return;
    const buffer = term.buffer.active;
    if (
      term.cols !== echoAnchor.cols ||
      echoAnchor.row < buffer.viewportY ||
      echoAnchor.row >= buffer.viewportY + term.rows
    ) {
      clearEcho();
      return;
    }
    echoPreview.style.top = `${((echoAnchor.row - buffer.viewportY) * screen.clientHeight) / term.rows}px`;
    Object.assign(
      echoPreview.style,
      terminalCellColors(
        buffer
          .getLine(echoAnchor.row)
          ?.getCell(Math.min(echoAnchor.col, term.cols - 1)),
        term.options.theme,
      ),
    );
    let actual = "";
    let row = echoAnchor.row;
    let col = echoAnchor.col;
    // Compare rendered cells, including wide Hangul and a wrapped line. An
    // unrelated render must not remove the preview before the echoed text paints.
    for (let cells = 0; cells < echoAnchor.text.length * 2 + 2; cells++) {
      if (col >= term.cols) {
        row++;
        col = 0;
      }
      const cell = buffer.getLine(row)?.getCell(col++);
      if (!cell) break;
      if (cell.getWidth() === 0) continue;
      actual += cell.getChars() || " ";
      if (!echoAnchor.text.startsWith(actual)) break;
      if (actual === echoAnchor.text) {
        clearEcho();
        break;
      }
    }
  };
  const ready = () => enabled() && doc.activeElement === textarea;
  const text = () => textarea.value.slice(base);
  const position = () => {
    if (preview.hidden) return;
    const buffer = term.buffer.active;
    const row = buffer.baseY + buffer.cursorY - buffer.viewportY;
    const left =
      (Math.min(buffer.cursorX, term.cols - 1) * screen.clientWidth) /
      term.cols;
    Object.assign(preview.style, {
      ...terminalCellColors(
        buffer
          .getLine(buffer.baseY + buffer.cursorY)
          ?.getCell(Math.min(buffer.cursorX, term.cols - 1)),
        term.options.theme,
      ),
      left: `${left}px`,
      top: `${(Math.max(0, Math.min(row, term.rows - 1)) * screen.clientHeight) / term.rows}px`,
      maxWidth: `${Math.max(1, screen.clientWidth - left)}px`,
      fontFamily: term.options.fontFamily,
      fontSize: `${term.options.fontSize}px`,
      lineHeight: `${screen.clientHeight / term.rows}px`,
    });
  };
  const show = () => {
    lastNativeValue = text();
    preview.textContent = lastNativeValue;
    preview.hidden = !preview.textContent;
    position();
  };
  const clear = (resetDOM = true) => {
    pending = false;
    base = 0;
    preview.hidden = true;
    preview.textContent = "";
    lastNativeValue = "";
    if (resetDOM) textarea.value = "";
  };
  const flush = (retainPreview = false) => {
    if (!pending) return;
    const value = text();
    if (retainPreview && enabled()) retainUntilEcho(value);
    clear();
    if (value && enabled()) term.input(value, true);
  };
  const begin = () => {
    if (pending) return;
    clearEcho();
    base = macSafari ? textarea.selectionStart : textarea.value.length;
    pending = true;
  };
  const listeners: Array<[string, EventListener]> = [];
  const on = (type: string, handler: (event: any) => void) => {
    const listener: EventListener = (event) => {
      if (event.target === textarea) handler(event);
    };
    host.addEventListener(type, listener, true);
    listeners.push([type, listener]);
  };
  on("compositionstart", () => {
    // Real composition events belong to stock xterm, including hardware IMEs.
    // The browser may already have mutated its next composition. Commit only
    // the last observed local run, preserving DOM for xterm's start position.
    if (pending) {
      const value = lastNativeValue;
      clear(false);
      if (value && enabled()) term.input(value, true);
    }
    standardComposition = true;
  });
  on("compositionend", (event: CompositionEvent) => {
    standardCommit = event.data;
    standardComposition = false;
  });
  on("keydown", (event: KeyboardEvent) => {
    if (!ready() || standardComposition || event.isComposing) return;
    if (
      macSafari &&
      event.keyCode === 229 &&
      (pending || hangul.test(event.key))
    ) {
      // Ancestor capture must also block CompositionHelper's deferred diff,
      // even after deleting the final local character. Native editing proceeds.
      event.stopImmediatePropagation();
      return;
    }
    standardCommit = null;
    const modified = event.ctrlKey || event.metaKey || event.altKey;
    if (!modified && event.keyCode === 0 && hangul.test(event.key)) {
      begin();
      event.stopImmediatePropagation();
    } else if (pending && !modified && event.key === "Backspace" && text()) {
      // Do not preventDefault: Safari must delete/recompose its native value.
      event.stopImmediatePropagation();
    } else if (
      pending &&
      !["Shift", "Control", "Alt", "Meta", "CapsLock"].includes(event.key)
    ) {
      flush(event.key === " ");
    }
  });
  on("keypress", (event: KeyboardEvent) => {
    if (pending && ready() && !standardComposition)
      event.stopImmediatePropagation();
  });
  on("keyup", (event: KeyboardEvent) => {
    if (pending && ready() && !standardComposition)
      event.stopImmediatePropagation();
  });
  on("beforeinput", (event: InputEvent) => {
    if (!ready() || standardComposition || event.isComposing) return;
    if (
      macSafari &&
      event.inputType === "insertText" &&
      standardCommit !== null
    ) {
      const isStandardCommit = event.data === standardCommit;
      standardCommit = null;
      if (isStandardCommit) return;
    }
    if (
      macSafari &&
      !pending &&
      event.inputType === "insertText" &&
      event.data &&
      hangul.test(event.data)
    )
      begin();
    if (
      pending &&
      !(macSafari && event.inputType === "insertReplacementText") &&
      event.inputType !== "deleteContentBackward" &&
      !(
        event.inputType === "insertText" &&
        event.data &&
        hangul.test(event.data)
      )
    )
      flush(event.inputType === "insertText" && event.data === " ");
    if (pending) event.stopImmediatePropagation();
  });
  on("input", (event: InputEvent) => {
    if (macSafari && !pending) standardCommit = null;
    if (!pending || !ready() || standardComposition || event.isComposing)
      return;
    event.stopImmediatePropagation();
    show();
  });
  on("paste", () => flush());
  // The actual editable textarea owns its native long-press gesture. Prevent
  // xterm's desktop right-click helper from replacing/selecting its value, and
  // keep mobile scroll/refocus handlers out of the OS selection/menu gesture.
  // Do not preventDefault or read the clipboard: Safari presents its own menu.
  const disposeGestures = macSafari
    ? () => {}
    : preserveNativeEditableGestures(host, textarea);
  on("blur", () => {
    clearEcho();
    // No cross-tab emission: enabled() includes the original tab identity.
    if (pending) {
      const value = text();
      clear();
      if (value && enabled()) term.input(value, true);
    }
  });
  const render = term.onRender(() => {
    reconcileEcho();
    position();
  });
  return {
    flush,
    cancel() {
      clearEcho();
      if (pending) clear();
    },
    dispose() {
      clearEcho();
      if (pending) clear();
      for (const [type, listener] of listeners)
        host.removeEventListener(type, listener, true);
      render.dispose();
      disposeGestures();
      textarea.classList.remove("ios-native-paste-target");
      preview.remove();
    },
  };
}
