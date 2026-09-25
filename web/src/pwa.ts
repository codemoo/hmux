import { t, getLocale, bindText, onLocaleChange } from "./i18n.ts";
interface InstallPrompt extends Event {
  prompt(): Promise<void>;
  userChoice: Promise<{ outcome: "accepted" | "dismissed" }>;
}
let pending: InstallPrompt | undefined;
const standalone = () =>
  window.matchMedia("(display-mode: standalone)").matches ||
  (navigator as Navigator & { standalone?: boolean }).standalone === true;
const installLabel = () =>
  standalone()
    ? t("Running as HMux app", "HMux 앱으로 실행 중")
    : pending
      ? t("Install HMux app", "HMux 앱 설치")
      : t("Add HMux to Home Screen", "홈 화면에 HMux 추가");
function updateButton(button: HTMLButtonElement) {
  bindText(button, installLabel);
  button.disabled = standalone();
}
function updateButtons() {
  // Query only mounted controls. A global callback set would retain buttons from
  // closed dialogs/login views until another (potentially rare) install event.
  for (const button of document.querySelectorAll<HTMLButtonElement>(
    "button[data-hmux-install]",
  ))
    updateButton(button);
}
window.addEventListener("beforeinstallprompt", (event) => {
  event.preventDefault();
  pending = event as InstallPrompt;
  updateButtons();
});
window.addEventListener("appinstalled", () => {
  pending = undefined;
  updateButtons();
});
export function installButton() {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "secondary";
  button.setAttribute("data-hmux-install", "");
  updateButton(button);
  button.onclick = async () => {
    if (pending) {
      const prompt = pending;
      pending = undefined;
      try {
        await prompt.prompt();
        await prompt.userChoice;
        return;
      } catch {
        // Fall back to browser-specific instructions if the prompt is unavailable.
      } finally {
        updateButtons();
      }
    }
    const help = document.createElement("p");
    help.className = "muted pwa-help";
    bindText(help, () =>
      /iPhone|iPad|iPod/.test(navigator.userAgent) ||
      (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1)
        ? t(
            "In Safari, select Share → Add to Home Screen → Open as Web App.",
            "Safari의 공유 버튼 → ‘홈 화면에 추가’ → ‘웹 앱으로 열기’를 선택하세요.",
          )
        : t(
            "In the browser menu, select Install App or Add to Home Screen. If unavailable, open in Chrome or Edge.",
            "브라우저 메뉴에서 ‘앱 설치’ 또는 ‘홈 화면에 추가’를 선택하세요. 설치 메뉴가 없으면 Chrome 또는 Edge에서 열어주세요.",
          ),
    );
    button.parentElement?.querySelector(".pwa-help")?.remove();
    button.after(help);
  };
  return button;
}
if ("serviceWorker" in navigator) {
  const registerWorker = () => {
    // The registration script URL carries only the explicit device UI locale.
    // The worker reads it independently; no transcript or account data is sent.
    void navigator.serviceWorker
      .register(`/sw.js?lang=${getLocale()}`, {
        scope: "/",
        updateViaCache: "none",
      })
      .catch(() => {
        // Login and online terminal use remain available if installation is blocked.
      });
  };
  window.addEventListener("load", registerWorker);
  onLocaleChange(registerWorker);
}
