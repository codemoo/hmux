import type { Quota, Usage } from "./types";
// Home normalizes every quota as a fraction in [0,1]. The codex-lb pool's
// weighted weekly quota is authoritative; never sum per-account percentages.
export function remaining(q?: Quota, now = Date.now()): string {
  if (!q || !Number.isFinite(q.used_pct) || q.used_pct < 0 || q.used_pct > 1)
    return "—";
  if (
    q.resets_at &&
    (!Number.isFinite(Date.parse(q.resets_at)) ||
      Date.parse(q.resets_at) <= now)
  )
    return "—";
  return `${Math.round(100 * (1 - q.used_pct))}%`;
}
export function validUsage(u?: Usage, now = Date.now()): boolean {
  if (!u || u.status.stale) return false;
  const age =
    now - Date.parse(u.status.quota_observed_at || u.generated_at_utc);
  return Number.isFinite(age) && age >= -60000 && age < 1800000;
}
export function representative(u?: Usage, now = Date.now()): string {
  return validUsage(u, now) && u!.weekly_observed
    ? remaining(u!.weekly, now)
    : "—";
}

export function bedlCycle(
  usages: (Usage | undefined)[],
  now = Date.now(),
): number {
  const durations: Record<string, number> = {
    walk: 2.25,
    jog: 1.5,
    run: 1,
    fly: 0.65,
    rocket: 0.5,
  };
  const active = usages
    .filter((u) => {
      const age = now - Date.parse(u?.generated_at_utc || "");
      return u && !u.status.stale && age >= -60000 && age < 60000;
    })
    .map((u) => durations[u!.burn_state || ""] || 0)
    .filter((n) => n > 0);
  return active.length ? Math.max(0.7, Math.min(...active)) : 0;
}

export function diskCapacity(used?: number, total?: number): string {
  if (
    !Number.isSafeInteger(used) ||
    !Number.isSafeInteger(total) ||
    used! < 0 ||
    total! <= 0 ||
    used! > total!
  )
    return "—";
  const unit = total! >= 1e12 ? 1e12 : 1e9;
  const suffix = unit === 1e12 ? "TB" : "GB";
  return `${(used! / unit).toFixed(1)} / ${(total! / unit).toFixed(1)} ${suffix}`;
}

// Reset timestamps come from the selected source; never invent a weekly cycle.
export function weeklyResetLabel(resetsAt?: string, now = Date.now()): string {
  const reset = Date.parse(resetsAt || "");
  if (!Number.isFinite(reset)) return "리셋 시각 미제공";
  if (reset <= now) return "리셋 정보 갱신 대기";
  const minutes = Math.ceil((reset - now) / 60000);
  const days = Math.floor(minutes / 1440);
  const hours = Math.floor((minutes % 1440) / 60);
  const parts = [];
  if (days) parts.push(`${days}일`);
  if (hours) parts.push(`${hours}시간`);
  if (minutes % 60 || !parts.length) parts.push(`${minutes % 60}분`);
  return `리셋까지 ${parts.join(" ")}`;
}

export function codexPlanLabel(plan?: string): string | undefined {
  const labels: Record<string, string> = {
    free: "Free",
    plus: "Plus",
    pro: "Pro",
    team: "Team",
    business: "Business",
    enterprise: "Enterprise",
    edu: "Edu",
    go: "Go",
  };
  return plan ? labels[plan] : undefined;
}
