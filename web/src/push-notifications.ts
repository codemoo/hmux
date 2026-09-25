import { t, msg, bindText, onLocaleChange, type TextValue } from "./i18n.ts";
import type { Identity } from "./types.ts";

type API = (path: string, body?: unknown, signal?: AbortSignal) => Promise<any>;

type PushState = {
  public_key: string;
  login_id: string;
  enabled: boolean;
  endpoint: string;
};

export type PushTarget = {
  session: Identity;
  login_id: string;
};

const sessionIDPattern = /^\$[0-9]{1,12}$/;
const loginIDPattern = /^[A-Za-z0-9_-]{1,128}$/;

function validIdentity(value: unknown): value is Identity {
  if (!value || typeof value !== "object") return false;
  const identity = value as Partial<Identity>;
  return (
    typeof identity.id === "string" &&
    sessionIDPattern.test(identity.id) &&
    Number.isSafeInteger(identity.created_at) &&
    Number(identity.created_at) > 0
  );
}

function validLoginID(value: unknown): value is string {
  return typeof value === "string" && loginIDPattern.test(value);
}

export function pushTargetFromURL(value: string | URL): PushTarget | undefined {
  const url =
    value instanceof URL ? value : new URL(value, "https://hmux.invalid");
  const id = url.searchParams.get("push_session");
  const created = url.searchParams.get("push_created");
  const loginID = url.searchParams.get("push_login");
  if (!id && !created && !loginID) return;
  if (!created || !/^[1-9][0-9]*$/.test(created)) return;
  const session = { id: id || "", created_at: Number(created) };
  if (!validIdentity(session) || !validLoginID(loginID)) return;
  return { session, login_id: loginID };
}

export function pushTargetFromMessage(value: unknown): PushTarget | undefined {
  if (!value || typeof value !== "object") return;
  const message = value as {
    type?: unknown;
    session?: unknown;
    login_id?: unknown;
  };
  if (
    message.type !== "hmux-push-open" ||
    !validIdentity(message.session) ||
    !validLoginID(message.login_id)
  )
    return;
  return { session: { ...message.session }, login_id: message.login_id };
}

export function removePushTargetFromURL() {
  const url = new URL(window.location.href);
  url.searchParams.delete("push_session");
  url.searchParams.delete("push_created");
  url.searchParams.delete("push_login");
  history.replaceState(
    history.state,
    "",
    `${url.pathname}${url.search}${url.hash}`,
  );
}

function applicationServerKey(value: string): Uint8Array<ArrayBuffer> {
  const normalized = value.replace(/-/g, "+").replace(/_/g, "/");
  const padded = normalized.padEnd(Math.ceil(normalized.length / 4) * 4, "=");
  const bytes = atob(padded);
  const result = new Uint8Array(new ArrayBuffer(bytes.length));
  for (let index = 0; index < bytes.length; index++)
    result[index] = bytes.charCodeAt(index);
  return result;
}

const isIOSDevice = () =>
  /iPhone|iPad|iPod/.test(navigator.userAgent) ||
  (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);

const supported = () =>
  "Notification" in window &&
  "serviceWorker" in navigator &&
  "PushManager" in window;

let pushManagerQueue: Promise<void> = Promise.resolve();
function serializePushManager<T>(operation: () => Promise<T>): Promise<T> {
  const result = pushManagerQueue.catch(() => {}).then(operation);
  pushManagerQueue = result.then(
    () => {},
    () => {},
  );
  return result;
}

export function installPushNotifications(
  root: HTMLElement,
  api: API,
  loginID: string,
) {
  const controller = new AbortController();
  let disposed = false;
  let busy = false;
  let state: PushState | undefined;
  let localEndpoint: string | undefined;
  let outcome: (() => string) | undefined;

  const heading = document.createElement("h3");
  bindText(heading, msg("Completion notifications", "완료 알림"));
  const description = document.createElement("p");
  description.className = "muted";
  bindText(
    description,
    msg(
      "When a Codex task finishes, this device receives a notification with the tab name but no conversation content.",
      "Codex 작업이 끝나면 탭 이름만 포함하고 대화 내용은 포함하지 않은 알림을 이 기기에 보냅니다.",
    ),
  );
  const status = document.createElement("p");
  status.className = "push-status";
  status.setAttribute("role", "status");
  const actions = document.createElement("div");
  actions.className = "push-actions";
  const toggle = document.createElement("button");
  toggle.type = "button";
  toggle.className = "secondary";
  const reconnect = document.createElement("button");
  reconnect.type = "button";
  reconnect.className = "secondary";
  bindText(reconnect, msg("Reconnect notifications", "알림 다시 연결"));
  const test = document.createElement("button");
  test.type = "button";
  test.className = "subtle-button";
  bindText(test, msg("Test notification", "테스트 알림"));
  actions.append(toggle, reconnect, test);
  root.append(heading, description, status, actions);

  const permission = () =>
    "Notification" in window ? Notification.permission : "default";
  const render = (message?: TextValue) => {
    if (disposed) return;
    if (message !== undefined)
      outcome =
        message === ""
          ? undefined
          : typeof message === "function"
            ? message
            : () => message;
    const enabled = state?.enabled === true;
    const connected =
      enabled && !!state?.endpoint && localEndpoint === state.endpoint;
    bindText(
      toggle,
      enabled
        ? msg("Turn off notifications", "알림 끄기")
        : msg("Turn on notifications", "알림 켜기"),
    );
    toggle.disabled =
      busy || (!enabled && (!supported() || !state?.public_key));
    reconnect.hidden =
      !enabled || connected || !supported() || !state?.public_key;
    reconnect.disabled = busy || permission() === "denied";
    test.hidden = !enabled;
    test.disabled = busy || permission() !== "granted";
    if (outcome) {
      bindText(status, outcome);
      return;
    }
    if (!state) {
      status.textContent = t(
        "Checking notification status…",
        "알림 상태를 확인하고 있습니다…",
      );
    } else if (!supported()) {
      status.textContent = isIOSDevice()
        ? t(
            "On iPhone or iPad, use Safari’s Share button to add HMux to the Home Screen, then enable notifications in the installed app.",
            "iPhone과 iPad에서는 Safari의 공유 버튼으로 홈 화면에 추가한 뒤, 설치된 HMux 앱에서 켜세요.",
          )
        : t(
            "This browser does not support web push notifications.",
            "이 브라우저는 웹 푸시 알림을 지원하지 않습니다.",
          );
    } else if (!state.public_key) {
      status.textContent = t(
        "Web push notifications are not configured on the server.",
        "서버에 웹 푸시 알림이 아직 설정되지 않았습니다.",
      );
    } else if (permission() === "denied") {
      status.textContent = t(
        "Allow HMux notifications in system or browser settings, then reopen this screen.",
        "운영체제 또는 브라우저 설정에서 HMux 알림 권한을 허용한 뒤 이 화면을 다시 여세요.",
      );
    } else if (enabled && localEndpoint === undefined) {
      status.textContent = t(
        "Checking this browser’s notification connection…",
        "이 브라우저의 알림 연결을 확인하고 있습니다…",
      );
    } else if (enabled && !connected) {
      status.textContent = t(
        "Server notifications are on, but this browser has no subscription. Reconnect.",
        "서버 알림은 켜져 있지만 이 브라우저의 구독이 없습니다. 다시 연결하세요.",
      );
    } else if (enabled) {
      status.textContent = t(
        "Completion notifications are on for this account.",
        "이 계정의 완료 알림이 켜져 있습니다.",
      );
    } else {
      status.textContent = t(
        "Completion notifications are off for this account.",
        "이 계정의 완료 알림이 꺼져 있습니다.",
      );
    }
  };

  const read = async () => {
    try {
      const next = (await api(
        "/api/push",
        undefined,
        controller.signal,
      )) as PushState;
      if (disposed) return;
      if (next.login_id !== loginID)
        throw new Error(
          t(
            "Could not verify notification account information.",
            "알림 계정 정보를 확인하지 못했습니다.",
          ),
        );
      state = next;
      render();
      if (next.enabled && supported() && next.public_key) {
        const endpoint = await serializePushManager(async () => {
          if (disposed) return undefined;
          const registration = await navigator.serviceWorker.ready;
          if (disposed) return undefined;
          const subscription = await registration.pushManager.getSubscription();
          if (disposed) return undefined;
          return subscription?.endpoint || "";
        });
        if (!disposed && endpoint !== undefined) {
          localEndpoint = endpoint;
          render();
        }
      }
    } catch (error) {
      if (!disposed && (error as Error).name !== "AbortError")
        render((error as Error).message);
    }
  };

  const disable = async () => {
    if (busy || !state) return;
    busy = true;
    render("");
    try {
      const serverEndpoint = state.endpoint;
      await serializePushManager(async () => {
        await api("/api/push/unsubscribe", {}, controller.signal);
        if (disposed) return;
        state = { ...state!, enabled: false, endpoint: "" };
        render(
          msg(
            "Turned off completion notifications for this account.",
            "이 계정의 완료 알림을 껐습니다.",
          ),
        );
        if (!supported()) return;
        try {
          const registration = await navigator.serviceWorker.ready;
          const local = await registration.pushManager.getSubscription();
          if (local && local.endpoint === serverEndpoint)
            await local.unsubscribe();
          if (!disposed) localEndpoint = "";
        } catch {
          if (!disposed)
            render(
              msg(
                "Server notifications are off. The browser subscription could not be removed and will be replaced when enabled again.",
                "서버 알림은 껐습니다. 브라우저 구독은 정리하지 못했으며 다시 켤 때 교체됩니다.",
              ),
            );
        }
      });
    } catch (error) {
      if (!disposed && (error as Error).name !== "AbortError")
        render((error as Error).message);
    } finally {
      busy = false;
      render();
    }
  };

  const enable = async () => {
    if (busy || !state || !supported() || !state.public_key) return;
    busy = true;
    render("");
    try {
      // Permission must be requested directly from this user gesture, before
      // waiting behind another panel's serialized PushManager operation.
      let currentPermission = permission();
      if (currentPermission === "default")
        currentPermission = await Notification.requestPermission();
      if (disposed) return;
      if (currentPermission !== "granted") {
        render();
        return;
      }
      await serializePushManager(async () => {
        if (disposed) return;
        const registration = await navigator.serviceWorker.ready;
        if (disposed) return;
        let subscription = await registration.pushManager.getSubscription();
        if (disposed) return;
        const knownOwner =
          !!subscription &&
          state?.enabled &&
          state.login_id === loginID &&
          state.endpoint === subscription.endpoint;
        if (subscription && !knownOwner) {
          await subscription.unsubscribe();
          if (disposed) return;
          subscription = null;
        }
        let created = false;
        if (!subscription) {
          subscription = await registration.pushManager.subscribe({
            userVisibleOnly: true,
            applicationServerKey: applicationServerKey(state!.public_key),
          });
          created = true;
        }
        if (disposed) {
          if (created) await subscription.unsubscribe();
          return;
        }
        try {
          await api(
            "/api/push/subscribe",
            subscription.toJSON(),
            controller.signal,
          );
        } catch (error) {
          if (created) await subscription.unsubscribe();
          throw error;
        }
        if (disposed) return;
        state = { ...state!, enabled: true, endpoint: subscription.endpoint };
        localEndpoint = subscription.endpoint;
        render(
          msg(
            "Turned on completion notifications for this account.",
            "이 계정의 완료 알림을 켰습니다.",
          ),
        );
      });
    } catch (error) {
      if (!disposed && (error as Error).name !== "AbortError")
        render((error as Error).message);
    } finally {
      busy = false;
      render();
    }
  };

  toggle.onclick = () => (state?.enabled ? disable() : enable());
  reconnect.onclick = enable;

  test.onclick = async () => {
    if (busy || !state?.enabled) return;
    busy = true;
    render(
      msg("Sending a test notification…", "테스트 알림을 보내고 있습니다…"),
    );
    try {
      await api("/api/push/test", {}, controller.signal);
      if (!disposed)
        render(msg("Test notification sent.", "테스트 알림을 보냈습니다."));
    } catch (error) {
      if (!disposed && (error as Error).name !== "AbortError")
        render((error as Error).message);
    } finally {
      busy = false;
      render();
    }
  };

  const unsubscribeLocale = onLocaleChange(() => render());
  render();
  void read();
  return () => {
    disposed = true;
    unsubscribeLocale();
    controller.abort();
    toggle.onclick = null;
    reconnect.onclick = null;
    test.onclick = null;
  };
}

type PresenceEnvironment = {
  document: Pick<
    Document,
    "visibilityState" | "hasFocus" | "addEventListener" | "removeEventListener"
  >;
  window: Pick<
    Window,
    "addEventListener" | "removeEventListener" | "setInterval" | "clearInterval"
  >;
  clientID: () => string;
};

export function installPushPresence(
  api: API,
  current: () => Identity | undefined,
  environment: PresenceEnvironment = {
    document,
    window,
    clientID: () => crypto.randomUUID(),
  },
) {
  const clientID = environment.clientID();
  if (!/^[A-Za-z0-9_-]{1,64}$/.test(clientID))
    throw new Error("Invalid push presence client ID");
  let disposed = false;
  let request: AbortController | undefined;
  const send = () => {
    if (disposed) return;
    request?.abort();
    request = new AbortController();
    const active =
      environment.document.visibilityState === "visible" &&
      environment.document.hasFocus();
    const session = active ? current() : undefined;
    void api(
      "/api/push/presence",
      {
        client_id: clientID,
        session: session ? { ...session } : null,
      },
      request.signal,
    ).catch(() => {});
  };
  const interval = environment.window.setInterval(send, 20_000);
  environment.document.addEventListener("visibilitychange", send);
  environment.window.addEventListener("focus", send);
  environment.window.addEventListener("blur", send);
  queueMicrotask(send);
  return {
    refresh: send,
    dispose() {
      disposed = true;
      request?.abort();
      environment.window.clearInterval(interval);
      environment.document.removeEventListener("visibilitychange", send);
      environment.window.removeEventListener("focus", send);
      environment.window.removeEventListener("blur", send);
    },
  };
}
