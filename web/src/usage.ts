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
  if (!u) return false;
  // A refresh failure does not invalidate a recent source measurement.
  // Stale responses must carry their actual measurement time, never use the
  // freshly generated transport timestamp as evidence of fresh quota.
  if (u.status.stale && !u.status.quota_observed_at) return false;
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
  if (!Number.isFinite(reset)) return "초기화 일정 없음";
  if (reset <= now) return "초기화 확인 중";
  const minutes = Math.ceil((reset - now) / 60000);
  const days = Math.floor(minutes / 1440);
  const hours = Math.floor((minutes % 1440) / 60);
  const parts = [];
  if (days) parts.push(`${days}일`);
  if (hours) parts.push(`${hours}시간`);
  if (!days && (minutes % 60 || !parts.length)) parts.push(`${minutes % 60}분`);
  return `${parts.join(" ")} 후 초기화`;
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

export function showFiveHourSummary(usage?: Usage): boolean {
  if (!usage?.rolling_5h_observed) return false;
  return (
    usage.provider !== "codex" ||
    !usage.accounts?.some((account) => account.active && !account.five_hour)
  );
}

export function observationLabel(
  observedAt?: string,
  now = Date.now(),
): string | undefined {
  const age = now - Date.parse(observedAt || "");
  if (!Number.isFinite(age) || age < -60000) return;
  const minutes = Math.floor(Math.max(0, age) / 60000);
  return minutes < 1
    ? "방금 업데이트"
    : minutes < 60
      ? `${minutes}분 전 업데이트`
      : `${Math.floor(minutes / 60)}시간 전 업데이트`;
}

export function validAccountMeasurement(
  observedAt?: string,
  now = Date.now(),
): boolean {
  const age = now - Date.parse(observedAt || "");
  return Number.isFinite(age) && age >= -60000 && age < 1800000;
}
