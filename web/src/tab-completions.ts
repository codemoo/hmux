import type { Identity } from "./types.ts";

export type CompletionEntry = {
  session: Identity;
  event_id: string;
  completed_at: string;
  unread: boolean;
};
export type CompletionState = {
  version: 1;
  sequence: number;
  read_revision: number;
  entries: CompletionEntry[];
};
const key = (id: Identity) => `${id.id}:${id.created_at}`;
export function parseCompletionState(
  value: unknown,
): CompletionState | undefined {
  if (!value || typeof value !== "object") return;
  const state = value as CompletionState;
  if (
    state.version !== 1 ||
    !Number.isSafeInteger(state.sequence) ||
    state.sequence < 0 ||
    !Number.isSafeInteger(state.read_revision) ||
    state.read_revision < 0 ||
    !Array.isArray(state.entries) ||
    state.entries.length > 256
  )
    return;
  const seen = new Set<string>();
  for (const entry of state.entries) {
    if (
      !entry?.session ||
      !/^\$[0-9]{1,12}$/.test(entry.session.id) ||
      !Number.isSafeInteger(entry.session.created_at) ||
      entry.session.created_at < 1 ||
      typeof entry.event_id !== "string" ||
      !/^[a-fA-F0-9]{64}$/.test(entry.event_id) ||
      typeof entry.completed_at !== "string" ||
      entry.completed_at.length > 64 ||
      !Number.isFinite(Date.parse(entry.completed_at)) ||
      typeof entry.unread !== "boolean" ||
      seen.has(key(entry.session))
    )
      return;
    seen.add(key(entry.session));
  }
  return state;
}
type API = (
  path: string,
  body: unknown,
  signal: AbortSignal,
) => Promise<unknown>;
export function createTabCompletions(api: API, changed: () => void) {
  let current: CompletionState | undefined;
  let disposed = false;
  const pending = new Map<string, AbortController>();
  const apply = (value: unknown) => {
    if (disposed) return;
    const next = parseCompletionState(value);
    if (
      !next ||
      (current &&
        (next.sequence < current.sequence ||
          next.read_revision < current.read_revision))
    )
      return;
    current = next;
    changed();
  };
  const entry = (session: Identity) =>
    current?.entries.find((e) => key(e.session) === key(session));
  return {
    apply,
    entry,
    unread: (session: Identity) => entry(session)?.unread === true,
    // Only the renderer can establish readiness; selection/cached tabs alone
    // are insufficient. Never optimistically remove a shared unread marker.
    async read(view: {
      session: Identity;
      visible: boolean;
      focused: boolean;
      ready: boolean;
      event_id?: string;
    }) {
      const latest = entry(view.session);
      if (
        disposed ||
        !view.visible ||
        !view.focused ||
        !view.ready ||
        !latest?.unread ||
        (view.event_id !== undefined && view.event_id !== latest.event_id)
      )
        return;
      const token = `${key(view.session)}:${latest.event_id}`;
      if (pending.has(token) || pending.size >= 4) return;
      const controller = new AbortController();
      pending.set(token, controller);
      try {
        apply(
          await api(
            "/api/completions/read",
            { session: { ...latest.session }, event_id: latest.event_id },
            controller.signal,
          ),
        );
      } catch {
        // Existing state polling retries after network/admission failures and
        // fetches a newer event after a CAS conflict; there is no extra timer.
      } finally {
        if (pending.get(token) === controller) pending.delete(token);
      }
    },
    dispose() {
      disposed = true;
      for (const request of pending.values()) request.abort();
      pending.clear();
      current = undefined;
    },
  };
}
