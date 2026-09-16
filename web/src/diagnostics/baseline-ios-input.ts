import type { Terminal } from "@xterm/xterm";

// Hold IME edits locally. Never repair remote text with backspaces: the shell/TUI
// owns its cursor and may count graphemes differently from the browser.
export function createCompositionCommit(
  emit: (text: string) => void,
  reset: () => void,
  preview: (text: string) => void,
  enabled: () => boolean,
) {
  let composing = false;
  let ending = false;
  let text = "";
  let timer: ReturnType<typeof setTimeout> | undefined;
  const cancel = () => {
    clearTimeout(timer);
    timer = undefined;
    composing = ending = false;
    text = "";
    preview("");
    reset();
  };
  const commit = () => {
    const value = text.normalize("NFC");
    cancel();
    if (value && enabled()) emit(value);
  };
  return {
    get composing() {
      return composing;
    },
    begin() {
      if (ending) commit();
      composing = true;
      text = "";
    },
    update(value: string) {
      text = value;
      preview(value);
    },
    end(value: string) {
      composing = false;
      ending = true;
      text = value;
      preview(value);
      clearTimeout(timer);
      // WebKit may mutate textarea again in the input event following compositionend.
      timer = setTimeout(commit, 0);
    },
    input(value: string, isComposing: boolean) {
      text = value;
      if (isComposing) composing = true;
      if (composing || ending) preview(value);
      else commit();
    },
    flush() {
      if (!composing && ending) commit();
    },
    cancel,
  };
}

export function installIOSInput(
  term: Terminal,
  host: HTMLElement,
  enabled: () => boolean,
  emit: (text: string) => void,
) {
  const textarea = term.textarea!;
  const ready = () => enabled() && document.activeElement === textarea;
  // An editable sentinel lets the software keyboard issue Backspace at an empty
  // input. It never enters the wire stream or the composition preview.
  const sentinel = "\u200b";
  const reset = () => {
    textarea.value = sentinel;
    textarea.setSelectionRange(1, 1);
  };
  const read = () => textarea.value.replace(/^\u200b/, "");
  textarea.setAttribute("inputmode", "text");
  textarea.setAttribute("enterkeyhint", "enter");
  textarea.setAttribute("autocorrect", "off");
  textarea.setAttribute("autocapitalize", "off");
  textarea.spellcheck = false;
  host.classList.add("ios-direct-input");
  const preview = document.createElement("span");
  preview.className = "ios-composition";
  preview.hidden = true;
  term.element!.querySelector(".xterm-screen")!.append(preview);
  const position = () => {
    if (preview.hidden) return;
    const screen = term.element!.querySelector<HTMLElement>(".xterm-screen")!;
    const buffer = term.buffer.active;
    const row = buffer.baseY + buffer.cursorY - buffer.viewportY;
    preview.style.left = `${(Math.min(buffer.cursorX, term.cols - 1) * screen.clientWidth) / term.cols}px`;
    preview.style.top = `${(Math.max(0, Math.min(row, term.rows - 1)) * screen.clientHeight) / term.rows}px`;
    preview.style.fontFamily = term.options.fontFamily!;
    preview.style.fontSize = `${term.options.fontSize}px`;
    preview.style.lineHeight = `${screen.clientHeight / term.rows}px`;
  };
  const ime = createCompositionCommit(
    emit,
    reset,
    (value) => {
      preview.textContent = value;
      preview.hidden = !value;
      position();
    },
    ready,
  );
  const disposers: (() => void)[] = [];
  type InputEvents = Omit<HTMLElementEventMap, "input"> & { input: InputEvent };
  const on = <K extends keyof InputEvents>(
    name: K,
    listener: (event: InputEvents[K]) => void,
  ) => {
    const handler = (event: Event) => {
      if (event.target !== textarea) return;
      listener(event as InputEvents[K]);
    };
    // Ancestor capture runs before xterm's own textarea capture handlers.
    host.addEventListener(name, handler, true);
    disposers.push(() => host.removeEventListener(name, handler, true));
  };
  on("compositionstart", (event: CompositionEvent) => {
    event.stopImmediatePropagation();
    if (ready()) ime.begin();
  });
  on("compositionupdate", (event: CompositionEvent) => {
    event.stopImmediatePropagation();
    if (ready()) ime.update(event.data);
  });
  on("compositionend", (event: CompositionEvent) => {
    event.stopImmediatePropagation();
    if (ready()) ime.end(event.data);
    else ime.cancel();
  });
  on("input", (event: InputEvent) => {
    event.stopImmediatePropagation();
    if (!ready()) {
      ime.cancel();
      return;
    }
    ime.input(read(), event.isComposing);
  });
  on("beforeinput", (event: InputEvent) => {
    if (!ready()) {
      event.preventDefault();
      ime.cancel();
      return;
    }
    if (ime.composing || event.isComposing) return;
    const key =
      event.inputType === "deleteContentBackward"
        ? "\x7f"
        : event.inputType === "insertLineBreak" ||
            event.inputType === "insertParagraph"
          ? "\r"
          : undefined;
    if (key) {
      event.preventDefault();
      event.stopImmediatePropagation();
      ime.flush();
      emit(key);
      reset();
    }
  });
  on("keydown", (event: KeyboardEvent) => {
    if (!ready()) {
      event.preventDefault();
      event.stopImmediatePropagation();
      return;
    }
    if (ime.composing || event.isComposing || event.keyCode === 229) {
      event.stopImmediatePropagation();
      return;
    }
    ime.flush();
    if (
      event.key.length === 1 &&
      !event.ctrlKey &&
      !event.metaKey &&
      !event.altKey
    ) {
      event.stopImmediatePropagation(); // native input, not xterm's keydown + input twice
    }
    // Non-text keys retain xterm's terminal mappings and preventDefault behavior.
  });
  on("keypress", (event: KeyboardEvent) => event.stopImmediatePropagation());
  on("paste", (event: ClipboardEvent) => {
    event.preventDefault();
    event.stopImmediatePropagation();
    if (!ready() || ime.composing) return;
    ime.flush();
    const value = event.clipboardData?.getData("text/plain");
    if (value) term.paste(value.normalize("NFC"));
    reset();
  });
  on("focus", () => {
    if (!ime.composing) reset();
  });
  on("blur", () => ime.cancel());
  const render = term.onRender(position);
  const scroll = term.onScroll(position);
  reset();
  return {
    cancel: ime.cancel,
    flush: ime.flush,
    get composing() {
      return ime.composing;
    },
    dispose() {
      ime.cancel();
      render.dispose();
      scroll.dispose();
      disposers.forEach((fn) => fn());
      preview.remove();
    },
  };
}
