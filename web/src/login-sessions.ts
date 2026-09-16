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
  const make = <K extends keyof HTMLElementTagNameMap>(
    tag: K,
    content: string,
    className = "",
  ) => {
    const node = doc.createElement(tag);
    node.textContent = content;
    node.className = className;
    return node;
  };
  let busy = false;
  const heading = make("div", "", "login-sessions-heading");
  const refresh = make("button", "새로고침", "subtle-button");
  refresh.type = "button";
  heading.append(make("h3", "로그인 세션"), refresh);
  const status = make("p", "", "muted");
  status.setAttribute("role", "status");
  const list = make("div", "", "login-sessions-list");
  root.append(
    heading,
    make(
      "p",
      "내 계정으로 로그인한 브라우저입니다. 다른 기기의 연결을 로그아웃할 수 있습니다.",
      "muted",
    ),
    make(
      "p",
      "로그인은 최대 7일 유지됩니다. 위치는 로그인 당시 IP로 추정하며 VPN·통신사에 따라 실제 위치와 다를 수 있습니다.",
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
      ? parsed.toLocaleString(undefined, {
          dateStyle: "medium",
          timeStyle: "short",
        })
      : "확인 불가";
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
      title.append(make("strong", session.browser || "알 수 없는 브라우저"));
      if (session.current)
        title.append(make("span", "이 브라우저", "usage-active"));
      const details = make("dl", "", "login-session-details");
      for (const [label, value] of [
        ["추정 위치", session.location || "위치 확인 불가"],
        ["접속 IP", session.ip || "확인 불가"],
        ["로그인", date(session.created_at)],
        ["최근 활동", date(session.last_seen_at)],
        ["만료 예정", date(session.expires_at)],
      ])
        details.append(make("dt", label), make("dd", value));
      const revoke = make(
        "button",
        session.current ? "이 브라우저 로그아웃" : "로그아웃",
        "subtle-button login-session-revoke",
      );
      revoke.type = "button";
      revoke.setAttribute(
        "aria-label",
        `${session.browser} ${session.ip} 로그아웃`,
      );
      revoke.onclick = async () => {
        if (busy || !alive()) return;
        if (session.current) onCurrentLogoutStart();
        lock(true);
        status.textContent = "로그아웃 처리 중…";
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
          status.textContent = "해당 브라우저를 로그아웃했습니다.";
          try {
            await load();
            if (alive())
              status.textContent = "해당 브라우저를 로그아웃했습니다.";
          } catch {
            if (alive())
              status.textContent =
                "로그아웃 완료 · 목록 갱신에 실패했습니다. 새로고침해 주세요.";
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
    status.textContent = data.sessions.length
      ? `${data.sessions.length}개의 로그인 세션`
      : "로그인 세션이 없습니다.";
  }
  async function reload() {
    if (busy || !alive()) return;
    lock(true);
    status.textContent = "로그인 세션 확인 중…";
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
