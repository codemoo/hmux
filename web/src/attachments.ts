import {
  t,
  msg,
  bindText,
  bindAttribute,
  onLocaleChange,
  type TextValue,
} from "./i18n.ts";
import type { Identity } from "./types.ts";
import {
  uploadFiles,
  uploadMetadata,
  type UploadStage,
} from "./file-upload.ts";

type Target = {
  identity: Identity;
  generation: number;
  name: string;
  instance: object;
};
type Options = {
  root: HTMLElement;
  stage: HTMLElement;
  status: HTMLElement;
  picker: HTMLInputElement;
  buttons: HTMLButtonElement[];
  current(): Target | undefined;
  csrf(): string;
  exists(target: Target): boolean;
  insert(target: Target, value: string): boolean;
  error(message: string): void;
};
type Job = {
  target: Target;
  abort: AbortController;
  epoch: number;
  name: string;
  received: number;
  total: number;
  result?: { stage: UploadStage; text: string };
  state: "sending" | "ready" | "inserted" | "error";
  error?: string;
};
const same = (a: Identity, b: Identity) =>
  a.id === b.id && a.created_at === b.created_at;

export function installAttachments(options: Options) {
  const { root, stage, status, picker, buttons } = options;
  let job: Job | undefined,
    disposed = false,
    epoch = 0,
    dragDepth = 0;
  let pickerTarget: Target | undefined;
  let clearTimer: ReturnType<typeof setTimeout> | undefined;
  const abort = () => {
    job?.abort.abort();
    job = undefined;
    clearTimeout(clearTimer);
    render();
  };
  const node = (tag: string, value: TextValue, cls?: string) => {
    const el = document.createElement(tag);
    bindText(el, value);
    if (cls) el.className = cls;
    return el;
  };
  const action = (label: TextValue, callback: () => void) => {
    const b = document.createElement("button");
    b.type = "button";
    bindText(b, label);
    b.onclick = callback;
    return b;
  };
  const currentMatches = (target: Target) => {
    const current = options.current();
    return (
      current &&
      same(current.identity, target.identity) &&
      options.exists(target)
    );
  };
  const insert = (current: Job) => {
    if (!current.result || !currentMatches(current.target)) return;
    if (current.result.stage.expires_at_unix * 1000 <= Date.now()) {
      current.state = "error";
      current.error = t(
        "Attachment expired. Select the files again.",
        "첨부가 만료되었습니다. 파일을 다시 선택해주세요.",
      );
      render();
      return;
    }
    if (!options.insert(current.target, current.result.text)) return;
    current.state = "inserted";
    render();
    clearTimer = setTimeout(() => {
      if (job === current) {
        job = undefined;
        render();
      }
    }, 4000);
  };
  function render() {
    if (disposed) return;
    for (const b of buttons)
      b.disabled =
        !options.current() ||
        job?.state === "sending" ||
        job?.state === "ready";
    status.hidden = !job;
    status.replaceChildren();
    if (!job) return;
    const current = job;
    const title = node("span", current.name, "attachment-name");
    title.title = current.name;
    status.append(title);
    if (current.state === "sending") {
      const progress = document.createElement("progress");
      progress.max = current.total;
      progress.value = current.received;
      bindAttribute(
        progress,
        "aria-label",
        msg("File transfer progress", "파일 전송 진행률"),
      );
      const percent = Math.floor((current.received / current.total) * 100);
      status.append(
        progress,
        node(
          "span",
          percent === 100
            ? msg("Saving to Home…", "Home에 저장 중…")
            : `${percent}%`,
          "attachment-detail",
        ),
        action(msg("Cancel", "취소"), abort),
      );
    } else if (current.state === "ready") {
      status.append(
        node(
          "span",
          () =>
            t(
              `${current.target.name} · deleted after 3 hours`,
              `${current.target.name} · 3시간 후 삭제`,
            ),
          "attachment-detail",
        ),
      );
      const paste = action(msg("Insert path", "경로 넣기"), () =>
        insert(current),
      );
      paste.disabled = !currentMatches(current.target);
      bindAttribute(
        paste,
        "title",
        msg(
          "Connect in the original tab, then select this",
          "원래 탭에서 연결 후 누르세요",
        ),
      );
      status.append(paste, action(msg("Close", "닫기"), abort));
    } else if (current.state === "inserted") {
      status.append(
        node(
          "span",
          msg(
            "Path inserted · deleted after 3 hours",
            "경로 입력 완료 · 3시간 후 삭제",
          ),
          "attachment-detail",
        ),
        action(msg("Close", "닫기"), abort),
      );
    } else {
      status.append(
        node(
          "span",
          current.error ?? msg("Could not attach files", "첨부하지 못했습니다"),
          "attachment-detail attachment-error",
        ),
        action(msg("Close", "닫기"), abort),
      );
    }
  }
  const unsubscribeLocale = onLocaleChange(render);
  const upload = async (files: File[], target: Target) => {
    if (disposed || !files.length) return;
    if (job?.state === "sending" || job?.state === "ready") {
      options.error(
        t(
          "Finish or close the current attachment before attaching more files.",
          "진행 중인 첨부를 완료하거나 닫은 뒤 다시 첨부해주세요.",
        ),
      );
      return;
    }
    try {
      uploadMetadata(files);
    } catch (error) {
      options.error((error as Error).message);
      return;
    }
    if (!options.exists(target)) {
      options.error(
        t(
          "The target tab closed. Select a tab, then attach the files again.",
          "첨부할 탭이 닫혔습니다. 탭을 선택한 뒤 다시 첨부해주세요.",
        ),
      );
      return;
    }
    clearTimeout(clearTimer);
    const current: Job = {
      target,
      abort: new AbortController(),
      epoch,
      name:
        files.length === 1
          ? files[0].name
          : t(
              `${files[0].name} and ${files.length - 1} more`,
              `${files[0].name} 외 ${files.length - 1}개`,
            ),
      received: 0,
      total: files.reduce((sum, f) => sum + f.size, 0),
      state: "sending",
    };
    job = current;
    render();
    try {
      current.result = await uploadFiles(
        files,
        target.identity,
        options.csrf(),
        current.abort.signal,
        (received) => {
          if (disposed || job !== current) return;
          current.received = received;
          const meter = status.querySelector("progress");
          if (meter) meter.value = received;
          const detail = status.querySelector(".attachment-detail");
          if (detail)
            bindText(
              detail as HTMLElement,
              received === current.total
                ? msg("Saving to Home…", "Home에 저장 중…")
                : `${Math.floor((received / current.total) * 100)}%`,
            );
        },
      );
      if (disposed || job !== current || !options.exists(target)) return;
      current.state = "ready";
      const active = options.current();
      if (
        current.epoch === epoch &&
        active?.generation === target.generation &&
        currentMatches(target) &&
        document.visibilityState === "visible" &&
        document.hasFocus()
      )
        insert(current);
      render();
    } catch (error) {
      if (disposed || job !== current) return;
      if (current.abort.signal.aborted) {
        job = undefined;
        render();
        return;
      }
      current.state = "error";
      current.error = (error as Error).message;
      render();
    }
  };
  const choose = () => {
    const target = options.current();
    if (!target) return;
    pickerTarget = { ...target, identity: { ...target.identity } };
    picker.value = "";
    picker.click();
  };
  const change = () => {
    const target = pickerTarget;
    pickerTarget = undefined;
    const files = Array.from(picker.files ?? []);
    picker.value = "";
    if (target) void upload(files, target);
  };
  const isFiles = (event: DragEvent) =>
    Array.from(event.dataTransfer?.types ?? []).includes("Files");
  const resetDrag = () => {
    dragDepth = 0;
    stage.classList.remove("attachment-dragging");
  };
  const enter = (event: DragEvent) => {
    if (!isFiles(event)) return;
    event.preventDefault();
    if (++dragDepth && options.current())
      stage.classList.add("attachment-dragging");
  };
  const over = (event: DragEvent) => {
    if (!isFiles(event)) return;
    event.preventDefault();
    if (event.dataTransfer)
      event.dataTransfer.dropEffect = options.current() ? "copy" : "none";
  };
  const leave = (event: DragEvent) => {
    if (!isFiles(event)) return;
    if (--dragDepth <= 0) resetDrag();
  };
  const drop = (event: DragEvent) => {
    if (!isFiles(event)) return;
    event.preventDefault();
    event.stopPropagation();
    resetDrag();
    const target = options.current();
    if (!target) {
      options.error(
        t(
          "Select a connected terminal tab before dropping files.",
          "연결된 터미널 탭을 선택한 뒤 파일을 놓아주세요.",
        ),
      );
      return;
    }
    const entries = Array.from(event.dataTransfer?.items ?? []);
    if (
      entries.some(
        (item) =>
          item.kind === "file" && item.webkitGetAsEntry?.()?.isDirectory,
      )
    ) {
      options.error(
        t(
          "Select files instead of a folder.",
          "폴더 대신 파일을 선택해주세요.",
        ),
      );
      return;
    }
    void upload(Array.from(event.dataTransfer?.files ?? []), {
      ...target,
      identity: { ...target.identity },
    });
  };
  const invalidate = () => {
    epoch++;
    resetDrag();
  };
  for (const b of buttons) b.addEventListener("click", choose);
  picker.addEventListener("change", change);
  root.addEventListener("dragenter", enter);
  root.addEventListener("dragover", over);
  root.addEventListener("dragleave", leave);
  root.addEventListener("drop", drop);
  window.addEventListener("blur", invalidate);
  document.addEventListener("visibilitychange", invalidate);
  render();
  return {
    refresh: render,
    cancel: abort,
    invalidate,
    close(identity: Identity) {
      if (job && same(job.target.identity, identity)) abort();
    },
    dispose() {
      abort();
      disposed = true;
      unsubscribeLocale();
      pickerTarget = undefined;
      resetDrag();
      for (const b of buttons) b.removeEventListener("click", choose);
      picker.removeEventListener("change", change);
      root.removeEventListener("dragenter", enter);
      root.removeEventListener("dragover", over);
      root.removeEventListener("dragleave", leave);
      root.removeEventListener("drop", drop);
      window.removeEventListener("blur", invalidate);
      document.removeEventListener("visibilitychange", invalidate);
    },
  };
}
