import type { Identity, Session } from "./types.ts";

const maxAge = 7 * 24 * 60 * 60 * 1000;
const maxBytes = 256 * 1024;
export function workspaceCacheKey(username: string, profile: string) {
  return `hmux.preview.v1.${JSON.stringify([username, profile])}`;
}
function identity(value: unknown): value is Identity {
  if (!value || typeof value !== "object") return false;
  const v = value as Identity;
  return (
    typeof v.id === "string" &&
    /^\$\d+$/.test(v.id) &&
    v.id.length < 32 &&
    Number.isSafeInteger(v.created_at) &&
    v.created_at > 0
  );
}
function label(value: unknown): value is string {
  return typeof value === "string" && value.length <= 256;
}
export function encodeWorkspaceCache(
  sessions: Session[],
  tabs: Identity[],
  active?: Identity,
  now = Date.now(),
): string {
  // Presentation only: never persist paths, terminal bytes, credentials or state.
  const result = JSON.stringify({
    version: 1,
    saved: now,
    sessions: sessions.slice(0, 256).map((s) => ({
      id: s.id,
      created_at: s.created_at,
      name: s.name.slice(0, 256),
      alias: s.alias?.slice(0, 256),
      hidden: s.hidden === true,
      runtime:
        s.runtime === "codex" || s.runtime === "claude" ? s.runtime : undefined,
    })),
    tabs: tabs
      .slice(0, 32)
      .filter(identity)
      .map(({ id, created_at }) => ({ id, created_at })),
    active: identity(active)
      ? { id: active.id, created_at: active.created_at }
      : undefined,
  });
  return result.length <= maxBytes ? result : "";
}
export function decodeWorkspaceCache(
  raw: string | null,
  now = Date.now(),
): { sessions: Session[]; tabs: Identity[]; active?: Identity } | undefined {
  if (!raw || raw.length > maxBytes) return;
  try {
    const v = JSON.parse(raw);
    if (
      v.version !== 1 ||
      !Number.isFinite(v.saved) ||
      v.saved > now ||
      now - v.saved > maxAge ||
      !Array.isArray(v.sessions) ||
      v.sessions.length > 256 ||
      !Array.isArray(v.tabs) ||
      v.tabs.length > 32
    )
      return;
    if (
      !v.sessions.every(
        (s: unknown) =>
          identity(s) &&
          label((s as Session).name) &&
          ((s as Session).alias === undefined || label((s as Session).alias)),
      ) ||
      !v.tabs.every(identity)
    )
      return;
    const sessions: Session[] = v.sessions.map((s: Session) => ({
      id: s.id,
      created_at: s.created_at,
      name: s.name,
      alias: s.alias,
      hidden: s.hidden === true,
      runtime:
        s.runtime === "codex" || s.runtime === "claude" ? s.runtime : undefined,
      window_count: 0,
      attached_clients: 0,
    }));
    const tabs: Identity[] = v.tabs.map((s: Identity) => ({
      id: s.id,
      created_at: s.created_at,
    }));
    return {
      sessions,
      tabs,
      active: identity(v.active)
        ? { id: v.active.id, created_at: v.active.created_at }
        : undefined,
    };
  } catch {
    return;
  }
}
