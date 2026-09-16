interface InstallPrompt extends Event {
  prompt(): Promise<void>;
  userChoice: Promise<{ outcome: "accepted" | "dismissed" }>;
}
let pending: InstallPrompt | undefined;
const listeners = new Set<() => void>();
const standalone = () =>
  window.matchMedia("(display-mode: standalone)").matches ||
  (navigator as Navigator & { standalone?: boolean }).standalone === true;
window.addEventListener("beforeinstallprompt", (event) => {
  event.preventDefault();
  pending = event as InstallPrompt;
  for (const update of listeners) update();
});
window.addEventListener("appinstalled", () => {
  pending = undefined;
  for (const update of listeners) update();
});
export function installButton() {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "secondary";
  const update = () => {
    if (!button.isConnected) {
      listeners.delete(update);
      return;
    }
    button.textContent = standalone()
      ? "HMux 앱으로 실행 중"
      : pending
        ? "HMux 앱 설치"
        : "홈 화면에 HMux 추가";
    button.disabled = standalone();
  };
  button.textContent = standalone()
    ? "HMux 앱으로 실행 중"
    : "홈 화면에 HMux 추가";
  button.disabled = standalone();
  listeners.add(update);
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
        for (const refresh of listeners) refresh();
      }
    }
    const help = document.createElement("p");
    help.className = "muted pwa-help";
    help.textContent =
      /iPhone|iPad|iPod/.test(navigator.userAgent) ||
      (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1)
        ? "Safari의 공유 버튼 → ‘홈 화면에 추가’ → ‘웹 앱으로 열기’를 선택하세요."
        : "브라우저 메뉴에서 ‘앱 설치’ 또는 ‘홈 화면에 추가’를 선택하세요. 설치 메뉴가 없으면 Chrome 또는 Edge에서 열어주세요.";
    button.parentElement?.querySelector(".pwa-help")?.remove();
    button.after(help);
  };
  return button;
}
if ("serviceWorker" in navigator) {
  window.addEventListener("load", () => {
    void navigator.serviceWorker
      .register("/sw.js", { scope: "/", updateViaCache: "none" })
      .catch(() => {
        // Login and online terminal use remain available if installation is blocked.
      });
  });
}
