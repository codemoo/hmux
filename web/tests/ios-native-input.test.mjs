import test from "node:test";
import assert from "node:assert/strict";
import {
  installIOSNativeInput,
  installMacSafariNativeInput,
} from "../src/ios-native-input.ts";
import { releaseTerminalView } from "../src/terminal-session.ts";
function setup(macSafari = false) {
  const host = new EventTarget();
  const classList = () => {
    const classes = new Set();
    return {
      add: (name) => classes.add(name),
      remove: (name) => classes.delete(name),
      contains: (name) => classes.has(name),
    };
  };
  const style = () => ({
    setProperty(name, value) {
      this[name] = value;
    },
    removeProperty(name) {
      delete this[name];
    },
  });
  const textarea = {
    value: "",
    selectionStart: 0,
    selectionEnd: 0,
    classList: classList(),
    style: style(),
  };
  const elements = [];
  const makeElement = () => {
    const element = {
      style: style(),
      classList: classList(),
      children: [],
      offsetHeight: 16,
      setAttribute() {},
      append(child) {
        this.children.push(child);
      },
      hidden: false,
      textContent: "",
      removed: false,
      remove() {
        this.removed = true;
      },
    };
    elements.push(element);
    return element;
  };
  host.ownerDocument = {
    activeElement: textarea,
    createElement: makeElement,
  };
  const screen = {
    clientWidth: 390,
    clientHeight: 160,
    append() {},
    ownerDocument: host.ownerDocument,
    classList: classList(),
    style: style(),
  };
  const layers = () =>
    elements.filter((e) => e.className === "native-input-layer");
  const visual = (layer) => ({
    get textContent() {
      return layer.children[0].children[0].textContent;
    },
    get hidden() {
      return layer.hidden;
    },
    get removed() {
      return layer.removed;
    },
  });
  const sent = [];
  let enabled = true;
  let render;
  let painted = [];
  const wrapped = new Set();
  const term = {
    textarea,
    element: { querySelector: () => screen },
    buffer: {
      active: {
        baseY: 0,
        cursorY: 0,
        viewportY: 0,
        cursorX: 0,
        getLine: (row) => ({
          isWrapped: wrapped.has(row),
          getCell: (col) => ({
            getChars: () => painted[row * 40 + col] ?? "",
            getWidth: () => (painted[row * 40 + col] === null ? 0 : 2),
            getFgColor: () => -1,
            getBgColor: () => -1,
            isFgRGB: () => false,
            isBgRGB: () => false,
            isFgPalette: () => false,
            isBgPalette: () => false,
            isInverse: () => 0,
          }),
        }),
      },
    },
    cols: 40,
    rows: 10,
    options: { fontSize: 12, fontFamily: "monospace" },
    input: (s) => sent.push(s),
    onRender: (callback) => {
      render = callback;
      return { dispose() {} };
    },
  };
  const bridge = (
    macSafari ? installMacSafariNativeInput : installIOSNativeInput
  )(term, host, () => enabled);
  const reached = [];
  for (const type of [
    "keydown",
    "keypress",
    "beforeinput",
    "input",
    "compositionstart",
    "compositionend",
    "paste",
    "touchstart",
    "touchmove",
    "touchend",
    "mousedown",
    "mouseup",
    "click",
    "contextmenu",
  ]) {
    host.addEventListener(type, () => reached.push(type));
  }
  const event = (type, fields = {}) => {
    const e = new Event(type, { cancelable: true });
    Object.defineProperty(e, "target", { value: textarea });
    Object.assign(e, fields);
    host.dispatchEvent(e);
    return e;
  };
  const input = (value, type = "insertText", data = value) => {
    const before = event("beforeinput", {
      inputType: type,
      data,
      isComposing: false,
    });
    assert.equal(before.defaultPrevented, false);
    textarea.value = value;
    textarea.selectionStart = textarea.selectionEnd = value.length;
    event("input", { inputType: type, data, isComposing: false });
  };
  const key = (key, code = 0) =>
    event("keydown", { key, keyCode: code, isComposing: false });
  return {
    bridge,
    event,
    input,
    key,
    textarea,
    sent,
    reached,
    preview: visual(layers()[0]),
    get elements() {
      return layers().map(visual);
    },
    screen,
    term,
    flow: layers()[0].children[0],
    layers,
    wrapped,
    render: () => render(),
    paint: (cells) => {
      painted = cells;
      render();
    },
    disable: () => (enabled = false),
  };
}
test("observed iPhone delete/insert sequence emits no raw jamo and shows native syllables", () => {
  const f = setup();
  for (const [key, value, deleted] of [
    ["ㄹ", "ㄹ"],
    ["ㅏ", "라", ""],
    ["ㄴ", "란", ""],
    ["ㄱ", "란ㄱ"],
    ["ㅡ", "란그", "란"],
    ["ㄹ", "란글", "란"],
  ]) {
    assert.equal(f.key(key).defaultPrevented, false);
    assert.equal(f.event("keypress", { key }).defaultPrevented, false);
    if (deleted !== undefined) f.input(deleted, "deleteContentBackward", null);
    f.input(value);
    f.event("keyup", { key });
    assert.equal(f.preview.textContent, value);
    assert.deepEqual(f.sent, []);
  }
  assert.deepEqual(f.reached, []);
  f.key("Enter", 13);
  assert.deepEqual(f.sent, ["란글"]);
  assert.deepEqual(f.reached, ["keydown"]);
  assert.equal(f.textarea.value, "");
  f.bridge.flush();
  assert.deepEqual(f.sent, ["란글"]);
});
test("native deletion and retyping remain local, including multi-syllable replacement", () => {
  const f = setup();
  f.key("ㅎ");
  f.input("한글");
  for (const value of ["한그", "한ㄱ", "한", "하", "ㅎ", ""]) {
    assert.equal(f.key("Backspace", 8).defaultPrevented, false);
    f.input(value, "deleteContentBackward", null);
    assert.equal(f.preview.textContent, value);
  }
  f.key("ㅎ");
  f.input("ㅎ");
  f.input("", "deleteContentBackward", null);
  f.input("하");
  f.input("", "deleteContentBackward", null);
  f.input("한");
  f.input("", "deleteContentBackward", null);
  f.input("하나");
  f.key(" ", 32);
  assert.deepEqual(f.sent, ["하나"]);
});
test("boundary controls and paste flush once before stock handling", () => {
  for (const boundary of ["Enter", "ArrowLeft", "Tab", "c"]) {
    const f = setup();
    f.key("ㅎ");
    f.input("한");
    f.event("keydown", {
      key: boundary,
      keyCode: 13,
      ctrlKey: boundary === "c",
    });
    assert.deepEqual(f.sent, ["한"]);
    assert.deepEqual(f.reached, ["keydown"]);
  }
  const f = setup();
  f.key("ㅎ");
  f.input("한");
  f.event("paste");
  assert.deepEqual(f.sent, ["한"]);
  assert.deepEqual(f.reached, ["paste"]);
});
test("cancelled/disconnected input cannot leak into another tab", () => {
  const f = setup();
  f.key("ㅎ");
  f.input("한");
  f.disable();
  f.event("blur");
  assert.deepEqual(f.sent, []);
  assert.equal(f.preview.hidden, true);
  f.bridge.cancel();
  f.bridge.dispose();
});
test("ordinary input and standard composition retain stock handlers", () => {
  const f = setup();
  f.key("a", 65);
  f.event("keypress", { key: "a" });
  assert.deepEqual(f.reached, ["keydown", "keypress"]);
  f.event("compositionstart");
  f.key("ㅎ");
  f.input("한");
  f.event("compositionend");
  assert.equal(f.preview.hidden, true);
  assert.deepEqual(f.sent, []);
  assert.ok(f.reached.includes("input"));
});

test("standard composition handoff preserves already mutated native context", () => {
  const f = setup();
  f.key("ㅎ");
  f.input("한");
  f.textarea.value = "한ㄱ";
  f.event("compositionstart");
  assert.equal(f.textarea.value, "한ㄱ");
  assert.deepEqual(f.sent, ["한"]);
  assert.equal(f.preview.hidden, true);
  f.input("한글", "insertCompositionText", "글");
  f.event("compositionend");
  f.input("한글", "insertText", "글");
  assert.deepEqual(f.sent, ["한"]);
  assert.equal(f.preview.hidden, true);
});
test("keydown-less boundaries flush before stock input sees the boundary", () => {
  for (const [inputType, data] of [
    ["insertText", "."],
    ["insertText", "a"],
    ["insertLineBreak", null],
  ]) {
    const f = setup();
    f.key("ㅎ");
    f.input("한");
    const e = f.event("beforeinput", { inputType, data });
    assert.equal(e.defaultPrevented, false);
    assert.deepEqual(f.sent, ["한"]);
    assert.deepEqual(f.reached, ["beforeinput"]);
    f.textarea.value = data ?? "\n";
    f.event("input", { inputType, data });
    assert.deepEqual(f.reached, ["beforeinput", "input"]);
  }
});
test("view release commits to the original live connection before it closes", () => {
  const f = setup();
  f.key("ㅎ");
  f.input("한");
  const tab = {
    generation: 1,
    nativeInput: f.bridge,
    status: "connected",
    ws: {
      close() {
        assert.deepEqual(f.sent, ["한"]);
        f.disable();
      },
    },
  };
  releaseTerminalView(tab);
  assert.deepEqual(f.sent, ["한"]);
  assert.equal(f.preview.hidden, true);
  f.bridge.flush();
  assert.deepEqual(f.sent, ["한"]);
});

test("space retains visual text until a matching echo render, not an unrelated render", () => {
  const f = setup();
  f.key("ㅎ");
  f.input("한글");
  f.key(" ", 32);
  const echo = f.elements[1];
  assert.equal(echo.textContent, "한글");
  assert.equal(echo.removed, false);
  assert.deepEqual(f.sent, ["한글"]);
  f.paint([]);
  assert.equal(echo.removed, false);
  f.paint(["한", null]);
  assert.equal(echo.removed, false);
  f.paint(["한", null, "글", null]);
  assert.equal(echo.removed, true);
  assert.deepEqual(f.sent, ["한글"]);
  f.bridge.dispose();
});
test("visual echo is bounded and cleaned up on cancel or new composition", async () => {
  const f = setup();
  f.key("ㅎ");
  f.input("한");
  f.key(" ", 32);
  const echo = f.elements[1];
  f.bridge.cancel();
  assert.equal(echo.removed, true);
  f.key("ㅎ");
  f.input("한");
  f.key(" ", 32);
  const next = f.elements[2];
  f.key("ㄱ");
  assert.equal(next.removed, true);
  f.input("ㄱ");
  f.key(" ", 32);
  const timed = f.elements[3];
  await new Promise((resolve) => setTimeout(resolve, 750));
  assert.equal(timed.removed, true);
  f.bridge.dispose();
});

test("native editable long press keeps browser defaults and preserves composition", () => {
  const f = setup();
  assert.equal(f.textarea.classList.contains("ios-native-paste-target"), true);
  f.key("ㅎ");
  f.input("한");
  const before = f.textarea.value;
  for (const type of [
    "touchstart",
    "touchmove",
    "touchend",
    "mousedown",
    "mouseup",
    "click",
    "contextmenu",
  ]) {
    const e = f.event(type);
    assert.equal(e.defaultPrevented, false);
    assert.equal(f.textarea.value, before);
    assert.deepEqual(f.sent, []);
    assert.deepEqual(f.reached, []);
  }
  const paste = f.event("paste");
  assert.equal(paste.defaultPrevented, false);
  assert.deepEqual(f.sent, ["한"]);
  assert.ok(f.reached.includes("paste"));
  f.bridge.dispose();
  assert.equal(f.textarea.classList.contains("ios-native-paste-target"), false);
});

test("Mac Safari input before overlapping keydowns retains replacements and resyllabification", () => {
  const f = setup(true);
  const edit = (value, type, data, key) => {
    if (type === "insertReplacementText") {
      f.textarea.selectionStart = f.textarea.value.length - 1;
      f.textarea.selectionEnd = f.textarea.value.length;
    }
    f.input(value, type, data);
    if (key) f.key(key, 229);
    assert.deepEqual(f.sent, []);
    assert.equal(f.preview.textContent, value);
  };
  edit("ㅂ", "insertText", "ㅂ", "ㅂ");
  edit("ㅂ", "insertReplacementText", "ㅂ");
  edit("ㅂㅈ", "insertText", "ㅈ", "ㅈ");
  edit("ㅂㅈㄷ", "insertText", "ㄷ", "ㄷ");
  edit("ㅂㅈ대", "insertReplacementText", "대", "ㅐ");
  edit("ㅂㅈ대ㄱ", "insertText", "ㄱ", "ㄱ");
  edit("ㅂㅈ대겨", "insertReplacementText", "겨", "ㅕ");
  f.event("keydown", { key: "Shift", keyCode: 16, shiftKey: true });
  assert.deepEqual(f.sent, []);
  f.key("Enter", 13);
  assert.deepEqual(f.sent, ["ㅂㅈ대겨"]);
  assert.equal(f.reached.filter((t) => t === "input").length, 0);
  assert.equal(f.textarea.classList.contains("ios-native-paste-target"), false);
  f.bridge.dispose();
  for (const [initial, replacement, next, expected] of [
    ["민", "미", "나", "미나"],
    ["잠", "자", "모", "자모"],
  ]) {
    const g = setup(true);
    g.input(initial);
    g.input(replacement, "insertReplacementText", replacement);
    g.input(expected, "insertText", next);
    g.key(" ", 32);
    assert.deepEqual(g.sent, [expected]);
    g.bridge.dispose();
  }
});

test("Mac Safari replacement backspace and final native deletion never emit jamo", () => {
  const f = setup(true);
  f.input("값");
  for (const value of ["갑", "가", "ㄱ"]) {
    f.input(value, "insertReplacementText", value);
    f.key("Backspace", 229);
  }
  f.input("ㄱ", "insertReplacementText", "ㄱ");
  f.key("Backspace", 8);
  f.input("", "deleteContentBackward", null);
  assert.deepEqual(f.sent, []);
  f.input("ㅎ");
  f.input("하", "insertReplacementText", "하");
  f.input("한", "insertReplacementText", "한");
  f.event("paste");
  assert.deepEqual(f.sent, ["한"]);
  f.bridge.dispose();
});

test("Mac Safari standard composition, desktop gestures and disabled lifecycle stay isolated", () => {
  const f = setup(true);
  f.event("compositionstart");
  f.key("ㅎ", 229);
  f.input("한", "insertCompositionText", "한");
  f.event("compositionend", { data: "한" });
  f.input("한", "insertText", "한");
  assert.equal(f.preview.hidden, true);
  assert.deepEqual(f.sent, []);
  assert.ok(f.reached.includes("input"));
  f.event("mousedown");
  assert.ok(f.reached.includes("mousedown"));
  f.key("ㄱ", 229);
  f.input("한ㄱ", "insertText", "ㄱ");
  f.input("한가", "insertReplacementText", "가");
  f.disable();
  f.event("blur");
  assert.deepEqual(f.sent, []);
  f.bridge.dispose();
});

test("wrapped preview stays inside the viewport and restores cursor ownership", () => {
  for (const mac of [false, true]) {
    const f = setup(mac);
    f.term.buffer.active.cursorX = 39;
    f.term.buffer.active.cursorY = 9;
    f.flow.offsetHeight = 48;
    if (!mac) f.key("ㅎ");
    f.input("한글".repeat(10));
    assert.equal(f.flow.style.textIndent, "380.25px");
    assert.equal(f.flow.style.top, "-32px");
    assert.equal(f.screen.classList.contains("native-input-pending"), true);
    assert.equal(
      f.flow.children.filter((e) => e.className === "native-input-caret")
        .length,
      1,
    );
    if (!mac) {
      assert.ok(
        Math.abs(parseFloat(f.textarea.style["--native-target-width"]) - 9.75) <
          0.001,
      );
      assert.equal(f.textarea.style["--native-target-height"], "16px");
    }
    assert.equal(f.layers()[0].style.top, "144px");
    assert.equal(f.textarea.style.top, "144px");
    f.flow.offsetHeight = 320;
    f.render();
    assert.equal(f.flow.style.top, "-304px");
    assert.deepEqual(f.sent, []);
    // Scrolling away must hide the run instead of clamping it to another row.
    f.term.buffer.active.baseY = 10;
    f.render();
    assert.equal(f.preview.hidden, true);
    assert.equal(f.screen.classList.contains("native-input-pending"), false);
    f.term.buffer.active.viewportY = 10;
    f.render();
    assert.equal(f.preview.hidden, false);
    f.key(" ", 32);
    assert.equal(f.screen.classList.contains("native-input-pending"), false);
    assert.equal(
      f.layers()[1].children[0].children.length,
      1,
      "echo has no local caret",
    );
    assert.deepEqual(f.sent, ["한글".repeat(10)]);
    f.event("compositionstart");
    assert.equal(f.elements[1].removed, true);
    f.bridge.dispose();
    assert.equal(f.elements[0].removed, true);
  }
});

test("empty deletion, blur, cancel and standard composition remove the pending caret", () => {
  for (const boundary of ["delete", "blur", "cancel", "compositionstart"]) {
    const f = setup(true);
    f.input("한");
    assert.equal(f.screen.classList.contains("native-input-pending"), true);
    if (boundary === "delete") f.input("", "deleteContentBackward", null);
    else if (boundary === "cancel") f.bridge.cancel();
    else f.event(boundary);
    assert.equal(f.preview.hidden, true);
    assert.equal(f.screen.classList.contains("native-input-pending"), false);
    assert.deepEqual(
      f.sent,
      ["blur", "compositionstart"].includes(boundary) ? ["한"] : [],
    );
    f.bridge.dispose();
  }
});

test("resize updates textarea geometry without moving standard composition", () => {
  for (const mac of [false, true]) {
    const f = setup(mac);
    f.term.rows = 8;
    f.screen.clientHeight = 128;
    f.term.buffer.active.cursorY = 7;
    f.textarea.style.top = "176px";
    f.render();
    assert.equal(f.textarea.style.top, "112px");
    f.event("compositionstart");
    f.textarea.style.top = "80px";
    f.render();
    assert.equal(f.textarea.style.top, "80px");
    f.bridge.dispose();
  }
});

test("wide glyph wrap padding does not leave an echo remnant", () => {
  const f = setup(true);
  f.term.buffer.active.cursorX = 39;
  f.input("한글");
  f.key(" ", 32);
  f.wrapped.add(1);
  const cells = Array(44).fill("");
  cells.splice(40, 4, "한", null, "글", null);
  f.paint(cells);
  assert.equal(f.elements[1].removed, true);
  assert.deepEqual(f.sent, ["한글"]);
  f.bridge.dispose();
});
