import type { Terminal } from "@xterm/xterm";
import { preserveNativeEditableGestures } from "./native-clipboard.ts";

// Android must focus the pinned top-left textarea when opening the keyboard.
// Only the already-focused, keyboard-visible state exposes an input-row target.
// This is geometry/gesture handling only: xterm owns all input and paste events.
export function installAndroidNativePaste(term: Terminal, host: HTMLElement) {
  const textarea = term.textarea!;
  const screen = term.element!.querySelector<HTMLElement>(".xterm-screen")!;
  textarea.classList.add("android-native-paste-target");
  const position = () => {
    const buffer = term.buffer.active;
    const cellHeight = screen.clientHeight / Math.max(1, term.rows);
    const row = buffer.baseY + buffer.cursorY - buffer.viewportY;
    // A cursor-sized target misses a finger placed on the typed text. Cover the
    // input row, with a touch-height band clamped inside the terminal viewport.
    const height = Math.min(screen.clientHeight, Math.max(44, cellHeight));
    const top = Math.max(
      0,
      Math.min(
        row * cellHeight - (height - cellHeight) / 2,
        screen.clientHeight - height,
      ),
    );
    textarea.style.setProperty("--paste-x", "0px");
    textarea.style.setProperty("--paste-y", `${top}px`);
    textarea.style.setProperty(
      "--paste-width",
      `${Math.max(1, screen.clientWidth)}px`,
    );
    textarea.style.setProperty("--paste-height", `${Math.max(1, height)}px`);
  };
  const disposeGestures = preserveNativeEditableGestures(host, textarea);
  const render = term.onRender(position);
  position();
  return () => {
    render.dispose();
    disposeGestures();
    textarea.classList.remove("android-native-paste-target");
    for (const name of [
      "--paste-x",
      "--paste-y",
      "--paste-width",
      "--paste-height",
    ])
      textarea.style.removeProperty(name);
  };
}
