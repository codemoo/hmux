export type DisconnectKind =
  | "network"
  | "timeout"
  | "limit"
  | "unavailable"
  | "output-overflow"
  | "protocol";

export function retryDelay(
  attempt: number,
  minimum = 1000,
  random = Math.random,
) {
  return Math.min(
    60_000,
    Math.max(minimum, 1000 * 2 ** Math.min(attempt - 1, 6)) *
      (1 + random() * 0.2),
  );
}

export function createConnectionRecovery(now = Date.now, random = Math.random) {
  let failures = 0;
  let retryAt = 0;
  let connectedAt: number | undefined;
  let issue: DisconnectKind | undefined;
  const descriptions: Record<DisconnectKind, string> = {
    network: "네트워크 연결이 끊겼습니다",
    timeout: "터미널 연결 시간이 초과되었습니다",
    limit: "동시 터미널 연결 한도에 도달했습니다",
    unavailable: "터미널을 열 수 없습니다",
    "output-overflow":
      "출력 처리량이 많아 자동 재연결을 멈췄습니다. 다시 연결을 눌러주세요",
    protocol: "연결 응답을 확인하지 못했습니다",
  };
  const released = () => {
    if (connectedAt !== undefined && now() - connectedAt >= 30_000)
      failures = 0;
    connectedAt = undefined;
  };
  return {
    ready() {
      connectedAt = now();
      retryAt = 0;
      issue = undefined;
    },
    released,
    reset() {
      failures = 0;
      retryAt = 0;
      connectedAt = undefined;
      issue = undefined;
    },
    failed(kind: DisconnectKind) {
      released();
      issue = kind;
      failures++;
      const delay =
        kind === "output-overflow"
          ? Infinity
          : retryDelay(
              failures,
              kind === "limit" || kind === "unavailable" ? 10_000 : 1000,
              random,
            );
      retryAt = now() + delay;
      return {
        kind,
        attempt: failures,
        retryMs: Number.isFinite(delay) ? Math.round(delay) : null,
      };
    },
    delay: () => Math.max(0, retryAt - now()),
    description: () => (issue ? descriptions[issue] : ""),
  };
}
