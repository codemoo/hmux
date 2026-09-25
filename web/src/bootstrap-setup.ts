import { t, msg, bindAttribute, bindText, type TextValue } from "./i18n.ts";
import { createTextFactory } from "./dom.ts";
import { withRequestDeadline } from "./request-deadline.ts";

type BootstrapAPI = (
  path: string,
  body: unknown,
  signal: AbortSignal,
) => Promise<unknown>;

type BeginResult =
  | { complete: true }
  | {
      complete: false;
      enrollment_id: string;
      totp_secret: string;
      totp_uri?: string;
    };

const setupFailure = () =>
  t(
    "Could not complete setup. Check the code and try again.",
    "설정을 완료하지 못했습니다. 코드를 확인하고 다시 시도하세요.",
  );

// Only the unauthenticated login view calls this. Older servers retain their
// normal login behavior when the endpoint is absent or temporarily unavailable.
export async function checkBootstrapRequired(request: typeof fetch = fetch) {
  try {
    return await withRequestDeadline(
      async (signal) => {
        const response = await request("/api/setup/status", {
          credentials: "same-origin",
          cache: "no-store",
          signal,
        });
        if (response.status === 404 || !response.ok) return false;
        const value = (await response.json()) as { required?: unknown };
        return value.required === true;
      },
      undefined,
      5_000,
    );
  } catch {
    return false;
  }
}

// Setup credentials are intentionally retained only for this mounted form.
export function installBootstrapSetup(
  root: HTMLElement,
  api: BootstrapAPI,
  onComplete: () => void,
) {
  const controller = new AbortController();
  const make = createTextFactory(root.ownerDocument);
  let busy = false;
  let setupToken = "";
  let enrollmentID = "";
  let secret = "";

  const clearInputs = () => {
    for (const input of root.querySelectorAll("input")) input.value = "";
  };
  const clear = () => {
    clearInputs();
    setupToken = "";
    enrollmentID = "";
    secret = "";
  };
  const alive = () => !controller.signal.aborted && root.isConnected;
  const fail = (error: unknown) =>
    error instanceof Error && error.name === "AbortError" ? "" : setupFailure;
  const lock = (form: HTMLFormElement, value: boolean) => {
    busy = value;
    for (const control of form.querySelectorAll("input, button"))
      (control as HTMLInputElement | HTMLButtonElement).disabled = value;
  };
  const error = make("p", "", "error");
  error.setAttribute("role", "alert");
  const status = make("p", "", "setup-status muted");
  status.setAttribute("role", "status");

  const showBegin = () => {
    if (!alive()) return;
    clear();
    error.textContent = "";
    status.textContent = "";
    root.replaceChildren();
    const heading = make("p", msg("Initial setup", "처음 설정"), "eyebrow");
    const title = make(
      "h2",
      msg("Create administrator account", "관리자 계정 만들기"),
    );
    const help = make(
      "p",
      msg(
        "Create the first account with the server’s one-time setup code.",
        "서버의 일회용 설정 코드로 첫 계정을 만드세요.",
      ),
      "muted",
    );
    const form = make("form");
    const field = (
      name: string,
      labelText: TextValue,
      type: string,
      placeholder: TextValue = "",
    ) => {
      const label = make("label", labelText);
      const input = make("input");
      input.name = name;
      input.type = type;
      input.required = true;
      if (placeholder) bindAttribute(input, "placeholder", placeholder);
      label.append(input);
      return { label, input };
    };
    const token = field("token", msg("Setup code", "설정 코드"), "password");
    token.input.autocomplete = "off";
    token.input.maxLength = 512;
    const username = field(
      "username",
      msg("Account", "계정"),
      "text",
      msg("Account name", "계정 이름"),
    );
    username.input.autocomplete = "username";
    username.input.maxLength = 80;
    const password = field(
      "password",
      msg("Password", "비밀번호"),
      "password",
      msg("At least 8 characters", "8자 이상"),
    );
    password.input.autocomplete = "new-password";
    password.input.maxLength = 128;
    const confirm = field(
      "password_confirm",
      msg("Confirm password", "비밀번호 확인"),
      "password",
      msg("Re-enter password", "비밀번호를 다시 입력"),
    );
    confirm.input.autocomplete = "new-password";
    confirm.input.maxLength = 128;
    const totpLabel = make("label", "", "setup-totp-option");
    const totp = make("input");
    totp.type = "checkbox";
    totp.name = "totp_enabled";
    totp.checked = true;
    totpLabel.append(
      totp,
      make(
        "span",
        msg("Add authenticator verification", "인증 앱으로 추가 확인"),
      ),
      make(
        "small",
        msg(
          "Recommended · Verify a six-digit code in the next step.",
          "권장 · 다음 단계에서 6자리 코드를 확인합니다.",
        ),
      ),
    );
    const submit = make(
      "button",
      msg("Create account", "계정 만들기"),
      "primary",
    );
    submit.type = "submit";
    form.append(
      heading,
      title,
      help,
      token.label,
      username.label,
      password.label,
      confirm.label,
      totpLabel,
      error,
      submit,
      status,
    );
    root.append(form);
    form.onsubmit = async (event) => {
      event.preventDefault();
      if (busy || !alive()) return;
      error.textContent = "";
      status.textContent = "";
      const passwordBytes = new TextEncoder().encode(
        password.input.value,
      ).length;
      if (passwordBytes < 8 || passwordBytes > 128) {
        bindText(
          error,
          msg(
            "Password must be 8–128 bytes.",
            "비밀번호는 8~128바이트여야 합니다.",
          ),
        );
        return;
      }
      if (password.input.value !== confirm.input.value) {
        bindText(
          error,
          msg("Passwords do not match.", "비밀번호가 일치하지 않습니다."),
        );
        return;
      }
      lock(form, true);
      try {
        const result = (await api(
          "/api/setup/begin",
          {
            token: token.input.value,
            username: username.input.value,
            password: password.input.value,
            password_confirm: confirm.input.value,
            totp_enabled: totp.checked,
          },
          controller.signal,
        )) as BeginResult;
        if (!alive()) return;
        if (result?.complete === true) {
          clear();
          onComplete();
          return;
        }
        if (
          !result ||
          result.complete !== false ||
          typeof result.enrollment_id !== "string" ||
          typeof result.totp_secret !== "string" ||
          !result.enrollment_id ||
          !result.totp_secret
        )
          throw new Error("Invalid setup response");
        setupToken = token.input.value;
        enrollmentID = result.enrollment_id;
        secret = result.totp_secret;
        // The URI carries the same secret. No QR dependency is bundled, so only
        // the manual key is rendered for this short-lived enrollment.
        void result.totp_uri;
        password.input.value = "";
        confirm.input.value = "";
        token.input.value = "";
        // The old form disconnects before its finally block can unlock it.
        // Release the shared guard before installing the verification form.
        busy = false;
        showVerification();
      } catch (cause) {
        if (alive()) bindText(error, fail(cause));
      } finally {
        if (alive() && form.isConnected) lock(form, false);
      }
    };
  };

  const showVerification = () => {
    if (!alive()) return;
    root.replaceChildren();
    const form = make("form");
    const heading = make(
      "p",
      msg("Authenticator setup", "인증 앱 등록"),
      "eyebrow",
    );
    const title = make("h2", msg("Verify the code", "인증 코드를 확인하세요"));
    const help = make(
      "p",
      msg(
        "Add the setup key to your authenticator app, then enter its six-digit code.",
        "인증 앱에 설정 키를 추가한 뒤 표시된 6자리 코드를 입력하세요.",
      ),
      "muted",
    );
    const secretLabel = make("label", msg("Setup key", "설정 키"));
    const secretInput = make("input");
    secretInput.value = secret;
    secretInput.readOnly = true;
    secretInput.autocomplete = "off";
    secretInput.spellcheck = false;
    secretInput.className = "setup-secret";
    secretLabel.append(secretInput);
    const codeLabel = make("label", msg("Verification code", "인증 코드"));
    const code = make("input");
    code.name = "code";
    code.required = true;
    code.inputMode = "numeric";
    code.autocomplete = "one-time-code";
    code.pattern = "[0-9]{6}";
    code.maxLength = 6;
    code.placeholder = "000000";
    code.className = "code-input";
    codeLabel.append(code);
    const cancel = make(
      "button",
      msg("Start over", "처음부터 다시"),
      "secondary",
    );
    cancel.type = "button";
    const submit = make(
      "button",
      msg("Verify and continue", "인증 후 계속"),
      "primary",
    );
    submit.type = "submit";
    const actions = make("div", "", "setup-actions");
    actions.append(cancel, submit);
    form.append(
      heading,
      title,
      help,
      secretLabel,
      codeLabel,
      error,
      actions,
      status,
    );
    root.append(form);
    cancel.onclick = () => {
      if (busy) return;
      showBegin();
    };
    form.onsubmit = async (event) => {
      event.preventDefault();
      if (busy || !alive()) return;
      error.textContent = "";
      status.textContent = "";
      lock(form, true);
      try {
        const result = (await api(
          "/api/setup/complete",
          { token: setupToken, enrollment_id: enrollmentID, code: code.value },
          controller.signal,
        )) as { complete?: unknown };
        if (!alive()) return;
        if (result?.complete !== true)
          throw new Error("Invalid setup response");
        clear();
        onComplete();
      } catch (cause) {
        if (alive()) bindText(error, fail(cause));
      } finally {
        if (alive() && form.isConnected) lock(form, false);
      }
    };
  };

  showBegin();
  return () => {
    controller.abort();
    clear();
    root.replaceChildren();
  };
}
