import {
  t,
  msg,
  bindText,
  bindAttribute,
  localeTag,
  type TextValue,
} from "./i18n.ts";
import { createTextFactory } from "./dom.ts";

export type LoginSession = {
  id: string;
  browser: string;
  ip: string;
  location?: string;
  created_at: string;
  last_seen_at: string;
  expires_at: string;
  current: boolean;
};

type SessionAPI = (
  path: string,
  body?: unknown,
  signal?: AbortSignal,
) => Promise<unknown>;

// This panel has its own lifetime: responses from a closed settings dialog or
// previous login must never render into a later user's workspace.
export function installLoginSessions(
  root: HTMLElement,
  api: SessionAPI,
  onCurrentLogout: () => void,
  onCurrentLogoutStart: () => void = () => {},
) {
  const controller = new AbortController();
  const doc = root.ownerDocument;
  const make = createTextFactory(doc);
  let busy = false;
  const heading = make("div", "", "login-sessions-heading");
  const refresh = make("button", msg("Refresh", "새로고침"), "subtle-button");
  refresh.type = "button";
  heading.append(make("h3", msg("Login sessions", "로그인 세션")), refresh);
  const status = make("p", "", "muted");
  status.setAttribute("role", "status");
  const list = make("div", "", "login-sessions-list");
  root.append(
    heading,
    make(
      "p",
      msg(
        "Browsers signed in to your account. You can sign out other devices.",
        "내 계정으로 로그인한 브라우저입니다. 다른 기기의 연결을 로그아웃할 수 있습니다.",
      ),
      "muted",
    ),
    make(
      "p",
      msg(
        "Sign-ins last up to 7 days. Location is estimated from the sign-in IP and may differ with a VPN or carrier.",
        "로그인은 최대 7일 유지됩니다. 위치는 로그인 당시 IP로 추정하며 VPN·통신사에 따라 실제 위치와 다를 수 있습니다.",
      ),
      "muted",
    ),
    status,
    list,
  );
  const alive = () => !controller.signal.aborted;
  const lock = (value: boolean) => {
    busy = value;
    refresh.disabled = value;
    for (const button of list.querySelectorAll("button"))
      button.disabled = value;
  };
  const date = (value: string) => {
    const parsed = new Date(value);
    return Number.isFinite(parsed.getTime())
      ? parsed.toLocaleString(localeTag(), {
          dateStyle: "medium",
          timeStyle: "short",
        })
      : t("Unavailable", "확인 불가");
  };
  async function load() {
    const data = (await api("/api/sessions", undefined, controller.signal)) as {
      sessions: LoginSession[];
    };
    if (!alive()) return;
    list.replaceChildren();
    for (const session of data.sessions) {
      const card = make("article", "", "login-session-card");
      const title = make("div", "", "login-session-title");
      title.append(
        make(
          "strong",
          session.browser || msg("Unknown browser", "알 수 없는 브라우저"),
        ),
      );
      if (session.current)
        title.append(
          make("span", msg("This browser", "이 브라우저"), "usage-active"),
        );
      const details = make("dl", "", "login-session-details");
      for (const [label, value] of [
        [
          msg("Estimated location", "추정 위치"),
          session.location || msg("Location unavailable", "위치 확인 불가"),
        ],
        [
          msg("Access IP", "접속 IP"),
          session.ip || msg("Unavailable", "확인 불가"),
        ],
        [msg("Signed in", "로그인"), () => date(session.created_at)],
        [msg("Last active", "최근 활동"), () => date(session.last_seen_at)],
        [msg("Expires", "만료 예정"), () => date(session.expires_at)],
      ] as [TextValue, TextValue][])
        details.append(make("dt", label), make("dd", value));
      const revoke = make(
        "button",
        session.current
          ? msg("Sign out this browser", "이 브라우저 로그아웃")
          : msg("Sign out", "로그아웃"),
        "subtle-button login-session-revoke",
      );
      revoke.type = "button";
      bindAttribute(revoke, "aria-label", () =>
        t(
          `Sign out ${session.browser} ${session.ip}`,
          `${session.browser} ${session.ip} 로그아웃`,
        ),
      );
      revoke.onclick = async () => {
        if (busy || !alive()) return;
        if (session.current) onCurrentLogoutStart();
        lock(true);
        bindText(status, msg("Signing out…", "로그아웃 처리 중…"));
        try {
          await api(
            "/api/sessions/revoke",
            { id: session.id },
            controller.signal,
          );
          if (!alive()) return;
          if (session.current) {
            onCurrentLogout();
            return;
          }
          card.remove();
          bindText(
            status,
            msg(
              "Signed out that browser.",
              "해당 브라우저를 로그아웃했습니다.",
            ),
          );
          try {
            await load();
            if (alive())
              bindText(
                status,
                msg(
                  "Signed out that browser.",
                  "해당 브라우저를 로그아웃했습니다.",
                ),
              );
          } catch {
            if (alive())
              bindText(
                status,
                msg(
                  "Signed out, but could not refresh the list. Select Refresh.",
                  "로그아웃 완료 · 목록 갱신에 실패했습니다. 새로고침해 주세요.",
                ),
              );
          }
        } catch (error) {
          if (alive()) status.textContent = (error as Error).message;
        } finally {
          if (alive()) lock(false);
        }
      };
      card.append(title, details, revoke);
      list.append(card);
    }
    bindText(status, () =>
      data.sessions.length
        ? t(
            `${data.sessions.length} login sessions`,
            `${data.sessions.length}개의 로그인 세션`,
          )
        : t("No login sessions.", "로그인 세션이 없습니다."),
    );
  }
  async function reload() {
    if (busy || !alive()) return;
    lock(true);
    bindText(status, msg("Checking login sessions…", "로그인 세션 확인 중…"));
    try {
      await load();
    } catch (error) {
      if (alive()) status.textContent = (error as Error).message;
    } finally {
      if (alive()) lock(false);
    }
  }
  refresh.onclick = reload;
  void reload();
  return () => controller.abort();
}
