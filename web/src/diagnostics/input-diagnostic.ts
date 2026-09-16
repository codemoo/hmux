import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { theme } from "../theme";
import { installIOSNativeInput } from "../ios-native-input";
import "@xterm/xterm/css/xterm.css";
import "./input-diagnostic.css";
import "../ios-native-input.css";

const native = document.querySelector<HTMLTextAreaElement>("#native")!;
const host = document.querySelector<HTMLElement>("#terminal")!;
const events = document.querySelector<HTMLElement>("#events")!;
const wire = document.querySelector<HTMLElement>("#wire")!;
const status = document.querySelector<HTMLElement>("#status")!;
const records: object[] = [];
let recording = true;
let sequence = 0;
let displayPending = false;
const record = (entry: object) => {
  if (!recording) return;
  if (records.length >= 2000) {
    recording = false;
    status.textContent =
      "기록 한도에 도달했습니다. 내려받은 뒤 기록을 지워 다시 시작해주세요.";
    return;
  }
  records.push({ sequence: ++sequence, time: performance.now(), ...entry });
  if (!displayPending) {
    displayPending = true;
    requestAnimationFrame(() => {
      displayPending = false;
      events.textContent = records
        .slice(-16)
        .map((v) => JSON.stringify(v))
        .join("\n");
    });
  }
};
const term = new Terminal({
  theme,
  fontSize: 16,
  cols: 40,
  rows: 6,
  scrollback: 100,
  drawBoldTextInBrightColors: false,
  cursorStyle: "bar",
});
const fit = new FitAddon();
term.loadAddon(fit);
term.open(host);
const textarea = term.textarea!;
const isIOS =
  /iPhone|iPad|iPod/.test(navigator.userAgent) ||
  (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);
const nativeInput = isIOS
  ? installIOSNativeInput(term, host, () => true)
  : undefined;
const snapshot = (input: HTMLTextAreaElement) => ({
  value: input.value.slice(0, 2048),
  start: input.selectionStart,
  end: input.selectionEnd,
});
// Observe before xterm's handlers without changing the default input path.
// No preventDefault, focus changes, DOM input writes or timing delays are added.
for (const type of [
  "keydown",
  "keypress",
  "keyup",
  "beforeinput",
  "input",
  "compositionstart",
  "compositionupdate",
  "compositionend",
  "focus",
  "blur",
]) {
  document.addEventListener(
    type,
    (event) => {
      const input =
        event.target === native
          ? native
          : event.target === textarea
            ? textarea
            : undefined;
      if (!input) return;
      const detail = event as InputEvent & KeyboardEvent & CompositionEvent;
      record({
        kind: "event",
        surface: input === native ? "native" : "terminal",
        type,
        inputType: detail.inputType,
        data: detail.data?.slice(0, 2048),
        key: detail.key,
        keyCode: detail.keyCode,
        charCode: detail.charCode,
        isComposing: detail.isComposing,
        composed: event.composed,
        trusted: event.isTrusted,
        cancelable: event.cancelable,
        before: snapshot(input),
      });
      queueMicrotask(() =>
        record({
          kind: "settled",
          surface: input === native ? "native" : "terminal",
          type,
          prevented: event.defaultPrevented,
          after: snapshot(input),
        }),
      );
      // Use two tasks: native dispatch may run a microtask checkpoint between
      // this capture listener and xterm's listener, which schedules its own timer.
      setTimeout(
        () =>
          setTimeout(() => {
            record({
              kind: "deferred",
              surface: input === native ? "native" : "terminal",
              type,
              after: snapshot(input),
            });
          }, 0),
        0,
      );
    },
    true,
  );
}
const sent: string[] = [];
const emit = (text: string, source: string) => {
  record({ kind: "wire", source, text: text.slice(0, 2048) });
  if (sent.length < 2000) sent.push(text.slice(0, 2048));
  wire.textContent = sent.map((v) => JSON.stringify(v)).join(" ");
  // Synthetic echo only; never a WebSocket or real terminal command.
  if (text === "\x7f") term.write("\b \b");
  else term.write(text.replace(/\r/g, "\r\n"));
};
term.onData((text) => emit(text, "xterm"));
host.addEventListener("click", () => term.focus());
const observer = new ResizeObserver(() => fit.fit());
observer.observe(host);
function report() {
  return JSON.stringify(
    {
      schema: 1,
      baseline: isIOS ? "xterm-6.0.0-ios-native-run-v1" : "xterm-6.0.0-default",
      userAgent: navigator.userAgent,
      viewport: {
        width: innerWidth,
        height: innerHeight,
        visualHeight: visualViewport?.height,
      },
      records,
    },
    null,
    2,
  );
}
const reportField = document.querySelector<HTMLTextAreaElement>("#report")!;
document.querySelector("#show-report")!.addEventListener("click", () => {
  recording = false;
  reportField.hidden = false;
  reportField.value = report();
  reportField.focus({ preventScroll: true });
  reportField.select();
  status.textContent =
    "기록을 멈췄습니다. 선택된 내용을 기본 복사 메뉴로 복사해 대화에 붙여주세요. 테스트 문자열이 포함됩니다.";
});
document.querySelector("#download")!.addEventListener("click", () => {
  const url = URL.createObjectURL(
    new Blob([report()], { type: "application/json" }),
  );
  const link = document.createElement("a");
  link.href = url;
  link.download = "hmux-ios-input-diagnostic.json";
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 30000);
  status.textContent =
    "기록 파일을 내려받았습니다. 파일에는 이 화면에서 입력한 테스트 문자열이 포함됩니다.";
});
document.querySelector("#clear")!.addEventListener("click", () => {
  native.value = "";
  nativeInput?.cancel();
  records.length = 0;
  sent.length = 0;
  sequence = 0;
  recording = true;
  reportField.hidden = true;
  reportField.value = "";
  wire.textContent = events.textContent = "";
  term.reset();
  status.textContent = "기록을 지웠습니다.";
});
