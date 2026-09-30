import type { Conversation } from "./conversation-view.ts";
import { APIRequestError } from "./session-api.ts";
import { withRequestDeadline } from "./request-deadline.ts";

const ATTEMPTS = 3;
const DEADLINE_MS = 45_000;

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

function transient(error: unknown): boolean {
  if (error instanceof APIRequestError)
    return [429, 503, 504].includes(error.status);
  return (
    error instanceof TypeError ||
    (error instanceof Error && error.name === "TimeoutError")
  );
}

// One finite read cycle, owned by the current reader/tab/account. No mutations or
// provider restarts; each attempt asks Home to discover the exact binding again.
export async function loadConversation(options: {
  request: (signal: AbortSignal) => Promise<Conversation>;
  signal: AbortSignal;
  retrying: (attempt: number, maximum: number) => void;
  wait?: (ms: number, signal: AbortSignal) => Promise<void>;
}): Promise<Conversation> {
  return withRequestDeadline(
    async (signal) => {
      for (let attempt = 1; ; attempt++) {
        signal.throwIfAborted();
        let result: Conversation | undefined;
        let failure: unknown;
        try {
          result = await options.request(signal);
          signal.throwIfAborted();
          if (
            !result ||
            typeof result.status !== "string" ||
            !Array.isArray(result.messages) ||
            typeof result.truncated !== "boolean"
          )
            throw new Error("Invalid conversation response");
          if (result.status !== "unavailable" || attempt === ATTEMPTS)
            return result;
        } catch (error) {
          signal.throwIfAborted();
          if (!transient(error) || attempt === ATTEMPTS) throw error;
          failure = error;
        }
        const delay = Math.max(
          attempt * 1000,
          failure instanceof APIRequestError ? failure.retryAfterMs : 0,
        );
        // A long server backoff remains authoritative; do not hide it behind a
        // new short loop or wait indefinitely on this foreground reader.
        if (!Number.isFinite(delay) || delay > 10_000) {
          if (failure) throw failure;
          return result!;
        }
        options.retrying(attempt + 1, ATTEMPTS);
        await (options.wait || waitForRetry)(delay, signal);
      }
    },
    options.signal,
    DEADLINE_MS,
  );
}
