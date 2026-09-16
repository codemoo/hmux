// Keep the deadline active through response body consumption, not just headers.
export async function withRequestDeadline<T>(
  operation: (signal: AbortSignal) => Promise<T>,
  parent?: AbortSignal,
  timeoutMs = 30_000,
): Promise<T> {
  const controller = new AbortController();
  const cancel = () => controller.abort(parent?.reason);
  if (parent?.aborted) cancel();
  else parent?.addEventListener("abort", cancel, { once: true });
  const timer = setTimeout(
    () =>
      controller.abort(new DOMException("Request timed out", "TimeoutError")),
    timeoutMs,
  );
  try {
    return await operation(controller.signal);
  } finally {
    clearTimeout(timer);
    parent?.removeEventListener("abort", cancel);
  }
}
