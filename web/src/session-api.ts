import { t } from "./i18n.ts";
import { withRequestDeadline } from "./request-deadline.ts";

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
        return await withRequestDeadline(async (signal) => {
          check(signal);
          const response = await (options.fetch || fetch)(path, {
            method: body === undefined ? "GET" : "POST",
            headers:
              body === undefined
                ? {}
                : {
                    "Content-Type": "application/json",
                    "X-CSRF-Token": options.csrf(),
                  },
            body: body === undefined ? undefined : JSON.stringify(body),
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
            throw new Error(
              message.slice(0, 250) ||
                t(
                  "Could not complete the request.",
                  "요청을 완료하지 못했습니다.",
                ),
            );
          }
          const value = await response.json();
          check(signal);
          return value;
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
