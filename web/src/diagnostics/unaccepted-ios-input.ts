import type { Terminal } from "@xterm/xterm";
import { assemble, removeLastCharacter } from "es-hangul";

const graphemes = new Intl.Segmenter("ko", { granularity: "grapheme" });
function withoutLastGrapheme(value: string) {
  const parts = [...graphemes.segment(value)];
  return value.slice(0, parts.at(-1)?.index ?? 0);
}

// Browser context is retained after commits. Only the uncommitted suffix may be
// changed by IME; a DOM replacement can never rewrite the remote terminal prefix.
export function createCompositionCommit(
  emit: (text: string) => void,
  write: (text: string, deferred?: boolean) => void,
  preview: (text: string) => void,
  enabled: () => boolean,
) {
  let nativeValue = "";
  let committed = "";
  let value = "";
  let composing = false;
  let pending = false;
  let deleting = false;
  let compositionBase = "";
  let timer: ReturnType<typeof setTimeout> | undefined;
  let deletion:
    { value: string; local: boolean; collapsed: boolean } | undefined;
  let recovering = false;
  let staleRepair: string | undefined;
  const stopTimer = () => {
    clearTimeout(timer);
    timer = undefined;
  };
  const show = () => preview(pending ? value.slice(committed.length) : "");
  const cancel = () => {
    stopTimer();
    nativeValue = committed = value = compositionBase = "";
    composing = pending = deleting = false;
    deletion = undefined;
    recovering = false;
    staleRepair = undefined;
    preview("");
    write("");
  };
  const commit = (keepTail = true) => {
    stopTimer();
    if (!enabled()) {
      cancel();
      return;
    }
    const lastStart = withoutLastGrapheme(value).length;
    const end =
      keepTail && /^[가-힣ㄱ-ㅎㅏ-ㅣ]+$/.test(value.slice(lastStart))
        ? Math.max(committed.length, lastStart)
        : value.length;
    const text = value.slice(committed.length, end);
    if (text) emit(text);
    committed = value.slice(0, end);
    pending = value.length > committed.length;
    composing = deleting = false;
    show();
    if (recovering && nativeValue !== value) {
      staleRepair = nativeValue;
      write(value, true);
    }
    // Bound the local mirror; compaction occurs only after native composition ends.
    if (committed.length > 4096) {
      const cut = [...graphemes.segment(committed)].at(-128)?.index ?? 0;
      committed = committed.slice(cut);
      value = value.slice(cut);
      nativeValue = value;
      write(value, true);
    }
    // Keep the final Korean syllable local even after compositionend: deleting a
    // jongseong must not require a remote Backspace + replacement transaction.
  };
  const accept = (next: string) => {
    if (!next.startsWith(committed)) {
      // Late IME replacement of already sent text is not a new append operation.
      staleRepair = next;
      write(value, true);
      return false;
    }
    const tail = next.slice(committed.length);
    value = recovering && tail ? committed + assemble([tail]) : next;
    return true;
  };
  return {
    get composing() {
      return composing;
    },
    get pending() {
      return pending;
    },
    begin(offset?: number) {
      stopTimer();
      composing = pending = true;
      deleting = false;
      // Native composition can replace a selected local tail after a deletion.
      compositionBase = value.slice(
        0,
        Math.max(committed.length, offset ?? value.length),
      );
    },
    update(text: string, snapshot?: string) {
      pending = true;
      const next = (snapshot ?? compositionBase + text).normalize("NFC");
      if (next.startsWith(committed)) preview(next.slice(committed.length));
    },
    end(text: string, snapshot?: string) {
      if (!composing && !pending) return;
      composing = false;
      if (deleting) {
        show();
        return;
      }
      // compositionend.data is a replacement payload, never an append. On iOS
      // compositionstart may arrive after the DOM/selection already moved.
      // Use the native full snapshot; final input can refine it before the timer.
      if (accept((snapshot ?? compositionBase + text).normalize("NFC"))) show();
      stopTimer();
      timer = setTimeout(commit, 0);
    },
    beforeInput(inputType: string, collapsed = true, beforeValue = value) {
      staleRepair = undefined;
      if (inputType.startsWith("delete")) {
        deletion = {
          value: beforeValue.normalize("NFC"),
          local:
            pending && beforeValue.normalize("NFC").length > committed.length,
          collapsed,
        };
        if (deletion.local) {
          stopTimer();
          deleting = true;
        }
      } else {
        deletion = undefined;
        // A new insertion ends the previous deletion transaction.
        deleting = false;
      }
    },
    input(next: string, isComposing: boolean, inputType = "insertText") {
      if (!enabled()) {
        cancel();
        return;
      }
      next = next.normalize("NFC");
      const previousNative = nativeValue;
      nativeValue = next;
      if (
        staleRepair !== undefined &&
        next === staleRepair &&
        !deletion &&
        !inputType.startsWith("delete")
      ) {
        write(value, true);
        return;
      }
      if (inputType.startsWith("delete") || deletion) {
        const edit = deletion ?? {
          value: previousNative,
          local: pending && previousNative.length > committed.length,
          collapsed: false,
        };
        deletion = undefined;
        if (edit.local) {
          stopTimer();
          const tail = edit.value.slice(committed.length);
          const wholeDeleted = withoutLastGrapheme(tail);
          const last = tail.slice(wholeDeleted.length);
          const phonemeDeleted = /^[가-힣ㄱ-ㅎㅏ-ㅣ]+$/.test(last)
            ? wholeDeleted + removeLastCharacter(last)
            : wholeDeleted;
          // Trust native partial deletion. Only repair a whole-grapheme deletion
          // of the local IME tail; committed characters are never decomposed.
          if (
            edit.collapsed &&
            inputType !== "deleteContentForward" &&
            next === committed + wholeDeleted &&
            phonemeDeleted !== wholeDeleted
          ) {
            staleRepair = next;
            next = committed + phonemeDeleted;
            recovering = true;
            write(next, true);
          }
          if (!accept(next)) return;
          pending = true;
          deleting = true;
          composing = isComposing;
          show();
        } else {
          // Exactly one remote key per native delete input. Never emit the DOM
          // value as text, including non-cancelable edits and missing beforeinput.
          emit(inputType === "deleteContentForward" ? "\x1b[3~" : "\x7f");
          value = committed = next;
          pending = composing = deleting = false;
          show();
          if (!next) write(""); // Restore the sentinel for another empty delete.
        }
        return;
      }
      if (!accept(next)) return;
      if (isComposing) {
        if (!composing) compositionBase = committed;
        composing = pending = true;
      }
      if (composing || timer) show();
      else commit();
    },
    finish() {
      composing = false;
      commit(false);
      cancel();
    },
    flush() {
      if (composing) return;
      if (pending) commit(false);
      // Terminal control keys move the remote cursor, invalidating local context.
      cancel();
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
  let writeEpoch = 0;
  const write = (value: string, deferred = false) => {
    const epoch = ++writeEpoch;
    const apply = () => {
      if (epoch !== writeEpoch || (deferred && !ready())) return;
      textarea.value = sentinel + value;
      textarea.setSelectionRange(textarea.value.length, textarea.value.length);
    };
    if (deferred) queueMicrotask(apply);
    else apply();
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
  const previewText = document.createElement("span");
  previewText.className = "ios-composition-text";
  preview.append(previewText);
  term.element!.querySelector(".xterm-screen")!.append(preview);
  const position = () => {
    if (preview.hidden) return;
    const screen = term.element!.querySelector<HTMLElement>(".xterm-screen")!;
    const buffer = term.buffer.active;
    const row = buffer.baseY + buffer.cursorY - buffer.viewportY;
    const cellHeight = screen.clientHeight / term.rows;
    preview.style.left = "0px";
    preview.style.width = `${screen.clientWidth}px`;
    preview.style.textIndent = `${(Math.min(buffer.cursorX, term.cols - 1) * screen.clientWidth) / term.cols}px`;
    preview.style.maxHeight = `${screen.clientHeight}px`;
    preview.style.fontFamily = term.options.fontFamily!;
    preview.style.fontSize = `${term.options.fontSize}px`;
    preview.style.lineHeight = `${cellHeight}px`;
    const needed = Math.max(cellHeight, preview.scrollHeight);
    const anchor = Math.max(0, Math.min(row, term.rows - 1)) * cellHeight;
    preview.style.top = `${Math.max(0, Math.min(anchor, screen.clientHeight - needed))}px`;
    // Long native compositions must stay visible at the bottom of a small viewport.
    preview.scrollTop = preview.scrollHeight;
  };
  const ime = createCompositionCommit(
    emit,
    write,
    (value) => {
      previewText.textContent = value;
      preview.hidden = !value;
      position();
    },
    enabled,
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
    if (ready())
      ime.begin(
        read()
          .slice(
            0,
            Math.max(
              0,
              (textarea.selectionStart ?? textarea.value.length) -
                sentinel.length,
            ),
          )
          .normalize("NFC").length,
      );
  });
  on("compositionupdate", (event: CompositionEvent) => {
    event.stopImmediatePropagation();
    if (ready()) ime.update(event.data, read() || event.data);
  });
  on("compositionend", (event: CompositionEvent) => {
    event.stopImmediatePropagation();
    if (ready()) ime.end(event.data, read());
    else ime.cancel();
  });
  on("input", (event: InputEvent) => {
    event.stopImmediatePropagation();
    if (!ready()) {
      ime.cancel();
      return;
    }
    if (
      event.inputType === "insertLineBreak" ||
      event.inputType === "insertParagraph"
    ) {
      ime.finish();
      emit("\r");
      ime.cancel();
      return;
    }
    ime.input(read(), event.isComposing, event.inputType);
  });
  on("beforeinput", (event: InputEvent) => {
    event.stopImmediatePropagation();
    if (!ready()) {
      event.preventDefault();
      ime.cancel();
      return;
    }
    // Observe native edits; beforeinput may be non-cancelable or absent on iOS.
    // The input event is the sole owner of deletion dispatch.
    ime.beforeInput(
      event.inputType,
      textarea.selectionStart === textarea.selectionEnd,
      read(),
    );
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
    if (event.key === "Backspace" || event.key === "Enter") {
      event.stopImmediatePropagation();
      return; // native beforeinput/input owns these keys exactly once
    }
    if (
      event.key.length === 1 &&
      !event.ctrlKey &&
      !event.metaKey &&
      !event.altKey
    ) {
      event.stopImmediatePropagation();
    } else if (
      !["Shift", "Control", "Alt", "Meta", "CapsLock"].includes(event.key)
    ) {
      ime.flush();
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
    write("");
  });
  on("focus", () => {
    if (!ime.composing) ime.cancel();
  });
  // Focus has already moved when blur fires. The identity/connection gate, not
  // document.activeElement, decides whether the visible suffix can be finalized.
  on("blur", () => {
    if (enabled()) ime.finish();
    else ime.cancel();
  });
  const render = term.onRender(position);
  const scroll = term.onScroll(position);
  write("");
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
