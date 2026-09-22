import { createTextFactory } from "./dom.ts";

export type ProviderID = "codex" | "claude" | "gemini";
export type ProviderStatus = {
  id: ProviderID;
  label: string;
  installed: boolean;
  version: string;
  auth: "none" | "account" | "api-key";
  key_hint: string;
  profile: boolean;
  profile_id: string;
};
export type ProviderJob = {
  state: "none" | "installing" | "login" | "connected" | "done" | "failed";
  url: string;
  code: string;
  needs_input: boolean;
  log: string[];
};
export type ProviderResult = {
  providers?: ProviderStatus[];
  job?: ProviderJob;
  error?: string;
};

const jobStates = [
  "none",
  "installing",
  "login",
  "connected",
  "done",
  "failed",
];
const loginHosts = [
  "auth.openai.com",
  "claude.ai",
  "claude.com",
  "platform.claude.com",
  "console.anthropic.com",
  "accounts.google.com",
];

// Only https URLs on known login hosts may become an "open login page" button.
export function safeLoginURL(value: unknown) {
  if (typeof value !== "string" || value.length > 4096) return "";
  try {
    const url = new URL(value);
    return url.protocol === "https:" &&
      !url.username &&
      !url.password &&
      loginHosts.includes(url.hostname)
      ? url.href
      : "";
  } catch {
    return "";
  }
}

export function parseProviderJob(value: unknown): ProviderJob | undefined {
  if (!value || typeof value !== "object") return;
  const j = value as Record<string, unknown>;
  if (!jobStates.includes(j.state as string)) return;
  return {
    state: j.state as ProviderJob["state"],
    url: safeLoginURL(j.url),
    code:
      typeof j.code === "string" && /^[A-Z0-9]{4}-[A-Z0-9]{4,5}$/.test(j.code)
        ? j.code
        : "",
    needs_input: j.needs_input === true,
    log: Array.isArray(j.log)
      ? j.log
          .filter((line): line is string => typeof line === "string")
          .slice(-12)
          .map((line) => line.slice(0, 320))
      : [],
  };
}

const providerIDs: ProviderID[] = ["codex", "claude", "gemini"];
const keyLabels: Record<ProviderID, string> = {
  codex: "OpenAI API 키",
  claude: "Anthropic API 키",
  gemini: "Gemini API 키",
};

function text(value: unknown, max: number) {
  return typeof value === "string" && value.length <= max ? value : "";
}

// Home output is untrusted display data; keep only known fields and shapes.
export function parseProviderResult(value: unknown): ProviderResult {
  if (!value || typeof value !== "object")
    throw new Error("AI 연결 상태를 확인하지 못했습니다.");
  const v = value as Record<string, unknown>;
  const result: ProviderResult = {};
  if (typeof v.error === "string" && v.error) result.error = v.error;
  if (Array.isArray(v.providers)) {
    result.providers = [];
    for (const raw of v.providers) {
      const p = raw as Record<string, unknown>;
      if (!p || !providerIDs.includes(p.id as ProviderID)) continue;
      result.providers.push({
        id: p.id as ProviderID,
        label: text(p.label, 64) || String(p.id),
        installed: p.installed === true,
        version: text(p.version, 64),
        auth:
          p.auth === "account" || p.auth === "api-key"
            ? (p.auth as "account" | "api-key")
            : "none",
        key_hint: text(p.key_hint, 8),
        profile: p.profile === true,
        profile_id:
          typeof p.profile_id === "string" &&
          /^[a-z][a-z0-9-]{0,62}$/.test(p.profile_id)
            ? p.profile_id
            : "",
      });
    }
  }
  const job = parseProviderJob(v.job);
  if (job) result.job = job;
  return result;
}

export function providerSummary(p: ProviderStatus) {
  if (!p.installed && p.auth === "none") return "설치되지 않음";
  const parts = [];
  if (!p.installed) parts.push("설치되지 않음");
  else if (p.version) parts.push(`v${p.version}`);
  if (p.auth === "account") parts.push("계정 연결됨");
  else if (p.auth === "api-key")
    parts.push(p.key_hint ? `API 키 ${p.key_hint}` : "API 키 연결됨");
  else parts.push("연결 필요");
  return parts.join(" · ");
}

type API = (
  path: string,
  body?: unknown,
  signal?: AbortSignal,
) => Promise<unknown>;

export function jobMessage(job: ProviderJob, action: "connect" | "update") {
  switch (job.state) {
    case "installing":
      return "설치하는 중입니다. 처음에는 1–2분 걸릴 수 있어요.";
    case "login":
      if (job.needs_input)
        return "로그인 페이지에서 받은 코드를 아래에 붙여넣으세요.";
      if (job.code)
        return "로그인 페이지를 열고 아래 코드를 입력하세요. 완료되면 자동으로 연결됩니다.";
      if (job.url) return "로그인 페이지를 열어 계정으로 로그인하세요.";
      return "로그인을 준비하는 중입니다…";
    case "connected":
    case "done":
      return action === "update" ? "업데이트했습니다." : "연결되었습니다.";
    case "failed":
      return "완료하지 못했습니다. 진행 로그를 확인하세요.";
    default:
      return "";
  }
}

type JobView = {
  root: HTMLElement;
  action: "connect" | "update";
  timer?: number;
  pendingKey?: string;
  update(job: ProviderJob): void;
};

export function installProviderSettings(
  root: HTMLElement,
  api: API,
  startProfile: (profileID: string) => void,
) {
  const controller = new AbortController();
  const doc = root.ownerDocument;
  const make = createTextFactory(doc);
  let busy = false;
  let current: ProviderStatus[] = [];
  const jobs = new Map<ProviderID, JobView>();
  root.append(
    make("h3", "AI 연결"),
    make(
      "p",
      "연결하기를 누르면 Home에 설치하고 계정 로그인까지 이어서 진행합니다. API 키로 연결할 수도 있습니다. 연결 정보는 Home에만 저장되며, 이 Home을 쓰는 모든 웹 계정이 함께 사용합니다.",
      "muted",
    ),
  );
  const list = make("div", "", "provider-list");
  const status = make("p", "", "muted provider-status");
  status.setAttribute("role", "status");
  const reload = make("button", "상태 새로고침", "subtle-button");
  reload.type = "button";
  reload.onclick = () => void load();
  root.append(list, status, reload);

  async function call(operation: string, payload?: unknown) {
    const value = await api(
      "/api/action",
      { operation, payload },
      controller.signal,
    );
    const result = parseProviderResult(value);
    if (result.error) throw new Error(result.error);
    return result;
  }
  function failure(error: unknown, fallback: string) {
    if (!controller.signal.aborted)
      status.textContent = error instanceof Error ? error.message : fallback;
  }
  async function run(
    message: string,
    operation: string,
    payload: unknown,
    done: string,
  ) {
    if (busy || controller.signal.aborted) return;
    busy = true;
    status.textContent = message;
    render();
    try {
      const result = await call(operation, payload);
      if (controller.signal.aborted) return;
      if (result.providers) current = result.providers;
      status.textContent = done;
    } catch (error) {
      failure(error, "요청을 완료하지 못했습니다.");
    } finally {
      busy = false;
      if (!controller.signal.aborted) render();
    }
  }
  function actionButton(label: string, action: () => void, primary = false) {
    const button = make(
      "button",
      label,
      primary ? "secondary" : "subtle-button",
    );
    button.type = "button";
    button.disabled = busy;
    button.onclick = action;
    return button;
  }

  // One persistent panel per running job: polling updates it in place, so a
  // half-typed authorization code survives re-rendering of the row.
  function jobView(p: ProviderStatus, action: "connect" | "update"): JobView {
    const panel = make("div", "", "provider-job");
    panel.setAttribute("role", "status");
    const message = make("p", "", "provider-job-message");
    const open = make("button", "로그인 페이지 열기", "secondary");
    open.type = "button";
    let url = "";
    open.onclick = () => {
      if (url) doc.defaultView?.open(url, "_blank", "noopener,noreferrer");
    };
    const codeRow = make("div", "", "provider-job-code");
    const code = make("strong");
    const copy = make("button", "복사", "subtle-button");
    copy.type = "button";
    copy.onclick = () =>
      void doc.defaultView?.navigator.clipboard
        ?.writeText(code.textContent || "")
        .then(() => (copy.textContent = "복사됨"))
        .catch(() => {});
    codeRow.append(code, copy);
    const form = make("form", "", "provider-job-input");
    const input = make("input");
    input.autocomplete = "off";
    input.spellcheck = false;
    input.placeholder = "코드 붙여넣기";
    input.setAttribute("aria-label", `${p.label} 인증 코드`);
    input.maxLength = 2048;
    const submit = make("button", "확인", "secondary");
    submit.type = "submit";
    form.append(input, submit);
    form.onsubmit = (event) => {
      event.preventDefault();
      const text = input.value.trim();
      if (!text) return;
      input.value = "";
      submit.disabled = true;
      void call("provider-job-input", { provider: p.id, text })
        .then((result) => result.job && view.update(result.job))
        .catch((error) => failure(error, "코드를 전달하지 못했습니다."))
        .finally(() => (submit.disabled = false));
    };
    const cancel = make("button", "취소", "subtle-button");
    cancel.type = "button";
    cancel.onclick = () => {
      stopJob(p.id);
      void call("provider-job-cancel", { provider: p.id }).catch(() => {});
      render();
    };
    const details = make("details", "", "provider-job-log");
    const log = make("pre");
    details.append(make("summary", "진행 로그"), log);
    panel.append(message, open, codeRow, form, cancel, details);
    const view: JobView = {
      root: panel,
      action,
      update(job) {
        message.textContent = jobMessage(job, view.action);
        url = job.url;
        open.hidden = !(job.state === "login" && url);
        code.textContent = job.code;
        codeRow.hidden = !(job.state === "login" && job.code);
        form.hidden = !(job.state === "login" && job.needs_input);
        cancel.hidden = job.state !== "installing" && job.state !== "login";
        log.textContent = job.log.join("\n");
        if (job.state === "failed") details.open = true;
      },
    };
    view.update({
      state: "installing",
      url: "",
      code: "",
      needs_input: false,
      log: [],
    });
    return view;
  }
  function stopJob(id: ProviderID) {
    const view = jobs.get(id);
    if (view?.timer) clearTimeout(view.timer);
    jobs.delete(id);
  }
  function watch(p: ProviderStatus, view: JobView, job: ProviderJob) {
    view.update(job);
    if (job.state === "installing" || job.state === "login") {
      view.timer = doc.defaultView?.setTimeout(async () => {
        if (controller.signal.aborted || jobs.get(p.id) !== view) return;
        try {
          const result = await call("provider-job", { provider: p.id });
          if (controller.signal.aborted || jobs.get(p.id) !== view) return;
          if (result.providers) current = result.providers;
          if (result.job) watch(p, view, result.job);
        } catch (error) {
          failure(error, "진행 상태를 확인하지 못했습니다.");
          watch(p, view, job);
        }
      }, 1000);
      return;
    }
    // Finished: show the result, then save a key that waited for the install.
    jobs.delete(p.id);
    const key = view.pendingKey;
    view.pendingKey = undefined;
    if (job.state !== "failed" && key)
      void run(
        "API 키를 저장하는 중…",
        "provider-key",
        { provider: p.id, key },
        `${p.label} API 키를 저장했습니다.`,
      );
    else {
      status.textContent =
        job.state === "failed"
          ? ""
          : `${p.label}: ${jobMessage(job, view.action)}`;
      render();
      if (job.state === "failed")
        list.querySelector(`[data-provider="${p.id}"]`)?.append(view.root);
    }
  }
  async function startJob(
    p: ProviderStatus,
    action: "connect" | "update",
    pendingKey?: string,
  ) {
    if (jobs.has(p.id) || controller.signal.aborted) return;
    const view = jobView(p, action);
    view.pendingKey = pendingKey;
    jobs.set(p.id, view);
    status.textContent = "";
    render();
    try {
      const result = await call("provider-job-start", {
        provider: p.id,
        action,
      });
      if (controller.signal.aborted || jobs.get(p.id) !== view) return;
      watch(
        p,
        view,
        result.job || {
          state: "installing",
          url: "",
          code: "",
          needs_input: false,
          log: [],
        },
      );
    } catch (error) {
      stopJob(p.id);
      render();
      failure(error, "시작하지 못했습니다.");
    }
  }

  function row(p: ProviderStatus) {
    const item = make("div", "", "provider-row");
    item.dataset.provider = p.id;
    const heading = make("div", "", "provider-heading");
    const running = jobs.get(p.id);
    heading.append(
      make("strong", p.label),
      make("span", running ? "진행 중" : providerSummary(p), "muted"),
    );
    item.append(heading);
    if (running) {
      item.append(running.root);
      return item;
    }
    const actions = make("div", "", "provider-actions");
    if (p.installed && p.auth !== "none" && p.profile_id)
      actions.append(
        actionButton("시작", () => startProfile(p.profile_id), true),
        actionButton("다시 로그인", () => void startJob(p, "connect")),
      );
    else
      actions.append(
        actionButton("연결하기", () => void startJob(p, "connect"), true),
      );
    if (p.installed)
      actions.append(
        actionButton("업데이트", () => void startJob(p, "update")),
      );
    const form = make("form", "", "provider-key");
    const input = make("input");
    input.type = "password";
    input.autocomplete = "off";
    input.spellcheck = false;
    input.placeholder = `또는 ${keyLabels[p.id]} 입력`;
    input.setAttribute("aria-label", `${p.label} ${keyLabels[p.id]}`);
    input.maxLength = 512;
    input.disabled = busy;
    const save = actionButton("키 저장", () => {});
    save.type = "submit";
    form.append(input, save);
    form.onsubmit = (event) => {
      event.preventDefault();
      const key = input.value.trim();
      // Keys are transient: never keep them in the DOM after submission.
      input.value = "";
      if (!key) return;
      // A key is only useful with the CLI installed; install first if needed.
      if (!p.installed) void startJob(p, "update", key);
      else
        void run(
          "API 키를 저장하는 중…",
          "provider-key",
          { provider: p.id, key },
          `${p.label} API 키를 저장했습니다.`,
        );
    };
    if (p.auth === "api-key" && p.key_hint)
      form.append(
        actionButton(
          "키 삭제",
          () =>
            void run(
              "API 키를 삭제하는 중…",
              "provider-key",
              { provider: p.id, key: "" },
              `${p.label} API 키를 삭제했습니다.`,
            ),
        ),
      );
    item.append(actions, form);
    return item;
  }
  function render() {
    list.replaceChildren(...current.map(row));
    reload.disabled = busy;
  }
  async function load() {
    if (busy || controller.signal.aborted) return;
    busy = true;
    status.textContent = "Home에서 상태를 확인하는 중…";
    render();
    try {
      const result = await call("providers");
      if (controller.signal.aborted) return;
      current = result.providers || [];
      status.textContent = "";
    } catch (error) {
      failure(error, "AI 연결 상태를 불러오지 못했습니다.");
    } finally {
      busy = false;
      if (!controller.signal.aborted) render();
    }
    // Reattach to jobs that kept running on Home while Settings was closed.
    for (const p of current) {
      if (jobs.has(p.id) || controller.signal.aborted) continue;
      try {
        const result = await call("provider-job", { provider: p.id });
        const job = result.job;
        if (!job || (job.state !== "installing" && job.state !== "login"))
          continue;
        const view = jobView(p, "connect");
        jobs.set(p.id, view);
        render();
        watch(p, view, job);
      } catch {
        // Status reattachment is best effort.
      }
    }
  }
  void load();
  return () => {
    controller.abort();
    for (const view of jobs.values()) if (view.timer) clearTimeout(view.timer);
  };
}
