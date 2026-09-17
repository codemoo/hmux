import { createTextFactory } from "./dom.ts";

type SecurityAPI = (
  path: string,
  body?: unknown,
  signal?: AbortSignal,
) => Promise<unknown>;

// Private inputs and responses belong only to this dialog/account lifetime.
export function installAccountSecurity(
  root: HTMLElement,
  api: SecurityAPI,
  onChanged: () => void,
) {
  const controller = new AbortController();
  const doc = root.ownerDocument;
  const make = createTextFactory(doc);
  let enabled: boolean | undefined;
  let busy = false;
  const heading = make("div", "", "security-heading");
  heading.append(make("h3", "계정 보안"));
  const row = make("div", "", "security-toggle-row");
  const label = make("div");
  const state = make("span", "확인 중", "muted");
  label.append(make("strong", "TOTP 로그인"), state);
  const toggle = make("button", "", "security-switch");
  toggle.type = "button";
  toggle.setAttribute("role", "switch");
  toggle.setAttribute("aria-label", "TOTP 로그인");
  toggle.setAttribute("aria-checked", "false");
  toggle.disabled = true;
  toggle.append(make("span"));
  row.append(label, toggle);
  const description = make(
    "p",
    "로그인할 때 인증 앱의 6자리 코드를 추가로 확인합니다.",
    "muted",
  );
  const status = make("p", "", "security-status muted");
  status.setAttribute("role", "status");
  const reload = make("button", "다시 불러오기", "subtle-button");
  reload.type = "button";
  reload.hidden = true;
  const form = make("form", "", "security-confirm");
  form.hidden = true;
  const title = make("strong");
  const field = (name: string, text: string, type: string) => {
    const label = make("label", text);
    const input = make("input");
    input.name = name;
    input.type = type;
    input.required = true;
    label.append(input);
    return { label, input };
  };
  const password = field("password", "현재 비밀번호", "password");
  password.input.autocomplete = "current-password";
  password.input.maxLength = 128;
  const code = field("code", "인증 앱 코드", "text");
  code.input.autocomplete = "one-time-code";
  code.input.inputMode = "numeric";
  code.input.pattern = "[0-9]{6}";
  code.input.maxLength = 6;
  code.input.placeholder = "000000";
  const error = make("p", "", "error");
  error.setAttribute("role", "alert");
  const actions = make("div", "", "dialog-actions");
  const cancel = make("button", "취소", "secondary");
  cancel.type = "button";
  const save = make("button", "", "primary");
  save.type = "submit";
  actions.append(cancel, save);
  form.append(
    title,
    make(
      "p",
      "본인 확인 후 변경됩니다. 이 브라우저는 유지되고, 내 계정의 다른 기기는 로그아웃됩니다.",
      "muted",
    ),
    password.label,
    code.label,
    make(
      "small",
      "기존 인증 앱을 그대로 사용합니다. 이미 사용한 코드는 다음 코드로 바뀐 뒤 입력하세요.",
      "muted",
    ),
    error,
    actions,
  );
  root.append(heading, row, description, form, status, reload);
  const alive = () => !controller.signal.aborted;
  const clear = () => {
    password.input.value = "";
    code.input.value = "";
  };
  const lock = (value: boolean) => {
    busy = value;
    toggle.disabled = value || enabled === undefined;
    for (const control of [password.input, code.input, cancel, save, reload])
      control.disabled = value;
  };
  const render = (value: unknown) => {
    if (
      !value ||
      typeof value !== "object" ||
      typeof (value as { totp_enabled?: unknown }).totp_enabled !== "boolean"
    )
      throw new Error("보안 설정을 확인하지 못했습니다.");
    enabled = (value as { totp_enabled: boolean }).totp_enabled;
    toggle.setAttribute("aria-checked", String(enabled));
    state.textContent = enabled ? "사용 중" : "사용 안 함";
    description.textContent = enabled
      ? "비밀번호와 인증 앱 코드로 로그인합니다."
      : "비밀번호만으로 로그인합니다. 인증 앱 등록은 유지됩니다.";
  };
  async function load() {
    if (busy || !alive()) return;
    lock(true);
    reload.hidden = true;
    status.textContent = "";
    try {
      const value = await api(
        "/api/account/security",
        undefined,
        controller.signal,
      );
      if (!alive()) return;
      render(value);
    } catch (err) {
      if (!alive()) return;
      status.textContent = (err as Error).message;
      reload.hidden = false;
    } finally {
      if (alive()) lock(false);
    }
  }
  toggle.onclick = () => {
    if (busy || enabled === undefined || !alive()) return;
    form.hidden = !form.hidden;
    clear();
    error.textContent = "";
    status.textContent = "";
    title.textContent = enabled ? "TOTP 로그인 끄기" : "TOTP 로그인 켜기";
    save.textContent = enabled ? "끄기" : "켜기";
    // Deliberately avoid autofocus: mobile keyboard opening is user-owned.
  };
  cancel.onclick = () => {
    if (busy) return;
    form.hidden = true;
    error.textContent = "";
    clear();
  };
  form.onsubmit = async (event) => {
    event.preventDefault();
    if (busy || enabled === undefined || !alive()) return;
    const requested = !enabled;
    lock(true);
    error.textContent = "";
    try {
      const result = await api(
        "/api/account/security",
        {
          totp_enabled: requested,
          password: password.input.value,
          code: code.input.value,
        },
        controller.signal,
      );
      if (!alive()) return;
      render(result);
      clear();
      form.hidden = true;
      status.textContent = enabled
        ? "TOTP 로그인을 켰습니다."
        : "TOTP 로그인을 껐습니다.";
      onChanged();
    } catch (err) {
      if (!alive()) return;
      clear();
      error.textContent = (err as Error).message;
      // A timeout can arrive after a committed change. Re-read before retrying.
      try {
        const result = await api(
          "/api/account/security",
          undefined,
          controller.signal,
        );
        if (!alive()) return;
        const previous = enabled;
        render(result);
        if (enabled !== previous) {
          form.hidden = true;
          status.textContent = "현재 보안 설정을 다시 확인했습니다.";
          onChanged();
        }
      } catch {
        /* Keep the original actionable error. */
      }
    } finally {
      if (alive()) lock(false);
    }
  };
  reload.onclick = () => void load();
  void load();
  return () => {
    controller.abort();
    clear();
  };
}
