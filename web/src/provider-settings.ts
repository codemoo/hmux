import { t, msg, bindText, bindAttribute, type TextValue } from "./i18n.ts";
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
const keyLabels: Record<ProviderID, () => string> = {
  codex: msg("OpenAI API key", "OpenAI API 키"),
  claude: msg("Anthropic API key", "Anthropic API 키"),
  gemini: msg("Gemini API key", "Gemini API 키"),
};

function text(value: unknown, max: number) {
  return typeof value === "string" && value.length <= max ? value : "";
}

// Home output is untrusted display data; keep only known fields and shapes.
export function parseProviderResult(value: unknown): ProviderResult {
  if (!value || typeof value !== "object")
    throw new Error(
      t(
        "Could not verify AI connection status.",
        "AI 연결 상태를 확인하지 못했습니다.",
      ),
    );
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

// Connected providers that already have a launch profile.
export function startableProviders(providers: ProviderStatus[]) {
  return providers.filter(
    (p) => p.installed && p.auth !== "none" && p.profile_id,
  );
}

// Setup remains useful until an installed, authenticated CLI has a launch profile.
export function needsProviderSetup(providers: ProviderStatus[]) {
  return startableProviders(providers).length === 0;
}

export function providerSummary(p: ProviderStatus) {
  if (!p.installed) return t("CLI not installed", "CLI 설치 안 됨");
  if (p.auth === "account") return t("CLI account login", "CLI 계정 로그인");
  if (p.auth === "api-key") return t("API key configured", "API 키 설정됨");
  return t("Sign-in needed", "로그인 필요");
}

function providerGuidance(p: ProviderStatus) {
  if (!p.installed)
    return p.auth !== "none"
      ? t(
          "Credentials were found on Home. Install the CLI to use them.",
          "Home에 인증 정보가 있습니다. CLI를 설치하면 사용할 수 있습니다.",
        )
      : t(
          "Install the CLI on Home, then choose how to sign in.",
          "Home에 CLI를 설치하고 사용할 계정으로 로그인하세요.",
        );
  if (p.auth === "account")
    return t(
      "Uses the CLI account already signed in on Home. No API key is needed.",
      "Home의 CLI에 로그인된 계정을 그대로 사용합니다. API 키는 필요하지 않습니다.",
    );
  if (p.auth === "api-key")
    return t(
      "An API key is configured on Home. API usage may be billed separately from a subscription.",
      "Home에 API 키가 설정되어 있습니다. API 사용 요금은 구독과 별도로 청구될 수 있습니다.",
    );
  return t(
    "Sign in to the CLI on Home. If you already signed in there, refresh status first.",
    "Home의 CLI에 로그인하세요. 이미 로그인했다면 먼저 상태를 새로고침하세요.",
  );
}

type API = (
  path: string,
  body?: unknown,
  signal?: AbortSignal,
) => Promise<unknown>;

export function jobMessage(job: ProviderJob, action: "connect" | "update") {
  switch (job.state) {
    case "installing":
      return t(
        "Installing. The first time may take 1–2 minutes.",
        "설치하는 중입니다. 처음에는 1–2분 걸릴 수 있어요.",
      );
    case "login":
      if (job.needs_input)
        return t(
          "Paste the code from the sign-in page below.",
          "로그인 페이지에서 받은 코드를 아래에 붙여넣으세요.",
        );
      if (job.code)
        return t(
          "Open the sign-in page and enter the code below. Connection follows automatically.",
          "로그인 페이지를 열고 아래 코드를 입력하세요. 완료되면 자동으로 연결됩니다.",
        );
      if (job.url)
        return t(
          "Open the sign-in page and sign in to your account.",
          "로그인 페이지를 열어 계정으로 로그인하세요.",
        );
      return t("Preparing sign-in…", "로그인을 준비하는 중입니다…");
    case "connected":
    case "done":
      return action === "update"
        ? t("Updated.", "업데이트했습니다.")
        : t("Connected.", "연결되었습니다.");
    case "failed":
      return t(
        "Could not finish. Check the progress log.",
        "완료하지 못했습니다. 진행 로그를 확인하세요.",
      );
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
    make("h3", msg("AI tools on Home", "Home의 AI 도구")),
    make(
      "p",
      msg(
        "HMux runs the CLIs installed on Home. Existing CLI logins work here without adding an API key.",
        "HMux는 Home에 설치된 CLI를 실행합니다. 이미 CLI에 로그인했다면 API 키를 추가할 필요가 없습니다.",
      ),
      "muted",
    ),
  );
  root.append(
    make(
      "p",
      msg(
        "Shared on Home · Authentication changes apply to everyone using this Home.",
        "Home 공용 인증 · 변경 사항은 이 Home을 사용하는 모든 웹 계정에 적용됩니다.",
      ),
      "provider-scope",
    ),
  );
  const list = make("div", "", "provider-list");
  const status = make("p", "", "muted provider-status");
  status.setAttribute("role", "status");
  const reload = make(
    "button",
    msg("Refresh status", "상태 새로고침"),
    "subtle-button",
  );
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
  function failure(error: unknown, fallback: TextValue) {
    if (!controller.signal.aborted)
      bindText(status, error instanceof Error ? error.message : fallback);
  }
  async function run(
    message: TextValue,
    operation: string,
    payload: unknown,
    done: TextValue,
  ) {
    if (busy || controller.signal.aborted) return;
    busy = true;
    bindText(status, message);
    render();
    try {
      const result = await call(operation, payload);
      if (controller.signal.aborted) return;
      if (result.providers) current = result.providers;
      bindText(status, done);
    } catch (error) {
      failure(
        error,
        msg("Could not complete the request.", "요청을 완료하지 못했습니다."),
      );
    } finally {
      busy = false;
      if (!controller.signal.aborted) render();
    }
  }
  function actionButton(label: TextValue, action: () => void, primary = false) {
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
    const open = make(
      "button",
      msg("Open sign-in page", "로그인 페이지 열기"),
      "secondary",
    );
    open.type = "button";
    let url = "";
    open.onclick = () => {
      if (url) doc.defaultView?.open(url, "_blank", "noopener,noreferrer");
    };
    const codeRow = make("div", "", "provider-job-code");
    const code = make("strong");
    const copy = make("button", msg("Copy", "복사"), "subtle-button");
    copy.type = "button";
    copy.onclick = () =>
      void doc.defaultView?.navigator.clipboard
        ?.writeText(code.textContent || "")
        .then(() => bindText(copy, msg("Copied", "복사됨")))
        .catch(() => {});
    codeRow.append(code, copy);
    const form = make("form", "", "provider-job-input");
    const input = make("input");
    input.autocomplete = "off";
    input.spellcheck = false;
    bindAttribute(input, "placeholder", msg("Paste code", "코드 붙여넣기"));
    bindAttribute(input, "aria-label", () =>
      t(`${p.label} verification code`, `${p.label} 인증 코드`),
    );
    input.maxLength = 2048;
    const submit = make("button", msg("Confirm", "확인"), "secondary");
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
        .catch((error) =>
          failure(
            error,
            msg("Could not submit code.", "코드를 전달하지 못했습니다."),
          ),
        )
        .finally(() => (submit.disabled = false));
    };
    const cancel = make("button", msg("Cancel", "취소"), "subtle-button");
    cancel.type = "button";
    cancel.onclick = () => {
      stopJob(p.id);
      void call("provider-job-cancel", { provider: p.id }).catch(() => {});
      render();
    };
    const details = make("details", "", "provider-job-log");
    const log = make("pre");
    details.append(make("summary", msg("Progress log", "진행 로그")), log);
    panel.append(message, open, codeRow, form, cancel, details);
    const view: JobView = {
      root: panel,
      action,
      update(job) {
        bindText(message, () => jobMessage(job, view.action));
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
    if (view) view.pendingKey = undefined;
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
          failure(
            error,
            msg(
              "Could not check progress.",
              "진행 상태를 확인하지 못했습니다.",
            ),
          );
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
        msg("Saving API key…", "API 키를 저장하는 중…"),
        "provider-key",
        { provider: p.id, key },
        () =>
          t(`${p.label} API key saved.`, `${p.label} API 키를 저장했습니다.`),
      );
    else {
      bindText(
        status,
        job.state === "failed"
          ? ""
          : () => `${p.label}: ${jobMessage(job, view.action)}`,
      );
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
    if (busy || jobs.has(p.id) || controller.signal.aborted) return;
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
      if (result.providers) current = result.providers;
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
      failure(error, msg("Could not start.", "시작하지 못했습니다."));
    }
  }

  function row(p: ProviderStatus) {
    const item = make("div", "", "provider-row");
    item.dataset.provider = p.id;
    item.setAttribute("role", "group");
    item.setAttribute("aria-label", p.label);
    const heading = make("div", "", "provider-heading");
    const running = jobs.get(p.id);
    const name = make("div", "", "provider-name");
    name.append(make("strong", p.label));
    if (p.installed && p.version)
      name.append(make("span", `CLI v${p.version}`, "provider-version"));
    heading.append(
      name,
      make(
        "span",
        () => (running ? t("In progress", "진행 중") : providerSummary(p)),
        "provider-auth-badge",
      ),
    );
    item.append(heading);
    if (running) {
      item.append(running.root);
      return item;
    }
    item.append(make("p", () => providerGuidance(p), "provider-guidance"));
    const authenticated = p.auth !== "none";
    if (authenticated) {
      const actions = make("div", "", "provider-actions");
      if (!p.installed) {
        actions.append(
          actionButton(
            msg("Install CLI", "CLI 설치"),
            () => void startJob(p, "update"),
            true,
          ),
        );
      } else if (p.profile_id) {
        actions.append(
          actionButton(
            msg("Start new session", "새 세션 시작"),
            () => startProfile(p.profile_id),
            true,
          ),
        );
      } else {
        actions.append(
          actionButton(
            msg("Add to session menu", "세션 메뉴에 추가"),
            () =>
              void run(
                msg("Adding the existing CLI…", "기존 CLI를 추가하는 중…"),
                "provider-job-start",
                { provider: p.id, action: "use-existing" },
                msg(
                  "Ready to start a new session. Your authentication settings were kept.",
                  "새 세션을 시작할 수 있습니다. 기존 인증 설정은 그대로 유지했습니다.",
                ),
              ),
            true,
          ),
        );
        actions.append(
          make(
            "span",
            msg(
              "Keeps your current authentication.",
              "현재 인증 설정을 그대로 유지합니다.",
            ),
            "muted",
          ),
        );
      }
      item.append(actions);
    }

    const methods = make("div", "", "provider-methods");
    if (authenticated) {
      const change = make("details", "", "provider-auth-options");
      change.append(
        make(
          "summary",
          p.auth === "api-key"
            ? msg("Manage authentication", "인증 설정 관리")
            : msg("Change sign-in method", "인증 방식 변경"),
        ),
        methods,
      );
      item.append(change);
    } else item.append(methods);

    const account = make("section", "", "provider-method");
    account.append(make("h4", msg("CLI account login", "CLI 계정 로그인")));
    if (p.auth === "api-key") {
      account.append(
        make(
          "p",
          p.key_hint
            ? msg(
                "To use an account login, remove the saved API key below first. Then check the CLI sign-in status.",
                "계정 로그인으로 사용하려면 아래에서 저장된 API 키를 먼저 제거하세요. 그다음 CLI 로그인 상태를 확인합니다.",
              )
            : msg(
                "This key is reported by the CLI. Manage it on Home, then refresh status to use an account login.",
                "CLI에서 감지한 키입니다. Home에서 인증 설정을 변경한 뒤 상태를 새로고침하세요.",
              ),
        ),
      );
    } else {
      account.append(
        make(
          "p",
          msg(
            "Continue with the provider’s account sign-in on Home.",
            "Home에서 제공업체의 계정 로그인 절차를 진행합니다.",
          ),
        ),
      );
      account.append(
        actionButton(
          p.auth === "account"
            ? msg("Sign in to CLI again", "CLI 다시 로그인")
            : p.installed
              ? msg("Sign in to CLI", "CLI 로그인")
              : msg("Install CLI and sign in", "CLI 설치 후 로그인"),
          () => void startJob(p, "connect"),
          !authenticated,
        ),
      );
    }
    const keyPanel = make("details", "", "provider-key-panel");
    keyPanel.open = p.auth === "api-key";
    keyPanel.append(
      make(
        "summary",
        p.auth === "api-key"
          ? msg("API key", "API 키")
          : msg("Use an API key instead", "API 키로 사용하기"),
      ),
    );
    keyPanel.append(
      make(
        "p",
        msg(
          "Optional · For API billing. Saving changes this CLI’s authentication on Home; it is not a separate HMux account.",
          "선택 사항 · API 과금 방식입니다. 저장하면 Home의 CLI 인증 설정이 바뀌며, HMux 전용 계정이 추가되는 것은 아닙니다.",
        ),
      ),
    );
    if (p.auth === "api-key" && p.key_hint)
      keyPanel.append(
        make(
          "p",
          () => t(`Saved key: ${p.key_hint}`, `저장된 키: ${p.key_hint}`),
          "provider-key-hint",
        ),
      );
    const form = make("form", "", "provider-key");
    const label = make("label", () => keyLabels[p.id]());
    const input = make("input");
    input.type = "password";
    input.autocomplete = "off";
    input.spellcheck = false;
    bindAttribute(
      input,
      "placeholder",
      p.auth === "api-key"
        ? msg("Enter a replacement key", "교체할 키 입력")
        : msg("Enter API key", "API 키 입력"),
    );
    bindAttribute(input, "aria-label", () => `${p.label} ${keyLabels[p.id]()}`);
    input.maxLength = 512;
    input.disabled = busy;
    label.append(input);
    const save = actionButton(
      p.auth === "api-key"
        ? msg("Replace API key", "API 키 교체")
        : p.installed
          ? msg("Use API key", "API 키로 전환")
          : msg("Install CLI and use key", "CLI 설치 후 키 사용"),
      () => {},
    );
    save.type = "submit";
    form.append(label, save);
    form.onsubmit = (event) => {
      event.preventDefault();
      if (busy || controller.signal.aborted) return;
      const key = input.value.trim();
      input.value = "";
      if (!key) return;
      if (!p.installed) void startJob(p, "update", key);
      else
        void run(
          msg("Saving API key…", "API 키를 저장하는 중…"),
          "provider-key",
          { provider: p.id, key },
          () =>
            t(
              `${p.label} API key saved on Home.`,
              `${p.label} API 키를 Home에 저장했습니다.`,
            ),
        );
    };
    keyPanel.append(form);
    if (p.auth === "api-key" && p.key_hint) {
      keyPanel.append(
        actionButton(
          msg("Remove saved API key", "저장된 API 키 제거"),
          () =>
            void run(
              msg("Removing API key…", "API 키를 제거하는 중…"),
              "provider-key",
              { provider: p.id, key: "" },
              msg(
                "Saved API key removed. Check the detected sign-in method above.",
                "저장된 API 키를 제거했습니다. 위에 표시된 인증 상태를 확인하세요.",
              ),
            ),
        ),
      );
    }
    if (p.auth === "api-key") methods.append(keyPanel, account);
    else methods.append(account, keyPanel);
    if (p.installed)
      methods.append(
        actionButton(
          msg("Update CLI", "CLI 업데이트"),
          () => void startJob(p, "update"),
        ),
      );
    return item;
  }
  function render() {
    list.replaceChildren(...current.map(row));
    reload.disabled = busy;
  }
  async function load() {
    if (busy || controller.signal.aborted) return;
    busy = true;
    bindText(
      status,
      msg("Checking status on Home…", "Home에서 상태를 확인하는 중…"),
    );
    render();
    try {
      const result = await call("providers");
      if (controller.signal.aborted) return;
      current = result.providers || [];
      status.textContent = "";
    } catch (error) {
      failure(
        error,
        msg(
          "Could not load AI connection status.",
          "AI 연결 상태를 불러오지 못했습니다.",
        ),
      );
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
    for (const view of jobs.values()) {
      if (view.timer) clearTimeout(view.timer);
      view.pendingKey = undefined;
    }
    jobs.clear();
    for (const input of root.querySelectorAll("input")) input.value = "";
  };
}
