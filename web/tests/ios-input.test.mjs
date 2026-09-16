import test from "node:test";
import assert from "node:assert/strict";
import {
  createCompositionCommit,
  installIOSInput,
} from "../src/diagnostics/unaccepted-ios-input.ts";
const settle = () => new Promise((resolve) => setTimeout(resolve, 5));
function state() {
  const sent = [],
    writes = [];
  let active = true,
    shown = "";
  const ime = createCompositionCommit(
    (text) => sent.push(text),
    (text, deferred) => writes.push({ text, deferred }),
    (text) => (shown = text),
    () => active,
  );
  return {
    ime,
    sent,
    writes,
    disable: () => (active = false),
    shown: () => shown,
  };
}
async function korean(s, text, full = text) {
  s.ime.begin();
  s.ime.update(text);
  s.ime.input(full, true);
  s.ime.end(text);
  s.ime.input(full, false, "insertFromComposition");
  await settle();
}
test("Korean tail stays visible and editable after compositionend; NFC on flush", async () => {
  const s = state();
  await korean(s, "한", "한");
  assert.deepEqual(s.sent, []);
  assert.equal(s.shown(), "한");
  assert.deepEqual(s.writes, []);
  s.ime.flush();
  assert.deepEqual(s.sent, ["한"]);
  assert.equal(s.shown(), "");
});
test("new syllable emits only stable prefix; delete final consonant before Enter", async () => {
  const s = state();
  await korean(s, "한");
  await korean(s, "글", "한글");
  assert.deepEqual(s.sent, ["한"]);
  assert.equal(s.shown(), "글");
  s.ime.beforeInput("deleteContentBackward");
  s.ime.input("한그", false, "deleteContentBackward");
  assert.deepEqual(s.sent, ["한"]);
  assert.equal(s.shown(), "그");
  s.ime.finish();
  s.sent.push("\r");
  assert.deepEqual(s.sent, ["한", "그", "\r"]);
});
test("whole local syllable deletion repairs one phoneme repeatedly, never remote text", async () => {
  const s = state();
  await korean(s, "값");
  for (const expected of ["갑", "가", "ㄱ", ""]) {
    s.ime.beforeInput("deleteContentBackward");
    s.ime.input("", false, "deleteContentBackward");
    s.ime.end("");
    await settle();
    assert.equal(s.shown(), expected);
    assert.deepEqual(s.sent, []);
  }
  s.ime.beforeInput("deleteContentBackward");
  s.ime.input("", false, "deleteContentBackward");
  assert.deepEqual(s.sent, ["\x7f"]);
});
test("compound vowels decompose locally, including recovery followed by more typing", async () => {
  const s = state();
  await korean(s, "화");
  s.ime.beforeInput("deleteContentBackward");
  s.ime.input("", false, "deleteContentBackward");
  assert.equal(s.shown(), "호");
  s.ime.begin(1);
  s.ime.beforeInput("insertCompositionText");
  s.ime.input("호ㅏ", true);
  s.ime.end("ㅏ");
  await settle();
  assert.equal(s.shown(), "화");
  assert.deepEqual(s.sent, []);
  s.ime.finish();
  assert.deepEqual(s.sent, ["화"]);
});
test("both delete/end/input event orders retain the pre-delete snapshot", async () => {
  for (const endFirst of [true, false]) {
    const s = state();
    s.ime.begin();
    s.ime.input("값", true);
    s.ime.beforeInput("deleteContentBackward");
    if (endFirst) s.ime.end("");
    s.ime.input("", false, "deleteContentBackward");
    if (!endFirst) s.ime.end("");
    await settle();
    assert.equal(s.shown(), "갑");
    assert.deepEqual(s.sent, []);
  }
});
test("selection deletion and missing-beforeinput do not invent phoneme recovery", async () => {
  const s = state();
  await korean(s, "가");
  s.ime.beforeInput("deleteContentBackward", false);
  s.ime.input("", false, "deleteContentBackward");
  assert.equal(s.shown(), "");
  assert.deepEqual(s.sent, []);
  s.ime.cancel();
  s.ime.input("abc", false);
  s.ime.input("ab", false, "deleteContentBackward");
  assert.deepEqual(s.sent, ["abc", "\x7f"]);
});
test("non-cancelable deletes dispatch one key, not retained DOM contents", () => {
  const s = state();
  s.ime.input("abc", false);
  for (const next of ["ab", "a", ""]) {
    s.ime.beforeInput("deleteContentBackward");
    s.ime.input(next, false, "deleteContentBackward");
  }
  s.ime.beforeInput("insertText");
  s.ime.input("x", false);
  assert.deepEqual(s.sent, ["abc", "\x7f", "\x7f", "\x7f", "x"]);
});
test("delayed replacement cannot replay a committed prefix or undo a repair", async () => {
  const s = state();
  s.ime.input("abc", false);
  s.ime.input("xbc", false, "insertReplacementText");
  assert.deepEqual(s.sent, ["abc"]);
  s.ime.cancel();
  await korean(s, "가");
  s.ime.beforeInput("deleteContentBackward");
  s.ime.input("", false, "deleteContentBackward");
  s.ime.input("", false, "insertFromComposition");
  s.ime.end("");
  await settle();
  assert.equal(s.shown(), "ㄱ");
  assert.deepEqual(s.sent, ["abc"]);
});
test("new composition cancels old end timer and retains native context", async () => {
  const s = state();
  s.ime.begin();
  s.ime.input("한", true);
  s.ime.end("한");
  s.ime.begin(1);
  s.ime.input("한ㄱ", true);
  await settle();
  assert.deepEqual(s.sent, []);
  assert.equal(s.shown(), "한ㄱ");
  assert.deepEqual(s.writes, []);
  s.ime.end("글");
  await settle();
  assert.deepEqual(s.sent, ["한"]);
  assert.equal(s.shown(), "글");
});
test("ASCII/emoji append once; controls/cancellation clear context; inactive timers cannot emit", async () => {
  const s = state();
  s.ime.input("a", false);
  s.ime.input("a🙂", false);
  s.ime.input("a🙂", false);
  assert.deepEqual(s.sent, ["a", "🙂"]);
  s.ime.flush();
  s.ime.input("b", false);
  assert.deepEqual(s.sent, ["a", "🙂", "b"]);
  s.ime.cancel();
  s.ime.begin();
  s.ime.end("한글");
  s.disable();
  await settle();
  assert.deepEqual(s.sent, ["a", "🙂", "b"]);
});
function fixture(t) {
  const original = globalThis.document;
  const preview = {
    style: {},
    hidden: true,
    child: undefined,
    scrollHeight: 20,
    append(child) {
      this.child = child;
    },
    get textContent() {
      return this.child?.textContent || "";
    },
    remove() {},
  };
  let elements = 0;
  globalThis.document = {
    createElement: () => (elements++ === 0 ? preview : { textContent: "" }),
  };
  t.after(() => (globalThis.document = original));
  const textarea = {
    value: "",
    selectionStart: 0,
    selectionEnd: 0,
    setAttribute() {},
    setSelectionRange(start, end) {
      this.selectionStart = start;
      this.selectionEnd = end;
    },
  };
  globalThis.document.activeElement = textarea;
  const handlers = new Map();
  const sent = [];
  const pasted = [];
  const screen = { clientWidth: 800, clientHeight: 400, append() {} };
  const term = {
    textarea,
    element: { querySelector: () => screen },
    cols: 80,
    rows: 20,
    buffer: { active: { baseY: 0, viewportY: 0, cursorX: 2, cursorY: 3 } },
    options: { fontSize: 14, fontFamily: "monospace" },
    paste: (value) => pasted.push(value),
    onRender: () => ({ dispose() {} }),
    onScroll: () => ({ dispose() {} }),
  };
  const host = {
    classList: { add() {} },
    addEventListener(name, handler, capture) {
      assert.equal(capture, true);
      handlers.set(name, handler);
    },
    removeEventListener(name) {
      handlers.delete(name);
    },
  };
  const input = installIOSInput(
    term,
    host,
    () => true,
    (value) => sent.push(value),
  );
  t.after(() => input.dispose());
  const fire = (name, fields = {}) => {
    const event = {
      target: textarea,
      prevented: false,
      stopped: false,
      preventDefault() {
        this.prevented = true;
      },
      stopImmediatePropagation() {
        this.stopped = true;
      },
      ...fields,
    };
    handlers.get(name)?.(event);
    return event;
  };
  return { textarea, sent, pasted, fire, preview, input, term, screen };
}

function setValue(f, value) {
  f.textarea.value = "\u200b" + value;
  f.textarea.setSelectionRange(
    f.textarea.value.length,
    f.textarea.value.length,
  );
}
test("inline preview remains visible through compositionend and local deletion", async (t) => {
  const f = fixture(t);
  f.fire("compositionstart");
  f.fire("compositionupdate", { data: "한" });
  assert.equal(f.preview.textContent, "한");
  assert.equal(f.preview.hidden, false);
  setValue(f, "한");
  f.fire("input", { isComposing: true, inputType: "insertCompositionText" });
  f.fire("compositionend", { data: "한" });
  await settle();
  assert.deepEqual(f.sent, []);
  assert.equal(f.preview.textContent, "한");
  assert.equal(f.textarea.value, "\u200b한");
  f.fire("beforeinput", { inputType: "deleteContentBackward" });
  setValue(f, "");
  f.fire("input", { inputType: "deleteContentBackward", isComposing: false });
  await settle();
  assert.equal(f.preview.textContent, "하");
  assert.equal(f.textarea.value, "\u200b하");
  assert.deepEqual(f.sent, []);
});
test("keydown + non-cancelable beforeinput + input produces exactly one Backspace or Enter", (t) => {
  const f = fixture(t);
  setValue(f, "abc");
  f.fire("input", { inputType: "insertText" });
  assert.equal(
    f.fire("keydown", { key: "Backspace", keyCode: 8 }).stopped,
    true,
  );
  assert.equal(
    f.fire("beforeinput", {
      inputType: "deleteContentBackward",
      cancelable: false,
    }).prevented,
    false,
  );
  setValue(f, "ab");
  f.fire("input", { inputType: "deleteContentBackward" });
  f.fire("keydown", { key: "Enter", keyCode: 13 });
  f.fire("beforeinput", { inputType: "insertLineBreak" });
  setValue(f, "ab\n");
  f.fire("input", { inputType: "insertLineBreak" });
  assert.deepEqual(f.sent, ["abc", "\x7f", "\r"]);
});
test("printable keys and paste remain single; blur finalizes visible tail", async (t) => {
  const f = fixture(t);
  const key = f.fire("keydown", { key: "a", keyCode: 65 });
  assert.equal(key.stopped, true);
  assert.equal(key.prevented, false);
  setValue(f, "a");
  f.fire("input", { inputType: "insertText" });
  f.fire("paste", { clipboardData: { getData: () => "한\n글" } });
  assert.deepEqual(f.pasted, ["한\n글"]);
  f.fire("compositionstart");
  setValue(f, "가");
  f.fire("input", { isComposing: true, inputType: "insertCompositionText" });
  f.fire("beforeinput", { inputType: "deleteContentBackward" });
  setValue(f, "");
  f.fire("input", { inputType: "deleteContentBackward" });
  f.fire("blur");
  await settle();
  assert.deepEqual(f.sent, ["a", "ㄱ"]);
  assert.equal(f.textarea.value, "\u200b");
  assert.equal(f.preview.hidden, true);
});

test("long composition stays visible at narrow terminal right/bottom edges", (t) => {
  const f = fixture(t);
  f.screen.clientWidth = 240;
  f.screen.clientHeight = 80;
  f.term.cols = 24;
  f.term.rows = 4;
  f.term.buffer.active.cursorX = 23;
  f.term.buffer.active.cursorY = 3;
  f.preview.scrollHeight = 60;
  f.fire("compositionstart");
  f.fire("compositionupdate", { data: "한글을계속입력하고삭제합니다" });
  assert.equal(f.preview.hidden, false);
  assert.equal(f.preview.style.textIndent, "230px");
  assert.equal(f.preview.style.width, "240px");
  assert.equal(f.preview.style.top, "20px");
  assert.equal(f.preview.textContent, "한글을계속입력하고삭제합니다");
});

test("end-empty before a missing-beforeinput deletion cannot delete prior remote text", async () => {
  const s = state();
  s.ime.input("abc", false);
  s.ime.begin();
  s.ime.input("abc한", true);
  s.ime.end("");
  s.ime.input("abc", false, "deleteContentBackward");
  await settle();
  assert.deepEqual(s.sent, ["abc"]);
  assert.equal(s.shown(), "");
});
test("blur finalizes the displayed Korean tail into the old focused session once", async (t) => {
  const f = fixture(t);
  f.fire("compositionstart");
  setValue(f, "한");
  f.fire("input", { inputType: "insertCompositionText", isComposing: true });
  f.fire("compositionend", { data: "한" });
  globalThis.document.activeElement = null;
  f.fire("blur");
  await settle();
  assert.deepEqual(f.sent, ["한"]);
  assert.equal(f.preview.hidden, true);
  f.fire("input", { inputType: "insertFromComposition", isComposing: false });
  assert.deepEqual(f.sent, ["한"]);
});
test("release cancels pending repairs and tail without replay after disposal", async (t) => {
  const f = fixture(t);
  f.fire("compositionstart");
  setValue(f, "가");
  f.fire("input", { inputType: "insertCompositionText", isComposing: true });
  f.fire("beforeinput", { inputType: "deleteContentBackward" });
  setValue(f, "");
  f.fire("input", { inputType: "deleteContentBackward" });
  f.input.cancel();
  await settle();
  assert.deepEqual(f.sent, []);
  assert.equal(f.textarea.value, "\u200b");
});

test("mirror compaction preserves graphemes and never emits retained history again", () => {
  const s = state();
  const value = "a".repeat(3900) + "🙂".repeat(200);
  s.ime.input(value, false);
  const retained = s.writes.at(-1).text;
  assert.ok(!/^[\uDC00-\uDFFF]/.test(retained));
  assert.equal(
    [...new Intl.Segmenter("ko", { granularity: "grapheme" }).segment(retained)]
      .length,
    128,
  );
  s.ime.input(retained + "x", false);
  assert.deepEqual(s.sent, [value, "x"]);
});
test("selection deletion crossing emitted prefix cannot replace earlier terminal text", async () => {
  const s = state();
  s.ime.input("abc", false);
  await korean(s, "한", "abc한");
  s.ime.beforeInput("deleteContentBackward", false);
  s.ime.input("ab", false, "deleteContentBackward");
  assert.deepEqual(s.sent, ["abc"]);
  assert.equal(s.shown(), "한");
  assert.equal(s.writes.at(-1).deferred, true);
});
test("NFD DOM context across another composition and native partial deletion remains stable", async (t) => {
  const f = fixture(t);
  f.fire("compositionstart");
  setValue(f, "한");
  f.fire("input", { inputType: "insertCompositionText", isComposing: true });
  f.fire("compositionend", { data: "한" });
  await settle();
  f.fire("compositionstart");
  setValue(f, "한글");
  f.fire("input", { inputType: "insertCompositionText", isComposing: true });
  f.fire("compositionend", { data: "글" });
  await settle();
  assert.deepEqual(f.sent, ["한"]);
  assert.equal(f.preview.textContent, "글");
  f.fire("beforeinput", { inputType: "deleteContentBackward" });
  setValue(f, "한그");
  f.fire("input", { inputType: "deleteContentBackward", isComposing: false });
  assert.equal(f.preview.textContent, "그");
  assert.deepEqual(f.sent, ["한"]);
});

test("repeated iOS composition starts after DOM mutation never append intermediate jamo", async (t) => {
  const f = fixture(t);
  for (const snapshot of [
    "ㅁ",
    "모",
    "몯",
    "모드",
    "모든",
    "모든ㅈ",
    "모든자",
    "모든자ㅁ",
    "모든자모",
  ]) {
    setValue(f, snapshot);
    f.fire("compositionstart");
    f.fire("compositionupdate", { data: snapshot.at(-1) });
    f.fire("input", { inputType: "insertCompositionText", isComposing: true });
    f.fire("compositionend", { data: snapshot.at(-1) });
    await settle();
    assert.equal(f.sent.join("") + f.preview.textContent, snapshot);
  }
  f.input.flush();
  assert.equal(f.sent.join(""), "모든자모");
});
test("end replacement payload cannot append to the retained textarea prefix", async (t) => {
  const f = fixture(t);
  for (const snapshot of ["ㅁ", "모", "모든"]) {
    setValue(f, snapshot);
    f.fire("compositionstart");
    f.fire("compositionend", { data: snapshot });
    await settle();
    assert.equal(f.sent.join("") + f.preview.textContent, snapshot);
  }
});
