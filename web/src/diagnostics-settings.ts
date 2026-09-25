import { t, msg, bindText } from "./i18n.ts";
import type { createDiagnostics } from "./diagnostics";

export function installDiagnosticSettings(
  container: HTMLElement,
  api: (path: string, body?: unknown, signal?: AbortSignal) => Promise<unknown>,
  diagnostics: ReturnType<typeof createDiagnostics>,
) {
  const controller = new AbortController();
  const title = document.createElement("h3");
  bindText(title, msg("Connection diagnostics", "접속 진단"));
  const description = document.createElement("p");
  description.className = "muted";
  bindText(
    description,
    msg(
      "Connection errors are collected automatically. Terminal content, input, and credentials are excluded.",
      "접속 오류를 자동 수집합니다. 터미널 내용·입력·인증 정보는 포함하지 않습니다.",
    ),
  );
  const download = document.createElement("button");
  download.type = "button";
  download.className = "secondary";
  bindText(download, msg("Download diagnostic log", "진단 로그 다운로드"));
  const status = document.createElement("p");
  status.className = "muted";
  status.setAttribute("role", "status");
  void api("/api/diagnostics", undefined, controller.signal)
    .then((value) => {
      if (controller.signal.aborted || download.disabled) return;
      const report = value as {
        counts?: Record<string, number>;
        storage_ok?: boolean;
      };
      const count = Object.values(report.counts || {}).reduce(
        (total, value) =>
          total + (Number.isSafeInteger(value) && value > 0 ? value : 0),
        0,
      );
      bindText(status, () =>
        report.storage_ok === false
          ? t(
              "Could not verify server storage. The download also includes this device’s records.",
              "서버 저장을 확인할 수 없습니다. 다운로드에 이 기기 기록도 포함됩니다.",
            )
          : t(
              `${count} errors in the last 7 days · collecting automatically`,
              `최근 7일 오류 ${count}건 · 자동 수집 중`,
            ),
      );
    })
    .catch(() => {
      if (!controller.signal.aborted && !download.disabled)
        bindText(
          status,
          msg(
            "Device records will be sent when the server reconnects.",
            "서버 연결이 돌아오면 기기 기록을 전송합니다.",
          ),
        );
    });
  download.onclick = async () => {
    download.disabled = true;
    bindText(
      status,
      msg(
        "Collecting recent diagnostic logs…",
        "최근 진단 로그를 모으고 있습니다…",
      ),
    );
    try {
      let server: unknown;
      try {
        server = await api("/api/diagnostics", undefined, controller.signal);
      } catch {
        if (controller.signal.aborted) return;
      }
      if (controller.signal.aborted) return;
      const report = {
        version: 1,
        server: server || null,
        device: diagnostics.snapshot(),
      };
      const url = URL.createObjectURL(
        new Blob([JSON.stringify(report, null, 2)], {
          type: "application/json",
        }),
      );
      const link = document.createElement("a");
      link.href = url;
      link.download = `hmux-diagnostics-${new Date().toISOString().slice(0, 10)}.json`;
      link.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      bindText(
        status,
        server
          ? msg(
              "Saved server records for this account and device records.",
              "현재 계정의 서버 기록과 이 기기 기록을 저장했습니다.",
            )
          : msg(
              "Saved device records only because the server could not be reached.",
              "서버 연결이 어려워 이 기기 기록만 저장했습니다.",
            ),
      );
    } catch {
      if (!controller.signal.aborted)
        bindText(
          status,
          msg(
            "Could not save the diagnostic file. Try again.",
            "진단 파일을 저장하지 못했습니다. 다시 시도해주세요.",
          ),
        );
    } finally {
      if (!controller.signal.aborted) download.disabled = false;
    }
  };
  container.append(title, description, download, status);
  return () => controller.abort();
}
