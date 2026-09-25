import { t } from "./i18n.ts";
import { withRequestDeadline } from "./request-deadline.ts";

// Only queries can be replayed. Creating sessions, auth changes and workspace
// mutations may already have succeeded when a response is interrupted.
function retryableQuery(path: string, body: unknown): boolean {
  if (path !== "/api/action" || !body || typeof body !== "object") return false;
  const request = body as { operation?: unknown; payload?: unknown };
  if (
    typeof request.operation === "string" &&
    ["conversation", "profiles", "providers"].includes(request.operation)
  )
    return true;
  if (request.operation !== "workspace") return false;
  const payload = request.payload;
  return (
    payload == null ||
    (typeof payload === "object" &&
      (payload as { change?: unknown }).change == null)
  );
}

function waitForRetry(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    signal.throwIfAborted();
    const cancel = () => {
      clearTimeout(timer);
      reject(signal.reason);
    };
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", cancel);
      resolve();
    }, ms);
    signal.addEventListener("abort", cancel, { once: true });
  });
}

// Every response, including errors and delayed bodies, belongs to one login.
export function createSessionAPI(options: {
  csrf: () => string;
  unauthorized: () => void;
  fetch?: typeof fetch;
  failure?: (event: {
    path: string;
    status: number;
    durationMs: number;
    reason: "network" | "timeout" | "http" | "protocol";
  }) => void;
}) {
  let scope = new AbortController();
  return {
    reset() {
      scope.abort();
      scope = new AbortController();
    },
    async request(path: string, body?: unknown, parent?: AbortSignal) {
      const owner = scope;
      const started = Date.now();
      let status = 0;
      const controller = new AbortController();
      const cancel = () => controller.abort();
      const signals = [owner.signal, parent].filter(
        (s): s is AbortSignal => !!s,
      );
      for (const signal of signals) {
        if (signal.aborted) cancel();
        else signal.addEventListener("abort", cancel, { once: true });
      }
      const check = (signal: AbortSignal) => {
        if (owner !== scope || controller.signal.aborted)
          throw new DOMException("Obsolete request", "AbortError");
        signal.throwIfAborted();
      };
      try {
        const serialized =
          body === undefined ? undefined : JSON.stringify(body);
        const query = retryableQuery(path, body);
        return await withRequestDeadline(async (signal) => {
          for (let attempt = 0; ; attempt++) {
            check(signal);
            status = 0;
            const response = await (options.fetch || fetch)(path, {
              method: body === undefined ? "GET" : "POST",
              headers:
                body === undefined
                  ? {}
                  : {
                      "Content-Type": "application/json",
                      "X-CSRF-Token": options.csrf(),
                    },
              body: serialized,
              credentials: "same-origin",
              cache: "no-store",
              signal,
            });
            check(signal);
            status = response.status || 200;
            if (!response.ok) {
              if (response.status === 401 && path !== "/api/login") {
                options.unauthorized();
                throw new DOMException("Authentication expired", "AbortError");
              }
              const message = await response.text();
              check(signal);
              if (query && response.status === 503 && attempt < 2) {
                const retryAfter = response.headers?.get("Retry-After");
                const seconds = retryAfter == null ? 1 : Number(retryAfter);
                // Never ignore a longer server backoff or extend the overall
                // request deadline. Cancel immediately on tab/account changes.
                if (Number.isFinite(seconds) && seconds >= 0 && seconds <= 2) {
                  await waitForRetry(Math.max(250, seconds * 1000), signal);
                  continue;
                }
              }
              throw new Error(
                (response.status === 503
                  ? t(
                      "Temporarily unavailable. Please try again shortly.",
                      "일시적으로 처리할 수 없습니다. 잠시 후 다시 시도해 주세요.",
                    )
                  : message.slice(0, 250)) ||
                  t(
                    "Could not complete the request.",
                    "요청을 완료하지 못했습니다.",
                  ),
              );
            }
            const value = await response.json();
            check(signal);
            return value;
          }
        }, controller.signal);
      } catch (error) {
        if (owner !== scope || controller.signal.aborted)
          throw new DOMException("Obsolete request", "AbortError");
        const name = error instanceof Error ? error.name : "";
        if (
          name !== "AbortError" &&
          path !== "/api/login" &&
          path !== "/api/diagnostics"
        ) {
          try {
            options.failure?.({
              path,
              status,
              durationMs: Date.now() - started,
              reason:
                name === "TimeoutError"
                  ? "timeout"
                  : status >= 400
                    ? "http"
                    : status
                      ? "protocol"
                      : "network",
            });
          } catch {
            /* Diagnostics must not change request behavior. */
          }
        }
        throw error;
      } finally {
        for (const signal of signals)
          signal.removeEventListener("abort", cancel);
      }
    },
  };
}
