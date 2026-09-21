import {
  createDiagnostics,
  diagnosticRoute,
  diagnosticReason,
} from "./diagnostics";
import { installDiagnosticSettings } from "./diagnostics-settings";
import {
  defaultUsagePreferences,
  parseUsagePreferences,
  installUsagePreferences,
  type UsagePreferences,
} from "./usage-preferences";
import { installAttachments } from "./attachments";
import { createSessionAPI } from "./session-api";
import {
  createConnectionRecovery,
  disconnectKind,
  retryDelay,
  type DisconnectKind,
} from "./connection-recovery";
import { installNativeClipboard, hasNativeSelection } from "./native-clipboard";
import { installAndroidNativePaste } from "./android-native-paste";
import {
  installIOSNativeInput,
  installMacSafariNativeInput,
} from "./ios-native-input";
import { releaseTerminalView, terminalSize } from "./terminal-session";
import { createTerminalHeartbeat } from "./terminal-heartbeat";
import { createTerminalOutput } from "./terminal-output";
import { createViewportController } from "./viewport";
import { createTerminalFonts } from "./fonts";
import { installButton } from "./pwa";
import { installTerminalScroll } from "./terminal-scroll";
import { preferredFontSize } from "./mobile";
import { installDesktopTerminal, isTerminalCopy } from "./desktop-terminal";
import { installMobileTerminalLinks } from "./mobile-terminal-links";
import { installAccountSecurity } from "./account-security";
import { installLoginSessions } from "./login-sessions";
import {
  installPushNotifications,
  installPushPresence,
  pushTargetFromMessage,
  pushTargetFromURL,
  removePushTargetFromURL,
  type PushTarget,
} from "./push-notifications";
import { workspaceShortcut } from "./shortcuts";
import { createPreferences } from "./preferences";
import {
  validateWorkspace,
  type SharedWorkspace,
  type WorkspaceChange,
} from "./shared-workspace";
import { createTextFactory, iconButton } from "./dom";
import { icon } from "./icons";
import { renderUsageFooter, renderUsagePanel } from "./usage-view";
import { renderConversation, type Conversation } from "./conversation-view";
import { theme } from "./theme";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import "./style.css";
import "./ios-native-input.css";
import "./chrome.css";
import "./dialogs.css";

import type { Identity, Session, Snapshot, Tab } from "./types";
const $ = <T extends HTMLElement = HTMLElement>(selector: string) =>
  document.querySelector<T>(selector)!;
const app = $("#app");
const mobileScreen = window.matchMedia(
  "(max-width: 700px), (pointer: coarse) and (max-height: 500px)",
);
const isAndroid = /Android/i.test(navigator.userAgent);
document.documentElement.classList.toggle("android", isAndroid);
const terminalFonts = createTerminalFonts(isAndroid);
const isIOS =
  /iPad|iPhone|iPod/.test(navigator.userAgent) ||
  (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);
const isMacSafari =
  !isIOS &&
  /Macintosh/.test(navigator.userAgent) &&
  /Version\/.*Safari\//.test(navigator.userAgent) &&
  !/Chrome|Chromium|Edg|OPR/.test(navigator.userAgent);
const fontPreferenceKey = () =>
  mobileScreen.matches ? "hmux.font.mobile.compact" : "hmux.font";
const preferences = createPreferences(() => window.localStorage);
const mark = '<img class="brandmark" src="/icons/hmux-192.png" alt="">';
let readerAbort: AbortController | undefined;
let layoutObserver: ResizeObserver | undefined;
let layoutFrame = 0;
let layoutSettledTimers: number[] = [];
function fitActiveTerminal() {
  const tab = tabs.get(active);
  if (!tab || reading || !tab.host.isConnected) return;
  const bounds = tab.host.getBoundingClientRect();
  if (bounds.width < 2 || bounds.height < 2) return;
  tab.fit.fit();
}
function scheduleTerminalLayout() {
  cancelAnimationFrame(layoutFrame);
  for (const timer of layoutSettledTimers) clearTimeout(timer);
  layoutFrame = requestAnimationFrame(() => {
    resizeMobileViewport();
    fitActiveTerminal();
  });
  // Android may report intermediate keyboard geometry without a final resize.
  // Re-read live dimensions throughout the transition; never reuse a tab height.
  layoutSettledTimers = [80, 200, 450, 800].map((delay) =>
    window.setTimeout(() => {
      resizeMobileViewport();
      fitActiveTerminal();
    }, delay),
  );
}
let refreshRequest: AbortController | undefined;
let accountEpoch = 0;
let usagePreferences = defaultUsagePreferences();
function applyUsagePreferences(value: UsagePreferences) {
  if (value.revision < usagePreferences.revision) return;
  usagePreferences = value;
  renderFooter();
}
let startRequest: object | undefined;
let startTimer: number | undefined;
let startFailures = 0;
let bootstrapping = false;
let sharedLoaded = false,
  applyingShared = false,
  workspaceDirty = false,
  workspaceEdit = 0,
  workspaceRevision = 0;
let workspaceBase: Identity[] = [];
let pendingChange: WorkspaceChange | undefined;
let pendingEdit = 0;
let workspaceEpoch = 0;
let workspaceRequest: AbortController | undefined;
let lastWorkspaceTabs = "[]";
let workspaceStorageKey = "hmux.tabs";
let pendingWorkspace: { tabs?: Identity[]; active?: Identity } | undefined;
let attachments: ReturnType<typeof installAttachments> | undefined;
let pushPresence: ReturnType<typeof installPushPresence> | undefined;
const initialURL = new URL(window.location.href);
let pendingPushTarget: PushTarget | undefined = pushTargetFromURL(initialURL);
if (
  ["push_session", "push_created", "push_login"].some((name) =>
    initialURL.searchParams.has(name),
  )
)
  removePushTargetFromURL();
let dialogCleanup: (() => void) | undefined;
let refreshUsageDialog: (() => void) | undefined;
let loggingOut = false;
let csrf = "",
  loginID = "",
  loggedIn = false,
  sessions: Session[] = [],
  snapshot: Snapshot = { online: false },
  active = "",
  reading = false,
  readEpoch = 0,
  pollTimer: number | undefined,
  fontSize = preferredFontSize(
    mobileScreen.matches,
    preferences.get(fontPreferenceKey()),
  ),
  ctrl = false;
const tabs = new Map<string, Tab>();
const collator = new Intl.Collator("en", {
  numeric: true,
  sensitivity: "base",
});
const key = (s: Identity) => `${s.id}:${s.created_at}`;
const label = (s: Session) => s.alias || s.name;
const text = createTextFactory(document);
const button = (title: string, name: string, action: () => void) =>
  iconButton(document, title, name, action);
function notice(message: string) {
  const n = $("#notice");
  if (n) {
    n.textContent = message;
    n.hidden = !message;
  }
}
const diagnostics = createDiagnostics({
  csrf: () => csrf,
  build: import.meta.url,
  storage: () => sessionStorage,
  context: () => ({
    online: navigator.onLine,
    visible: document.visibilityState === "visible",
    standalone:
      window.matchMedia("(display-mode: standalone)").matches ||
      (navigator as Navigator & { standalone?: boolean }).standalone === true,
  }),
});
window.addEventListener("error", (event) =>
  diagnostics.record("runtime-error", {
    reason: diagnosticReason(event.error),
    line: event.lineno,
    column: event.colno,
  }),
);
window.addEventListener("unhandledrejection", (event) =>
  diagnostics.record("unhandled-rejection", {
    reason: diagnosticReason(event.reason),
  }),
);
const sessionAPI = createSessionAPI({
  csrf: () => csrf,
  unauthorized: showLogin,
  failure: (event) =>
    diagnostics.record("api-failed", {
      route: diagnosticRoute(event.path),
      reason: event.reason,
      code: event.status,
      duration_ms: event.durationMs,
    }),
});
const api = sessionAPI.request;
function reportError(error: unknown) {
  if ((error as Error).name !== "AbortError") notice((error as Error).message);
}
async function action(
  operation: string,
  session?: Identity,
  payload?: unknown,
  signal?: AbortSignal,
) {
  return api(
    "/api/action",
    {
      operation,
      session: session
        ? { id: session.id, created_at: session.created_at }
        : undefined,
      payload,
    },
    signal,
  );
}
function disposeAll() {
  usagePreferences = defaultUsagePreferences();
  diagnostics.dispose();
  accountEpoch++;
  sessionAPI.reset();
  refreshRequest?.abort();
  refreshRequest = undefined;
  startRequest = undefined;
  clearTimeout(startTimer);
  startFailures = 0;
  attachments?.dispose();
  attachments = undefined;
  pushPresence?.dispose();
  pushPresence = undefined;
  dialogCleanup?.();
  dialogCleanup = undefined;
  workspaceEpoch++;
  workspaceRequest?.abort();
  workspaceRequest = undefined;
  sharedLoaded = false;
  workspaceDirty = false;
  pendingChange = undefined;
  workspaceBase = [];
  workspaceRevision = 0;
  layoutObserver?.disconnect();
  readerAbort?.abort();
  for (const t of tabs.values()) {
    releaseTerminalView(t);
    t.nativeInput?.dispose();
    t.disposeNativePaste?.();
    t.interaction?.dispose();
    t.term.dispose();
    t.host.remove();
  }
  tabs.clear();
  active = "";
  readEpoch++;
  clearTimeout(pollTimer);
}
function resolvePushTarget() {
  if (!pendingPushTarget || !loggedIn || !loginID || !sharedLoaded) return;
  if (pendingPushTarget.login_id !== loginID) return;
  const target = pendingPushTarget;
  const session = sessions.find(
    (value) =>
      value.id === target.session.id &&
      value.created_at === target.session.created_at,
  );
  if (!session) return;
  pendingPushTarget = undefined;
  openSession(session);
}
function showLogin() {
  loggingOut = false;
  loggedIn = false;
  disposeAll();
  csrf = "";
  loginID = "";
  sessions = [];
  snapshot = { online: false };
  app.innerHTML = `<main class="login"><div class="login-story"><a class="brand" href="/">${mark}<span>HMux</span></a><div class="story-copy"><span class="eyebrow">YOUR PERSONAL WORKSPACE</span><h1>작업은 그대로.<br><span>어디서든 이어서.</span></h1><p>Home에서 이어지는 터미널과 AI 작업.<br>익숙한 공간으로 돌아오세요.</p><div class="story-terminal"><div><i></i><i></i><i></i><span>home / workspace</span></div><p><b>❯</b> tmux attach</p><p class="muted">Your work is right where you left it.<span class="cursor">▍</span></p></div></div><p class="story-foot">ONE HOME. EVERY SCREEN.</p></div><section class="login-panel"><form id="login-form"><div class="lock-badge">${icon("lock")}</div><span class="eyebrow">WELCOME BACK</span><h2>내 작업 공간에 로그인</h2><p class="muted">계정으로 내 작업 공간에 접속하세요.</p><label>계정<input name="username" autocomplete="username" required maxlength="80" autofocus placeholder="계정 이름"></label><label>비밀번호<input name="password" type="password" autocomplete="current-password" required maxlength="128" placeholder="비밀번호"></label><label id="login-totp" hidden>인증 코드<input name="code" class="code-input" inputmode="numeric" autocomplete="one-time-code" pattern="[0-9]{6}" maxlength="6" placeholder="000000"><small>Google Authenticator의 6자리 코드</small></label><p id="login-error" class="error" role="alert"></p><button class="primary" type="submit">작업 이어가기 ${icon("arrow")}</button><p class="login-note">개인 전용 공간 · 공개 회원가입 없음</p></form></section></main>`;
  $("#login-form").append(installButton());
  const loginForm = $<HTMLFormElement>("#login-form");
  const codeInput = loginForm.elements.namedItem("code") as HTMLInputElement;
  const resetChallenge = () => {
    $("#login-totp").hidden = true;
    codeInput.required = false;
    codeInput.value = "";
  };
  for (const name of ["username", "password"]) {
    (loginForm.elements.namedItem(name) as HTMLInputElement).oninput =
      resetChallenge;
  }
  $("#login-form").onsubmit = async (e) => {
    e.preventDefault();
    const form = e.currentTarget as HTMLFormElement;
    const b = form.querySelector("button")!;
    b.disabled = true;
    $("#login-error").textContent = "";
    const f = new FormData(form);
    const fields = [...form.querySelectorAll("input")];
    for (const field of fields) field.disabled = true;
    try {
      const result = (await api("/api/login", Object.fromEntries(f))) as {
        ok?: boolean;
        totp_required?: boolean;
      };
      if (!form.isConnected) return;
      if (result.totp_required === true) {
        $("#login-totp").hidden = false;
        codeInput.required = true;
        for (const field of fields) field.disabled = false;
        codeInput.focus();
        b.disabled = false;
        return;
      }
      if (result.ok !== true)
        throw new Error("로그인 응답을 확인하지 못했습니다.");
      form.reset();
      await start();
    } catch (err) {
      if (!form.isConnected) return;
      $("#login-error").textContent = (err as Error).message;
      b.disabled = false;
    } finally {
      if (form.isConnected) {
        for (const field of fields) field.disabled = false;
      }
    }
  };
}
function shell() {
  app.innerHTML = `<div class="workspace"><aside id="session-sidebar" class="sidebar"><div class="sidebar-head"><a class="brand" href="/">${mark}<span>HMux</span><small>WORKSPACE</small></a><button id="sidebar-close" class="icon-button" title="목록 닫기" aria-label="목록 닫기">${icon("close")}</button></div><div class="home-card"><span class="status-dot" id="home-dot"></span><div><strong>Home</strong><small id="home-state">연결 확인 중</small></div></div><div class="search-box">${icon("search")}<input id="search" type="search" placeholder="세션 검색" aria-label="세션 검색"><kbd>⌘ K</kbd></div><div class="list-heading"><span>세션 <b id="count">0</b></span><button id="new-session" class="icon-button" title="새 세션" aria-label="새 세션">${icon("plus")}</button></div><div id="session-list" class="session-list"></div><label class="hidden-toggle"><input id="show-hidden" type="checkbox"> 숨긴 세션 표시</label><div class="sidebar-bottom"><span id="username"></span><button id="logout" class="icon-button" title="로그아웃 (Alt+Q)" aria-label="로그아웃">${icon("logout")}</button></div></aside><div id="scrim"></div><main class="workarea"><button id="floating-tabs" class="icon-button" aria-label="탭 목록 펼치기" aria-expanded="false" aria-controls="tabs">${icon("menu")}</button><header class="tabbar"><button id="menu" class="icon-button" title="사이드바 전환 (Alt+L / Alt+&#96;)" aria-label="사이드바 전환" aria-controls="session-sidebar">${icon("menu")}</button><div id="tabs" role="tablist" aria-label="열린 세션"></div><div class="toolbar"><button id="terminal-refresh" class="icon-button" title="터미널 화면 새로고침" aria-label="터미널 화면 새로고침" disabled>${icon("refresh")}</button><button id="attach" class="icon-button" title="파일 첨부" aria-label="파일 첨부">${icon("attach")}</button><button id="tab-new" class="icon-button" title="새 세션" aria-label="새 세션">${icon("plus")}</button><button id="conversation" class="icon-button" title="대화 읽기" aria-label="대화 읽기" aria-pressed="false">${icon("book")}</button><button id="settings" class="icon-button" title="터미널 설정" aria-label="터미널 설정">${icon("settings")}</button></div></header><div id="notice" class="notice" role="status" hidden></div><div id="attachment-status" class="attachment-status" role="status" hidden></div><input id="attachment-picker" type="file" multiple hidden><div id="stage"><div id="empty"><div class="empty-mark">${icon("terminal")}</div><span class="eyebrow">READY WHEN YOU ARE</span><h2>이어서 할 작업을 선택하세요</h2><p>세션을 열면 Home의 작업에 연결됩니다.<br>이 화면을 닫아도 작업은 계속됩니다.</p><button id="browse" class="secondary">세션 둘러보기 ${icon("arrow")}</button></div><div id="reader" hidden></div></div><div class="keybar" aria-label="터미널 보조 키"><button id="terminal-refresh-mobile" aria-label="터미널 화면 새로고침" title="터미널 화면 새로고침" disabled>${icon("refresh")}</button><button id="attach-mobile" aria-label="파일 첨부" title="파일 첨부">${icon("attach")}</button><button data-key="\u001b">Esc</button><button data-key="\t">Tab</button><button id="ctrl" aria-pressed="false">Ctrl</button><button data-key="\u0003">Ctrl C</button><button data-key="\u001b[A">↑</button><button data-key="\u001b[B">↓</button><button data-key="\u001b[D">←</button><button data-key="\u001b[C">→</button></div><footer><span id="bedl" class="bedl" aria-hidden="true"></span><button id="usage" class="footer-button">Claude <span>—</span><i></i> Codex <span>—</span></button><span id="metrics">Home · 사용량 대기 중</span><span id="footer-connection" class="footer-connection" title="공용 탭 연결 중"><span id="terminal-state" role="status"></span><button id="reconnect" class="subtle-button" hidden>${icon("refresh")} 다시 연결</button></span></footer></main></div><dialog id="dialog" aria-labelledby="dialog-title"><div class="dialog-head"><h2 id="dialog-title"></h2><button id="dialog-close" class="icon-button" aria-label="닫기">${icon("close")}</button></div><div id="dialog-body"></div></dialog>`;
  app.classList.toggle(
    "sidebar-collapsed",
    preferences.get("hmux.sidebar") === "hidden",
  );
  updateSidebarAccessibility();
  $("#menu").onclick = () => setSidebar(!sidebarVisible());
  $("#sidebar-close").onclick = $("#scrim").onclick = () => setSidebar(false);
  $("#browse").onclick = () => {
    setSidebar(true);
    $("#search").focus();
  };
  $("#floating-tabs").onpointerdown = (e) => e.preventDefault();
  $("#floating-tabs").onclick = () => {
    const expanded = app.classList.toggle("floating-tabs-open");
    $("#floating-tabs").setAttribute("aria-expanded", String(expanded));
    $("#floating-tabs").setAttribute(
      "aria-label",
      expanded ? "탭 목록 접기" : "탭 목록 펼치기",
    );
  };
  $("#search").oninput = renderSessions;
  $("#show-hidden").onchange = renderSessions;
  $("#new-session").onclick = $("#tab-new").onclick = () => void createDialog();
  $("#settings").onclick = settingsDialog;
  attachments = installAttachments({
    root: $(".workarea"),
    stage: $("#stage"),
    status: $("#attachment-status"),
    picker: $("#attachment-picker"),
    buttons: [$("#attach"), $("#attach-mobile")],
    csrf: () => csrf,
    current: () => {
      const t = tabs.get(active);
      if (
        !loggedIn ||
        loggingOut ||
        !t ||
        reading ||
        $("#dialog").hasAttribute("open") ||
        t.status !== "connected" ||
        t.ws?.readyState !== WebSocket.OPEN
      )
        return;
      return {
        identity: t.identity,
        instance: t,
        generation: t.generation,
        name:
          sessions.find((s) => key(s) === active)?.alias ||
          sessions.find((s) => key(s) === active)?.name ||
          "원래 탭",
      };
    },
    exists: (target) => tabs.get(key(target.identity)) === target.instance,
    insert: (target, value) => {
      const t = tabs.get(key(target.identity));
      if (
        !loggedIn ||
        !t ||
        active !== key(target.identity) ||
        reading ||
        $("#dialog").hasAttribute("open") ||
        t.status !== "connected" ||
        t.ws?.readyState !== WebSocket.OPEN ||
        t.ws.bufferedAmount > 256 << 10
      )
        return false;
      t.nativeInput?.flush();
      ctrl = false;
      $("#ctrl").setAttribute("aria-pressed", "false");
      t.term.paste(value);
      return true;
    },
    error: notice,
  });
  $("#conversation").onclick = () => void toggleReader();
  $("#usage").onclick = usageDialog;
  $("#logout").onclick = async () => {
    if (loggingOut) return;
    loggingOut = true;
    attachments?.cancel();
    try {
      await api("/api/logout", {});
      preferences.remove(workspaceStorageKey);
      showLogin();
    } catch (e) {
      reportError(e);
    } finally {
      loggingOut = false;
      attachments?.refresh();
    }
  };
  $("#dialog-close").onclick = () => $("#dialog").closest("dialog")!.close();
  $("#dialog").addEventListener("close", () => {
    dialogCleanup?.();
    dialogCleanup = undefined;
    attachments?.refresh();
    scheduleTerminalLayout();
    if (isAndroid) {
      $("#settings").focus({ preventScroll: true });
      resizeMobileViewport();
      requestAnimationFrame(resizeMobileViewport);
      return;
    }
    if (!reading) {
      const t = tabs.get(active);
      if (t) focusTerminal(t);
    }
  });
  for (const id of ["#terminal-refresh", "#terminal-refresh-mobile"]) {
    $(id).onpointerdown = (e) => e.preventDefault();
    $(id).onclick = () => {
      const t = tabs.get(active);
      if (
        !loggedIn ||
        loggingOut ||
        !snapshot.online ||
        reading ||
        $("#dialog").hasAttribute("open") ||
        !t ||
        t.status !== "connected"
      )
        return;
      if (t.ws?.readyState !== WebSocket.OPEN) return;
      t.fit.fit();
      t.term.refresh(0, t.term.rows - 1);
      // Always resync the PTY size, even when FitAddon sees no local change.
      t.ws.send(
        JSON.stringify({
          type: "resize",
          ...terminalSize(t.term.cols, t.term.rows),
        }),
      );
      t.ws.send(JSON.stringify({ type: "refresh" }));
    };
  }
  $("#reconnect").onclick = () => {
    const t = tabs.get(active);
    if (t) connect(t, true);
  };
  for (const b of document.querySelectorAll<HTMLButtonElement>("[data-key]")) {
    b.onpointerdown = (e) => e.preventDefault();
    b.onclick = () => {
      send(b.dataset.key!);
    };
  }
  $("#ctrl").onpointerdown = (e) => e.preventDefault();
  $("#ctrl").onclick = () => {
    tabs.get(active)?.nativeInput?.flush();
    ctrl = !ctrl;
    $("#ctrl").setAttribute("aria-pressed", String(ctrl));
    {
      const t = tabs.get(active);
      if (t) focusTerminal(t);
    }
  };
  const observer = new ResizeObserver(scheduleTerminalLayout);
  observer.observe($("#stage"));
  observer.observe(app);
  layoutObserver = observer;
}
function renderSessions() {
  const query = $<HTMLInputElement>("#search").value.toLocaleLowerCase();
  const hidden = $<HTMLInputElement>("#show-hidden").checked;
  const list = $("#session-list");
  const filtered = sessions
    .filter(
      (s) =>
        (hidden || !s.hidden) &&
        [label(s), s.name, s.runtime, s.current_path]
          .join(" ")
          .toLocaleLowerCase()
          .includes(query),
    )
    .sort(
      (a, b) =>
        collator.compare(label(a), label(b)) ||
        collator.compare(a.name, b.name) ||
        key(a).localeCompare(key(b)),
    );
  $("#count").textContent = String(filtered.length);
  const nodes = new Map(
    Array.from(list.children).map((n) => [
      (n as HTMLElement).dataset.key,
      n as HTMLElement,
    ]),
  );
  let position = 0;
  for (const s of filtered) {
    const k = key(s);
    let row = nodes.get(k);
    if (!row) {
      row = document.createElement("div");
      row.className = "session-row";
      row.dataset.key = k;
      const open = document.createElement("button");
      open.className = "session-open";
      open.append(
        text("span", "", "session-icon"),
        text("span", "", "session-info"),
      );
      open.onclick = () => {
        const current = sessions.find((v) => key(v) === k);
        if (current) openSession(current);
      };
      row.append(
        open,
        button("세션 관리", "settings", () => {
          const current = sessions.find((v) => key(v) === k);
          if (current) editDialog(current);
        }),
      );
    }
    row.dataset.runtime = s.runtime || "shell";
    row.classList.toggle("selected", k === active);
    row.classList.toggle("is-hidden", !!s.hidden);
    const open = row.firstElementChild as HTMLButtonElement;
    open.disabled = !snapshot.online;
    open.setAttribute("aria-current", String(k === active));
    row.querySelector(".session-icon")!.textContent =
      s.runtime === "codex" ? "›_" : s.runtime === "claude" ? "✳" : "⌁";
    const info = row.querySelector(".session-info")!;
    info.replaceChildren(
      text("strong", label(s)),
      text(
        "small",
        [
          s.runtime || "shell",
          s.current_path?.split("/").filter(Boolean).at(-1) ||
            `${s.window_count}개 창`,
          s.hidden ? "숨김" : "",
        ]
          .filter(Boolean)
          .join(" · "),
      ),
    );
    open.title = s.current_path || s.name;
    if (list.children[position] !== row)
      list.insertBefore(row, list.children[position] || null);
    position++;
    nodes.delete(k);
  }
  for (const n of nodes.values()) n.remove();
  if (!filtered.length)
    list.append(
      text(
        "p",
        query
          ? "일치하는 세션이 없습니다."
          : snapshot.online
            ? "아직 세션이 없습니다. +로 시작하세요."
            : "Home 연결을 기다리고 있습니다.",
        "list-empty",
      ),
    );
}
function persistTabs() {
  const serialized = JSON.stringify([...tabs.values()].map((t) => t.identity));
  if (sharedLoaded && !applyingShared && serialized !== lastWorkspaceTabs) {
    lastWorkspaceTabs = serialized;
    workspaceDirty = true;
    workspaceEdit++;
    void syncSharedWorkspace();
  }
  if (bootstrapping) return;
  preferences.set(
    workspaceStorageKey,
    JSON.stringify({
      tabs: [...tabs.values()].map((t) => t.identity),
      active: tabs.get(active)?.identity,
    }),
  );
}
function renderTabs() {
  const list = $("#tabs");
  const nodes = new Map(
    Array.from(list.children).map((n) => [
      (n as HTMLElement).dataset.key,
      n as HTMLElement,
    ]),
  );
  let position = 0;
  for (const [k, t] of tabs) {
    const session = sessions.find((s) => key(s) === k);
    let box = nodes.get(k);
    if (!box) {
      box = document.createElement("div");
      box.dataset.key = k;
      box.draggable = true;
      box.ondragstart = (event) =>
        event.dataTransfer?.setData("text/hmux-tab", k);
      box.ondragover = (event) => {
        if (event.dataTransfer?.types.includes("text/hmux-tab"))
          event.preventDefault();
      };
      box.ondrop = (event) => {
        const source = event.dataTransfer?.getData("text/hmux-tab");
        if (!source || !tabs.has(source)) return;
        event.preventDefault();
        moveTab(source, k);
      };
      const choose = document.createElement("button");
      choose.role = "tab";
      choose.onpointerdown = (e) => {
        if (document.documentElement.classList.contains("keyboard-visible"))
          e.preventDefault();
      };
      choose.onclick = () => selectTab(k);
      choose.title = "드래그로 순서 변경 · Alt+Shift+좌우로 탭 전환";
      box.append(
        choose,
        button("탭 닫기 (Alt+W)", "close", () => closeTab(k)),
      );
    }
    box.className = "tab" + (k === active ? " active" : "");
    box.dataset.status = k === active ? t.status : "idle";
    const choose = box.firstElementChild as HTMLButtonElement;
    choose.setAttribute("aria-selected", String(k === active));
    choose.textContent = session ? label(session) : "세션 없음";
    choose.title = `${choose.textContent} · 드래그로 순서 변경`;
    if (list.children[position] !== box)
      list.insertBefore(box, list.children[position] || null);
    position++;
    nodes.delete(k);
  }
  for (const node of nodes.values()) node.remove();
  const t = tabs.get(active);
  $("#footer-connection").dataset.status = t?.status || "idle";
  $("#terminal-state").textContent = t
    ? {
        connected: "연결됨",
        connecting: "연결 중…",
        disconnected: "연결 끊김",
      }[t.status]
    : "";
  $("#terminal-state").title = t?.recovery.description() || "";
  $("#terminal-state").setAttribute(
    "aria-label",
    $("#terminal-state").textContent || "세션 없음",
  );
  $("#reconnect").setAttribute("aria-label", "터미널 다시 연결");
  $("#reconnect").hidden =
    !t || t.status !== "disconnected" || !snapshot.online;
  $("#empty").hidden = tabs.size > 0;
  $<HTMLButtonElement>("#conversation").disabled = !t;
  for (const id of ["#terminal-refresh", "#terminal-refresh-mobile"]) {
    $<HTMLButtonElement>(id).disabled =
      !t || reading || !snapshot.online || t.status !== "connected";
  }
  attachments?.refresh();
}
function moveTab(source: string, target: string) {
  const order = [...tabs.keys()];
  const from = order.indexOf(source),
    to = order.indexOf(target);
  if (from < 0 || to < 0 || from === to) return;
  order.splice(from, 1);
  order.splice(to, 0, source);
  const entries = order.map((k) => [k, tabs.get(k)!] as const);
  tabs.clear();
  for (const [k, t] of entries) tabs.set(k, t);
  renderTabs();
  persistTabs();
}
function closeTab(k: string) {
  const t = tabs.get(k);
  if (!t) return;
  attachments?.close(t.identity);
  releaseTerminalView(t);
  t.nativeInput?.dispose();
  t.disposeNativePaste?.();
  t.interaction?.dispose();
  t.term.dispose();
  t.host.remove();
  tabs.delete(k);
  if (active === k) {
    active = "";
    selectTab([...tabs.keys()].at(-1) || "");
  }
  renderTabs();
  renderSessions();
  persistTabs();
}
function selectTab(k: string) {
  const changed = active !== k || reading;
  attachments?.invalidate();
  tabs.get(active)?.interaction?.hide();
  tabs.get(active)?.nativeInput?.flush();
  scheduleTerminalLayout();
  app.classList.remove("floating-tabs-open");
  document
    .querySelector("#floating-tabs")
    ?.setAttribute("aria-expanded", "false");
  readerAbort?.abort();
  active = k;
  ctrl = false;
  $("#ctrl").setAttribute("aria-pressed", "false");
  reading = false;
  readEpoch++;
  $("#reader").hidden = true;
  $("#reader").replaceChildren();
  $("#conversation").setAttribute("aria-pressed", "false");
  for (const [id, t] of tabs) {
    // Hidden mobile input must not remain the keyboard's scroll anchor.
    if (
      (isAndroid || isIOS) &&
      id !== k &&
      document.activeElement === t.term.textarea
    )
      t.term.blur();
    if (isAndroid) t.host.inert = id !== k;
    t.host.style.visibility = id === k ? "visible" : "hidden";
    t.term.options.disableStdin = id !== k;
    // A browser keeps one live view; hidden tabs must not exhaust the shared
    // gateway's connection budget for other devices. Original tmux survives.
    if (id !== k && t.ws) {
      releaseTerminalView(t);
    }
  }
  const t = tabs.get(k);
  if (changed) t?.recovery.resume();
  if (t)
    requestAnimationFrame(() => {
      if (active === k) {
        t.fit.fit();
        if (
          t.status === "disconnected" &&
          snapshot.online &&
          sessions.some((s) => key(s) === k)
        )
          connect(t);
        if (!isAndroid) focusTerminal(t);
      }
    });
  renderTabs();
  // Scroll only the horizontal tab strip; scrollIntoView can pan the mobile viewport.
  const strip = $("#tabs");
  const selected = strip.querySelector<HTMLElement>(".tab.active");
  if (selected) {
    const row = strip.getBoundingClientRect();
    const bounds = selected.getBoundingClientRect();
    if (bounds.left < row.left) strip.scrollLeft -= row.left - bounds.left;
    else if (bounds.right > row.right)
      strip.scrollLeft += bounds.right - row.right;
  }
  renderSessions();
  persistTabs();
  if (mobileSidebar.matches) setSidebar(false, false);
  notice("");
  pushPresence?.refresh();
}

function openSession(s: Session, activate = true) {
  if (!sharedLoaded && !applyingShared) {
    notice(
      "공용 탭을 불러온 뒤 세션을 열 수 있습니다. Home 연결을 확인해주세요.",
    );
    return;
  }
  const k = key(s);
  if (tabs.has(k)) {
    selectTab(k);
    return;
  }
  if (tabs.size >= 32) {
    notice("최대 32개 탭을 열 수 있습니다. 사용하지 않는 탭을 닫아주세요.");
    return;
  }
  const host = document.createElement("div");
  host.className = "terminal-host";
  $("#stage").append(host);
  const term = new Terminal({
    theme,
    fontFamily: terminalFonts.family(),
    fontSize,
    scrollback: 5000,
    drawBoldTextInBrightColors: false,
    cursorBlink: true,
    cursorStyle: "bar",
    cursorInactiveStyle: "bar",
    cursorWidth: 1,
    allowProposedApi: false,
    allowTransparency: false,
    convertEol: false,
    screenReaderMode: false,
  });
  const fit = new FitAddon();
  term.attachCustomKeyEventHandler((event) => {
    if (isTerminalCopy(term, event)) return false;
    return !handleWorkspaceShortcut(event);
  });
  term.loadAddon(fit);
  term.open(host);
  const t: Tab = {
    identity: { id: s.id, created_at: s.created_at },
    term,
    fit,
    host,
    status: "disconnected",
    generation: 0,
    recovery: createConnectionRecovery(),
    output: createTerminalOutput(
      (bytes, done) => term.write(bytes, done),
      () => {
        if (t.status === "disconnected") scheduleReconnect(t);
      },
    ),
  };
  tabs.set(k, t);
  if (isIOS || isAndroid)
    t.interaction = installMobileTerminalLinks(
      term,
      host,
      () => loggedIn && active === k && !reading,
    );
  else
    t.interaction = installDesktopTerminal(
      term,
      host,
      () => loggedIn && active === k && !reading,
      () => t.nativeInput?.flush(),
    );
  if (isIOS || isMacSafari)
    t.nativeInput = (
      isIOS ? installIOSNativeInput : installMacSafariNativeInput
    )(
      term,
      host,
      () => loggedIn && active === k && !reading && t.status === "connected",
    );
  if (isAndroid) t.disposeNativePaste = installAndroidNativePaste(term, host);
  if (isIOS || isAndroid) installNativeClipboard(term, host);
  installTerminalScroll(
    term,
    host,
    () => active === k && !reading,
    () => (isIOS || isAndroid) && hasNativeSelection(host),
  );
  term.onData((data) => {
    if (active === key(t.identity)) send(data);
  });
  term.onBinary((data) => {
    if (t.ws?.readyState === WebSocket.OPEN && t.status === "connected")
      t.ws.send(Uint8Array.from(data, (c) => c.charCodeAt(0)));
  });
  term.onResize(({ cols, rows }) => {
    if (t.ws?.readyState === WebSocket.OPEN && t.status === "connected")
      t.ws.send(
        JSON.stringify({ type: "resize", ...terminalSize(cols, rows) }),
      );
  });
  if (activate) selectTab(k);
  else {
    if (isAndroid) t.host.inert = true;
    t.host.style.visibility = "hidden";
    t.term.options.disableStdin = true;
  }
}
function ensureActiveConnection() {
  const tab = tabs.get(active);
  if (
    !loggedIn ||
    !snapshot.online ||
    !navigator.onLine ||
    reading ||
    document.visibilityState !== "visible" ||
    !tab
  )
    return;
  if (!sessions.some((session) => key(session) === active)) return;
  if (tab.status === "disconnected") connect(tab);
}
function releaseActiveConnection() {
  const tab = tabs.get(active);
  if (!tab) return;
  releaseTerminalView(tab);
}
function scheduleReconnect(t: Tab) {
  clearTimeout(t.retryTimer);
  t.retryTimer = undefined;
  const delay = t.recovery.delay();
  if (
    !Number.isFinite(delay) ||
    !loggedIn ||
    reading ||
    active !== key(t.identity) ||
    document.visibilityState !== "visible"
  )
    return;
  t.retryTimer = window.setTimeout(
    () => {
      t.retryTimer = undefined;
      if (tabs.get(key(t.identity)) === t) ensureActiveConnection();
    },
    Math.max(1, delay),
  );
}
function connect(t: Tab, manual = false) {
  if (
    !loggedIn ||
    loggingOut ||
    !navigator.onLine ||
    reading ||
    tabs.get(key(t.identity)) !== t ||
    active !== key(t.identity) ||
    document.visibilityState !== "visible"
  )
    return;
  if (manual) {
    releaseTerminalView(t);
    t.recovery.reset();
  }
  if (t.ws && t.ws.readyState < WebSocket.CLOSING) return;
  if (t.output.pending() > 0) return;
  if (t.recovery.delay() > 0) {
    scheduleReconnect(t);
    return;
  }
  clearTimeout(t.retryTimer);
  clearTimeout(t.openTimer);
  t.heartbeat?.dispose();
  t.heartbeat = undefined;
  t.nativeInput?.flush();
  t.nativeInput?.cancel();
  t.generation++;
  const gen = t.generation;
  const connectionStarted = Date.now();
  t.ws?.close();
  t.fit.fit();
  t.status = "connecting";
  renderTabs();
  const ws = new WebSocket(
    `${location.origin.replace("https:", "wss:")}/api/terminal`,
  );
  ws.binaryType = "arraybuffer";
  t.ws = ws;
  const fail = (kind: DisconnectKind, code = 0) => {
    if (t.generation !== gen) return;
    t.generation++;
    clearTimeout(t.openTimer);
    t.openTimer = undefined;
    t.heartbeat?.dispose();
    t.heartbeat = undefined;
    t.ws = undefined;
    t.status = "disconnected";
    t.nativeInput?.cancel();
    const event = t.recovery.failed(kind);
    // Fixed categories only: no terminal text, URLs, account IDs or server reason strings.
    console.info("[HMux] terminal disconnected", { ...event, code });
    diagnostics.record("terminal-failed", {
      reason: kind,
      code,
      attempt: event.attempt,
      retry_ms: event.retryMs,
      duration_ms: Date.now() - connectionStarted,
    });
    ws.close();
    if (active === key(t.identity)) {
      notice(
        t.recovery.description() +
          ` · 약 ${Math.ceil(event.retryMs / 1000)}초 후 다시 연결`,
      );
    }
    renderTabs();
    scheduleReconnect(t);
  };
  let outputFlow = false;
  t.openTimer = window.setTimeout(() => fail("timeout"), 20000);
  ws.onopen = () => {
    if (t.generation !== gen) {
      ws.close();
      return;
    }
    ws.send(
      JSON.stringify({
        type: "open",
        capabilities: ["terminal-output-flow-v1"],
        session: t.identity,
        ...terminalSize(t.term.cols, t.term.rows),
      }),
    );
  };
  ws.onmessage = (e) => {
    if (t.generation !== gen) return;
    t.heartbeat?.received();
    if (typeof e.data === "string") {
      try {
        const message = JSON.parse(e.data);
        if (message.type === "refresh-result" && active === key(t.identity)) {
          if (message.ok !== true)
            notice("화면을 다시 그리지 못했습니다. 잠시 후 다시 시도하세요.");
          else {
            t.term.refresh(0, t.term.rows - 1);
            for (const id of [
              "#terminal-refresh",
              "#terminal-refresh-mobile",
            ]) {
              $(id).animate([{ color: "#8cb7d9" }, { color: "#878580" }], {
                duration: 700,
              });
            }
          }
        }
        if (message.type === "ready") {
          outputFlow = message.output_flow === true;
          t.heartbeat?.dispose();
          t.heartbeat =
            message.heartbeat === true
              ? createTerminalHeartbeat(() => fail("timeout"))
              : undefined;
          clearTimeout(t.openTimer);
          t.openTimer = undefined;
          if (t.recovery.description())
            diagnostics.record("terminal-recovered", {
              duration_ms: Date.now() - connectionStarted,
            });
          t.recovery.ready();
          notice("");
          t.status = "connected";
          t.nativeInput?.cancel();
          t.term.reset();
          renderTabs();
          if (!isAndroid && active === key(t.identity)) focusTerminal(t);
        }
      } catch {
        fail("protocol");
      }
    } else {
      const bytes = new Uint8Array(e.data);
      if (
        !t.output.enqueue(bytes, () => {
          // Old xterm callbacks must never credit a replacement connection.
          if (
            outputFlow &&
            t.generation === gen &&
            ws.readyState === WebSocket.OPEN
          ) {
            ws.send(
              JSON.stringify({
                type: "output-ack",
                received: bytes.byteLength,
              }),
            );
          }
        })
      ) {
        fail("output-overflow");
      }
    }
  };
  ws.onclose = (event) => fail(disconnectKind(event.code), event.code);
  // close carries the actual capacity/policy code; error alone does not.
  ws.onerror = () => ws.close();
}
function send(data: string) {
  const t = tabs.get(active);
  if (
    !t ||
    reading ||
    t.ws?.readyState !== WebSocket.OPEN ||
    t.status !== "connected"
  )
    return;
  t.nativeInput?.flush();
  if (ctrl) {
    if (/^[a-z@\[\]\\^_]$/i.test(data))
      data = String.fromCharCode(data.toUpperCase().charCodeAt(0) & 31);
    ctrl = false;
    $("#ctrl").setAttribute("aria-pressed", "false");
  }
  const bytes = new TextEncoder().encode(data);
  if (
    bytes.length > 256 << 10 ||
    t.ws.bufferedAmount + bytes.length > 512 << 10
  ) {
    notice("입력 전송량이 많습니다. 나누어 붙여넣거나 연결을 확인하세요.");
    return;
  }
  for (let i = 0; i < bytes.length; i += 16384)
    t.ws.send(bytes.subarray(i, i + 16384));
}
function dialog(title: string) {
  attachments?.invalidate();
  tabs.get(active)?.interaction?.hide();
  dialogCleanup?.();
  dialogCleanup = undefined;
  $("#dialog").classList.remove("settings-dialog");
  $("#dialog-title").textContent = title;
  $("#dialog-body").replaceChildren();
  // Native dialog restores its prior focus on close. Keep that target off the
  // terminal textarea so Android does not reopen the software keyboard.
  if (isAndroid) $("#settings").focus({ preventScroll: true });
  $<HTMLDialogElement>("#dialog").showModal();
  attachments?.refresh();
  scheduleTerminalLayout();
  return $("#dialog-body");
}
function dialogActions(primary: HTMLButtonElement) {
  const actions = text("div", "", "dialog-actions");
  const cancel = text("button", "취소", "secondary") as HTMLButtonElement;
  cancel.type = "button";
  cancel.onclick = () => $<HTMLDialogElement>("#dialog").close();
  actions.append(cancel, primary);
  return actions;
}
function formField(
  parent: HTMLElement,
  label: string,
  value = "",
  type = "text",
) {
  const l = text("label", label);
  const i = document.createElement("input");
  i.type = type;
  i.value = value;
  l.append(i);
  parent.append(l);
  return i;
}
function editDialog(s: Session) {
  const body = dialog("세션 설정");
  body.append(text("p", s.name, "dialog-context"));
  const form = document.createElement("form");
  const alias = formField(form, "표시 별칭", s.alias || "");
  alias.maxLength = 80;
  const error = text("p", "", "error");
  const save = text("button", "저장", "primary") as HTMLButtonElement;
  save.type = "submit";
  form.append(error, dialogActions(save));
  form.onsubmit = async (e) => {
    e.preventDefault();
    save.disabled = true;
    save.textContent = "저장 중…";
    try {
      await action("alias", s, { alias: alias.value });
      s.alias = alias.value;
      $<HTMLDialogElement>("#dialog").close();
      renderSessions();
      renderTabs();
      await refresh();
    } catch (e) {
      error.textContent = (e as Error).message;
    } finally {
      save.disabled = false;
      save.textContent = "저장";
    }
  };
  body.append(form);
  const hide = text(
    "button",
    s.hidden ? "목록에 다시 표시" : "목록에서 숨기기",
    "session-visibility-action",
  ) as HTMLButtonElement;
  hide.onclick = async () => {
    hide.disabled = true;
    try {
      await action("hidden", s, { hidden: !s.hidden });
      s.hidden = !s.hidden;
      $<HTMLDialogElement>("#dialog").close();
      renderSessions();
      await refresh();
    } catch (e) {
      error.textContent = (e as Error).message;
      hide.disabled = false;
    }
  };
  body.append(
    hide,
    text("p", "숨기거나 탭을 닫아도 Home의 작업은 종료되지 않습니다.", "muted"),
  );
}
async function createDialog() {
  const body = dialog("새 작업 시작");
  body.append(text("p", "작업 환경을 선택하고 새 세션을 시작하세요.", "muted"));
  const form = document.createElement("form");
  const name = formField(form, "세션 이름 (선택)");
  name.maxLength = 80;
  const l = text("label", "프로파일");
  const select = document.createElement("select");
  l.append(select);
  form.append(l);
  const error = text("p", "", "error");
  const save = text("button", "세션 만들기", "primary") as HTMLButtonElement;
  save.disabled = true;
  form.append(error, dialogActions(save));
  body.append(form);
  try {
    const profiles = (await action("profiles")) as {
      id: string;
      label: string;
    }[];
    for (const p of profiles) {
      const o = text("option", p.label || p.id) as HTMLOptionElement;
      o.value = p.id;
      select.append(o);
    }
    save.disabled = !profiles.length;
    if (!profiles.length)
      error.textContent = "Home에 등록된 프로파일이 없습니다.";
  } catch (e) {
    error.textContent = (e as Error).message;
  }
  form.onsubmit = async (e) => {
    e.preventDefault();
    save.disabled = true;
    try {
      const result = await action("create", undefined, {
        name: name.value,
        profile: select.value,
      });
      $<HTMLDialogElement>("#dialog").close();
      await refresh();
      const s = sessions.find(
        (s) => s.id === result.id && s.created_at === result.created_at,
      );
      if (s) openSession(s);
      else notice("세션을 만들었습니다. 목록이 갱신되면 열어주세요.");
    } catch (e) {
      error.textContent = (e as Error).message;
      save.disabled = false;
    }
  };
}
function settingsDialog() {
  const body = dialog("설정");
  $("#dialog").classList.add("settings-dialog");
  const appearance = text("section", "", "settings-section");
  appearance.append(
    text("h3", "터미널"),
    text("p", "이 기기에 편한 크기로 맞추세요.", "muted"),
  );
  const preview = text("div", "❯ 이어지는 작업, 나만의 터미널", "font-preview");
  preview.style.fontFamily = terminalFonts.family();
  preview.style.fontSize = `${fontSize}px`;
  appearance.append(preview);
  const controls = text("div", "", "font-controls");
  const decrease = button("글자 작게", "minus", () => apply(fontSize - 1));
  const increase = button("글자 크게", "plus", () => apply(fontSize + 1));
  const input = formField(controls, "글자 크기", String(fontSize), "range");
  input.min = "8";
  input.max = "24";
  const value = text("output", `${fontSize}px`, "font-value");
  controls.prepend(decrease);
  controls.append(increase, value);
  const reset = text("button", "기본 크기로", "subtle-button");
  reset.setAttribute("type", "button");
  reset.onclick = () => apply(preferredFontSize(mobileScreen.matches, null));
  appearance.append(controls, reset);
  function apply(size: number) {
    fontSize = Math.max(8, Math.min(24, size));
    input.value = String(fontSize);
    value.textContent = `${fontSize}px`;
    preview.style.fontSize = `${fontSize}px`;
    preferences.set(fontPreferenceKey(), String(fontSize));
    for (const t of tabs.values()) t.term.options.fontSize = fontSize;
    scheduleTerminalLayout();
  }
  input.oninput = () => apply(Number(input.value));
  const details = text("div", "", "appearance-details");
  details.append(
    text("span", "Monatendard Mono"),
    text("span", "Flexoki Dark"),
  );
  appearance.append(details);
  const keys = text("section", "", "settings-section shortcut-guide");
  keys.append(text("h3", "키보드 단축키"));
  for (const [label, chord] of [
    ["탭 이동", "Alt Shift ← / →"],
    ["번호로 탭 이동", "Alt 1–9"],
    ["사이드바", "Alt L / Alt ` (₩)"],
    ["현재 탭 닫기", "Alt W"],
    ["로그아웃", "Alt Q"],
    ["세션 검색", "⌘ / Ctrl K"],
  ]) {
    const row = text("div", "", "shortcut-row");
    row.append(text("span", label), text("kbd", chord));
    keys.append(row);
  }
  const install = text("section", "", "settings-section");
  install.append(
    text("h3", "어디서든 HMux"),
    text("p", "홈 화면에 추가하고 앱처럼 열어보세요.", "muted"),
    installButton(),
  );
  const loginSessions = text("section", "", "settings-section login-sessions");
  const notifications = text(
    "section",
    "",
    "settings-section push-notifications",
  );
  const security = text("section", "", "settings-section account-security");
  const diagnosticPanel = text("section", "", "settings-section");
  const usagePanel = text("section", "", "settings-section");
  body.append(
    appearance,
    usagePanel,
    notifications,
    security,
    loginSessions,
    diagnosticPanel,
    keys,
    install,
  );
  let disposeSessions: (() => void) | undefined;
  const refreshSessions = () => {
    disposeSessions?.();
    loginSessions.replaceChildren();
    disposeSessions = installLoginSessions(
      loginSessions,
      api,
      () => {
        preferences.remove(workspaceStorageKey);
        showLogin();
      },
      () => attachments?.cancel(),
    );
  };
  refreshSessions();
  const disposeSecurity = installAccountSecurity(
    security,
    api,
    refreshSessions,
  );
  const disposePush = installPushNotifications(notifications, api, loginID);
  const disposeUsagePreferences = installUsagePreferences(
    usagePanel,
    api,
    applyUsagePreferences,
  );
  const disposeDiagnostics = installDiagnosticSettings(
    diagnosticPanel,
    api,
    diagnostics,
  );
  dialogCleanup = () => {
    disposeUsagePreferences();
    disposeDiagnostics();
    disposePush();
    disposeSecurity();
    disposeSessions?.();
  };
}

async function toggleReader() {
  const t = tabs.get(active);
  if (!t) return;
  if (reading) {
    selectTab(active);
    return;
  }
  t.nativeInput?.flush();
  t.interaction?.hide();
  reading = true;
  attachments?.invalidate();
  attachments?.refresh();
  readerAbort?.abort();
  readerAbort = new AbortController();
  const epoch = ++readEpoch;
  const identity = { ...t.identity };
  $("#conversation").setAttribute("aria-pressed", "true");
  t.host.style.visibility = "hidden";
  t.term.options.disableStdin = true;
  t.term.blur();
  const reader = $("#reader");
  reader.hidden = false;
  reader.replaceChildren(
    text("p", "현재 tmux 세션의 Codex 대화를 확인하고 있습니다…", "muted"),
  );
  try {
    const data = (await action(
      "conversation",
      identity,
      undefined,
      readerAbort.signal,
    )) as Conversation;
    if (!reading || epoch !== readEpoch || key(identity) !== active) return;
    if (!renderConversation(reader, data, () => selectTab(active))) return;
    requestAnimationFrame(() => {
      if (reading && epoch === readEpoch && key(identity) === active) {
        reader.scrollTop = reader.scrollHeight;
      }
    });
  } catch (e) {
    if (epoch === readEpoch)
      reader.replaceChildren(text("p", (e as Error).message, "error"));
  }
}
function renderFooter() {
  renderUsageFooter(
    snapshot,
    {
      dog: $("#bedl"),
      usageButton: $("#usage"),
      metrics: $("#metrics"),
    },
    usagePreferences,
  );
}
function usageDialog() {
  const body = dialog("계정 사용량");
  const render = () => {
    const scrollTop = body.scrollTop;
    body.replaceChildren();
    renderUsagePanel(
      body,
      snapshot,
      $("#metrics").textContent || "",
      usagePreferences,
    );
    body.scrollTop = scrollTop;
  };
  render();
  refreshUsageDialog = render;
  const timer = window.setInterval(render, 30000);
  dialogCleanup = () => {
    window.clearInterval(timer);
    refreshUsageDialog = undefined;
  };
}

async function refresh() {
  if (refreshRequest) return;
  const request = new AbortController();
  const epoch = accountEpoch;
  refreshRequest = request;
  try {
    const next = (await api(
      "/api/state",
      undefined,
      request.signal,
    )) as Snapshot;
    if (!loggedIn || epoch !== accountEpoch || refreshRequest !== request)
      return;
    snapshot = next;
    const nextPreferences = parseUsagePreferences(next.usage_preferences);
    if (
      nextPreferences &&
      nextPreferences.revision >= usagePreferences.revision
    )
      usagePreferences = nextPreferences;
    if (next.catalog?.sessions) sessions = next.catalog.sessions;
    $("#home-state").textContent = next.online
      ? "연결됨 · Home에서 실행 중"
      : "오프라인 · Home 연결 대기";
    $("#home-dot").classList.toggle("online", next.online);
    renderSessions();
    renderTabs();
    renderFooter();
    refreshUsageDialog?.();
    ensureActiveConnection();
    resolvePushTarget();
    if (next.online) void syncSharedWorkspace();
  } finally {
    if (refreshRequest === request) refreshRequest = undefined;
  }
}
async function poll() {
  if (!loggedIn) return;
  const epoch = accountEpoch;
  try {
    ensureActiveConnection();
    await refresh();
  } catch (e) {
    if (loggedIn && epoch === accountEpoch) reportError(e);
  } finally {
    if (
      loggedIn &&
      epoch === accountEpoch &&
      document.visibilityState === "visible"
    ) {
      clearTimeout(pollTimer);
      pollTimer = window.setTimeout(poll, 5000);
    }
  }
}
let fontRestore: Promise<void> | undefined;
function restoreTerminalFonts(): Promise<void> {
  if (fontRestore) return fontRestore;
  fontRestore = (async () => {
    try {
      await terminalFonts.load();
      for (const tab of tabs.values()) {
        tab.term.options.fontFamily = terminalFonts.family();
        tab.term.clearTextureAtlas();
        tab.term.refresh(0, tab.term.rows - 1);
      }
      requestAnimationFrame(() => tabs.get(active)?.fit.fit());
    } catch {
      notice(
        "터미널 폰트 연결 대기 중입니다. 앱으로 돌아오거나 네트워크가 복구되면 다시 불러옵니다.",
      );
    }
  })().finally(() => {
    fontRestore = undefined;
  });
  return fontRestore;
}
function showConnectionRecovery(message: string) {
  const main = text("main", "", "login");
  const panel = text("section", "", "login-panel");
  const retry = text("button", "다시 연결", "primary");
  retry.type = "button";
  retry.onclick = () => void start();
  panel.append(text("h2", "연결 확인 중"), text("p", message, "muted"), retry);
  main.append(panel);
  app.replaceChildren(main);
}
async function start() {
  if (loggedIn || startRequest) return;
  clearTimeout(startTimer);
  const request = {};
  const epoch = accountEpoch;
  startRequest = request;
  showConnectionRecovery("기존 로그인을 확인하고 있습니다.");
  try {
    const session = await api("/api/session");
    if (epoch !== accountEpoch) return;
    workspaceStorageKey = session.profile
      ? `hmux.tabs.${session.profile}`
      : "hmux.tabs";
    csrf = session.csrf;
    loginID = session.login_id;
    diagnostics.bind(loginID);
    loggedIn = true;
    startFailures = 0;
    bootstrapping = true;
    shell();
    $("#username").textContent = session.username;
    pushPresence = installPushPresence(api, () => tabs.get(active)?.identity);
    try {
      pendingWorkspace = JSON.parse(
        preferences.get(workspaceStorageKey) || "{}",
      );
    } catch {
      pendingWorkspace = {};
    }
    // Font downloads and shared-tab synchronization must not block recovery.
    void restoreTerminalFonts();
    await poll();
  } catch (error) {
    if (epoch !== accountEpoch || (error as Error).name === "AbortError")
      return;
    const delay = retryDelay(++startFailures);
    showConnectionRecovery(
      `서버에 연결하지 못했습니다. 약 ${Math.ceil(delay / 1000)}초 후 다시 확인합니다.`,
    );
    startTimer = window.setTimeout(() => {
      if (epoch === accountEpoch) void start();
    }, delay);
  } finally {
    if (startRequest === request) startRequest = undefined;
  }
}
document.addEventListener("keydown", (e) => {
  if (e.defaultPrevented || handleWorkspaceShortcut(e)) return;
  if (loggedIn && (e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
    e.preventDefault();
    setSidebar(true);
    $("#search").focus();
  }
  if (e.key === "Escape" && mobileSidebar.matches && sidebarVisible())
    setSidebar(false);
});
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "hidden") {
    releaseActiveConnection();
    clearTimeout(pollTimer);
  }
  if (document.visibilityState === "visible" && loggedIn) {
    resumeConnection();
  }
});
void start();

const mobileSidebar = window.matchMedia(
  "(max-width: 700px), (pointer: coarse) and (max-height: 500px)",
);
function sidebarVisible() {
  return mobileSidebar.matches
    ? app.classList.contains("show-sessions")
    : !app.classList.contains("sidebar-collapsed");
}
function updateSidebarAccessibility() {
  const visible = sidebarVisible();
  const sidebar = document.querySelector<HTMLElement>("#session-sidebar");
  if (sidebar) sidebar.inert = !visible;
  document
    .querySelector("#menu")
    ?.setAttribute("aria-expanded", String(visible));
}
function setSidebar(visible: boolean, focus = true) {
  scheduleTerminalLayout();
  if (mobileSidebar.matches) app.classList.toggle("show-sessions", visible);
  else {
    app.classList.toggle("sidebar-collapsed", !visible);
    preferences.set("hmux.sidebar", visible ? "visible" : "hidden");
  }
  updateSidebarAccessibility();
  if (!visible && focus) {
    const t = tabs.get(active);
    if (t) focusTerminal(t);
    else $("#menu").focus();
  }
  requestAnimationFrame(() => {
    const t = tabs.get(active);
    if (t) t.fit.fit();
  });
}
mobileSidebar.addEventListener("change", () => {
  app.classList.remove("show-sessions");
  updateSidebarAccessibility();
});
function handleWorkspaceShortcut(event: KeyboardEvent) {
  const shortcut = workspaceShortcut(event);
  if (!loggedIn || !shortcut || document.querySelector("dialog[open]"))
    return false;
  event.preventDefault();
  if (event.type !== "keydown") return true;
  if (shortcut === "logout") {
    if (!event.repeat) $("#logout").click();
    return true;
  }
  if (shortcut === "close") {
    if (!event.repeat && tabs.has(active)) closeTab(active);
    return true;
  }
  if (shortcut === "sidebar") {
    if (!event.repeat) setSidebar(!sidebarVisible());
    return true;
  }
  const order = [...tabs.keys()];
  if (typeof shortcut === "object") {
    const target = order[shortcut.tabIndex];
    if (target) selectTab(target);
    return true;
  }
  if (order.length)
    selectTab(
      order[
        (order.indexOf(active) +
          (event.code === "ArrowLeft" ? -1 : 1) +
          order.length) %
          order.length
      ],
    );
  return true;
}

function focusTerminal(t: Tab) {
  if (
    active === key(t.identity) &&
    !hasNativeSelection(t.host) &&
    !reading &&
    !$<HTMLDialogElement>("#dialog")?.open &&
    document.activeElement !== $("#search") &&
    document.visibilityState === "visible"
  )
    t.term.focus();
}

function localWorkspace() {
  return {
    tabs: [...tabs.values()].map((t) => ({ ...t.identity })),
    selected: tabs.get(active)?.identity,
  };
}
async function syncSharedWorkspace() {
  if (!loggedIn || !snapshot.online || workspaceRequest) return;
  const request = new AbortController();
  workspaceRequest = request;
  const epoch = workspaceEpoch;
  try {
    if (sharedLoaded && workspaceDirty && !pendingChange) {
      pendingEdit = workspaceEdit;
      const local = localWorkspace();
      pendingChange = {
        operation_id: crypto.randomUUID(),
        revision: workspaceRevision,
        base: workspaceBase.map((id) => ({ ...id })),
        tabs: local.tabs,
      };
    }
    const sent = pendingChange;
    const value = validateWorkspace(
      (await action(
        "workspace",
        undefined,
        {
          change: sent || null,
        },
        request.signal,
      )) as SharedWorkspace,
    );
    if (!loggedIn || epoch !== workspaceEpoch) return;
    workspaceRevision = value.revision;
    if (value.conflict) {
      pendingChange = undefined;
      workspaceDirty = false;
      applySharedWorkspace(value);
      notice(
        "공용 탭이 변경되어 Home 목록을 다시 불러왔습니다. 최근 로컬 탭 변경은 저장되지 않았습니다.",
      );
      $("#footer-connection").title = "공용 탭 · 최신 목록 복원됨";
      return;
    }
    if (sent) {
      workspaceBase = sent.tabs;
      pendingChange = undefined;
      workspaceDirty = workspaceEdit !== pendingEdit;
    }
    if (!sharedLoaded && !value.initialized) {
      applyingShared = true;
      try {
        if (pendingWorkspace && Array.isArray(pendingWorkspace.tabs)) {
          for (const id of pendingWorkspace.tabs.slice(0, 32)) {
            const found = sessions.find((s) => key(s) === key(id));
            if (found) openSession(found, false);
          }
          if (tabs.size)
            selectTab(
              pendingWorkspace.active && tabs.has(key(pendingWorkspace.active))
                ? key(pendingWorkspace.active)
                : [...tabs.keys()][0],
            );
        }
      } finally {
        applyingShared = false;
        bootstrapping = false;
        pendingWorkspace = undefined;
        sharedLoaded = true;
        workspaceBase = [];
        lastWorkspaceTabs = JSON.stringify(localWorkspace().tabs);
        workspaceDirty = tabs.size > 0;
      }
    } else if (!workspaceDirty) {
      applySharedWorkspace(value);
    }
    $("#footer-connection").title = workspaceDirty
      ? "공용 탭 · 동기화 중"
      : "공용 탭 · 동기화됨";
    resolvePushTarget();
  } catch (error) {
    if (epoch !== workspaceEpoch) return;
    if (loggedIn) $("#footer-connection").title = "공용 탭 · 동기화 대기";
  } finally {
    if (workspaceRequest === request) workspaceRequest = undefined;
  }
  if (epoch === workspaceEpoch && workspaceDirty && !pendingChange)
    window.setTimeout(() => {
      if (epoch === workspaceEpoch) void syncSharedWorkspace();
    }, 100);
}
function applySharedWorkspace(value: SharedWorkspace) {
  const first = !sharedLoaded;
  const previous = active;
  applyingShared = true;
  try {
    const allowed = new Set(value.tabs.map(key));
    for (const k of [...tabs.keys()]) if (!allowed.has(k)) closeTab(k);
    for (const id of value.tabs) {
      if (tabs.has(key(id))) continue;
      const session = sessions.find((s) => key(s) === key(id)) || {
        ...id,
        name: "세션 없음",
        window_count: 0,
        attached_clients: 0,
      };
      openSession(session, false);
    }
    const ordered = value.tabs.map(
      (id) => [key(id), tabs.get(key(id))!] as const,
    );
    tabs.clear();
    for (const [k, t] of ordered) tabs.set(k, t);
    const selected =
      !first && tabs.has(previous)
        ? previous
        : first &&
            pendingWorkspace?.active &&
            tabs.has(key(pendingWorkspace.active))
          ? key(pendingWorkspace.active)
          : [...tabs.keys()][0] || "";
    if (active !== selected) selectTab(selected);
    else {
      renderTabs();
      renderSessions();
    }
    workspaceBase = localWorkspace().tabs;
    lastWorkspaceTabs = JSON.stringify(workspaceBase);
    sharedLoaded = true;
    bootstrapping = false;
    pendingWorkspace = undefined;
    persistTabs();
    resolvePushTarget();
  } finally {
    applyingShared = false;
  }
}

navigator.serviceWorker?.addEventListener("message", (event) => {
  const target = pushTargetFromMessage(event.data);
  const source = event.source as ServiceWorker | null;
  if (!target || !source || !("scriptURL" in source)) return;
  let sourceURL: URL;
  try {
    sourceURL = new URL(source.scriptURL);
  } catch {
    return;
  }
  if (
    sourceURL.origin !== window.location.origin ||
    sourceURL.pathname !== "/sw.js" ||
    !loggedIn ||
    target.login_id !== loginID
  )
    return;
  pendingPushTarget = target;
  resolvePushTarget();
  event.ports[0]?.postMessage({ type: "hmux-push-open-ack" });
});

const viewportController = createViewportController(app, isIOS, isAndroid);
function resizeMobileViewport() {
  viewportController.update();
}
window.visualViewport?.addEventListener("resize", scheduleTerminalLayout);
window.visualViewport?.addEventListener("scroll", scheduleTerminalLayout);
window.addEventListener("resize", scheduleTerminalLayout);
resizeMobileViewport();
function resumeConnection() {
  if (!loggedIn || document.visibilityState !== "visible") return;
  diagnostics.record("resume");
  // Requests created before suspension may never complete on the old network.
  refreshRequest?.abort();
  refreshRequest = undefined;
  tabs.get(active)?.recovery.resume();
  void restoreTerminalFonts();
  clearTimeout(pollTimer);
  void poll();
}
window.addEventListener("pagehide", releaseActiveConnection);
window.addEventListener("offline", () => {
  if (loggedIn) diagnostics.record("offline");
  releaseActiveConnection();
  if (loggedIn) renderTabs();
});
window.addEventListener("pageshow", (event) => {
  scheduleTerminalLayout();
  resizeMobileViewport();
  requestAnimationFrame(resizeMobileViewport);
  if (loggedIn) {
    if (event.persisted) releaseActiveConnection();
    resumeConnection();
  }
});
window.addEventListener("online", () => {
  if (loggedIn) {
    // Replace a socket tied to the previous network; keep an in-flight new open.
    if (tabs.get(active)?.status === "connected") releaseActiveConnection();
    resumeConnection();
  }
});
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible") {
    scheduleTerminalLayout();
    resizeMobileViewport();
    requestAnimationFrame(resizeMobileViewport);
  }
});
for (const type of ["gesturestart", "gesturechange"])
  document.addEventListener(type, (e) => e.preventDefault(), {
    passive: false,
  });
document.addEventListener(
  "touchmove",
  (e) => {
    if (e.touches.length > 1) e.preventDefault();
  },
  { passive: false },
);
mobileScreen.addEventListener("change", () => {
  fontSize = preferredFontSize(
    mobileScreen.matches,
    preferences.get(fontPreferenceKey()),
  );
  for (const tab of tabs.values()) tab.term.options.fontSize = fontSize;
  requestAnimationFrame(() => tabs.get(active)?.fit.fit());
});

document.addEventListener("focusin", scheduleTerminalLayout);
document.addEventListener("focusout", () =>
  setTimeout(scheduleTerminalLayout, 100),
);
