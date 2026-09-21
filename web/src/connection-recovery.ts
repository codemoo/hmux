export type DisconnectKind =
  | "network"
  | "timeout"
  | "limit"
  | "unavailable"
  | "output-overflow"
  | "protocol";

// Private close codes are fixed gateway categories, never raw Home errors.
export function disconnectKind(code: number): DisconnectKind {
  if (code === 4002) return "output-overflow";
  if (code === 1013) return "limit";
  if (code === 1002) return "protocol";
  if (code === 1008 || code === 4001 || code === 4003) return "unavailable";
  return "network";
}

export function retryDelay(
  attempt: number,
  minimum = 1000,
  random = Math.random,
  maximum = 60_000,
) {
  return Math.min(
    maximum,
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
    "output-overflow": "출력 처리량이 많아 잠시 후 다시 연결합니다",
    protocol: "연결 응답을 확인하지 못했습니다",
  };
  return {
    ready() {
      connectedAt = now();
      retryAt = 0;
      issue = undefined;
    },
    released() {
      // Switching tabs or hiding a healthy view is not a failed connection.
      if (connectedAt !== undefined) failures = 0;
      connectedAt = undefined;
    },
    resume() {
      // A real foreground/network transition is a new opportunity to connect.
      // Preserve server-capacity and output-pressure cooldowns.
      if (issue === "network" || issue === "timeout" || issue === "protocol")
        retryAt = 0;
    },
    reset() {
      failures = 0;
      retryAt = 0;
      connectedAt = undefined;
      issue = undefined;
    },
    failed(kind: DisconnectKind) {
      if (connectedAt !== undefined && now() - connectedAt >= 10_000)
        failures = 0;
      connectedAt = undefined;
      issue = kind;
      failures++;
      const pressure =
        kind === "limit" ||
        kind === "unavailable" ||
        kind === "output-overflow";
      const delay = retryDelay(
        failures,
        pressure ? 10_000 : 1000,
        random,
        pressure ? 60_000 : 15_000,
      );
      retryAt = now() + delay;
      return {
        kind,
        attempt: failures,
        retryMs: Math.round(delay),
      };
    },
    delay: () => Math.max(0, retryAt - now()),
    description: () => (issue ? descriptions[issue] : ""),
  };
}
