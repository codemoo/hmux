import { createTextFactory } from "./dom.ts";
import {
  diskCapacity,
  weeklyResetLabel,
  codexPlanLabel,
  bedlCycle,
  remaining,
  representative,
  validUsage,
  showFiveHourSummary,
  observationLabel,
  validAccountMeasurement,
} from "./usage.ts";
import type { Snapshot } from "./types.ts";
import {
  defaultUsagePreferences,
  selectedUsage,
  usageSourceLabel,
  type UsagePreferences,
} from "./usage-preferences.ts";

type UsageFooterElements = {
  dog: HTMLElement;
  usageButton: HTMLElement;
  metrics: HTMLElement;
};

export function renderUsageFooter(
  snapshot: Snapshot,
  elements: UsageFooterElements,
  preferences: UsagePreferences = defaultUsagePreferences(),
) {
  const { dog, usageButton, metrics } = elements;
  const text = createTextFactory(usageButton.ownerDocument);
  const usage = selectedUsage(snapshot, preferences);
  const duration = snapshot.online ? bedlCycle([usage.claude, usage.codex]) : 0;
  dog.classList.toggle("running", duration > 0);
  dog.style.setProperty("--bedl-cycle", `${duration || 1}s`);
  usageButton.replaceChildren();
  const descriptions: string[] = [];
  for (const provider of ["codex", "claude"] as const) {
    if (!preferences[provider].enabled) continue;
    if (descriptions.length) usageButton.append(text("i"));
    const label = usageSourceLabel(provider, preferences[provider].source);
    usageButton.append(
      text(
        "span",
        provider === "codex" ? "Codex" : "Claude",
        `provider-label provider-${provider}`,
      ),
      text("strong", representative(usage[provider])),
    );
    descriptions.push(`${label} ${representative(usage[provider])}`);
  }
  usageButton.hidden = descriptions.length === 0;
  dog.hidden = descriptions.length === 0;
  usageButton.setAttribute(
    "aria-label",
    `계정별 사용량: ${descriptions.join(", ")}`,
  );
  usageButton.title = descriptions.join(" · ");
  const m = snapshot.catalog?.host_metrics;
  metrics.textContent =
    snapshot.online && m && Date.now() - Date.parse(m.observed_at) < 20000
      ? `Home · CPU ${m.cpu_percent?.toFixed(0) ?? "—"}% · GPU ${m.gpu_percent?.toFixed(0) ?? "—"}% · RAM ${m.memory_total_bytes && m.memory_used_bytes !== undefined ? ((100 * m.memory_used_bytes) / m.memory_total_bytes).toFixed(0) + "%" : "—"} · Disk ${diskCapacity(m.disk_used_bytes, m.disk_total_bytes)}`
      : "Home · 사용량 대기 중";
  metrics.title = metrics.textContent || "";
}
function usageGauge(
  text: ReturnType<typeof createTextFactory>,
  label: string,
  value: string,
  reset?: { at?: string },
) {
  const box = text("div", "", "usage-gauge");
  const known = value !== "—";
  const percent = known ? Number.parseInt(value, 10) : 0;
  const heading = text("div", "", "usage-gauge-heading");
  heading.append(text("span", label), text("strong", known ? value : "—"));
  const track = text("div", "", "usage-track");
  if (known) {
    track.setAttribute("role", "meter");
    track.setAttribute("aria-label", `${label} 잔여량`);
    track.setAttribute("aria-valuemin", "0");
    track.setAttribute("aria-valuemax", "100");
    track.setAttribute("aria-valuenow", String(percent));
    track.dataset.level =
      percent <= 15 ? "low" : percent <= 35 ? "medium" : "high";
    const fill = text("span", "", "usage-fill");
    fill.style.width = `${percent}%`;
    track.append(fill);
  } else {
    track.classList.add("unknown");
    track.setAttribute("aria-label", `${label} 정보 없음`);
  }
  box.append(heading, track);
  if (reset?.at && Number.isFinite(Date.parse(reset.at))) {
    const countdown = text("p", weeklyResetLabel(reset.at), "usage-reset");
    countdown.title = new Date(reset.at).toLocaleString();

    box.append(countdown);
  }
  return box;
}
export function renderUsagePanel(
  body: HTMLElement,
  snapshot: Snapshot,
  metricsText: string,
  preferences: UsagePreferences = defaultUsagePreferences(),
) {
  const text = createTextFactory(body.ownerDocument);
  const panel = text("div", "", "usage-panel");
  const intro = text("div", "", "usage-intro");
  intro.append(text("p", "계정별 남은 한도와 초기화 일정"));
  const providers = text("div", "", "usage-providers");
  panel.append(intro, providers);
  const usage = selectedUsage(snapshot, preferences);
  for (const provider of ["codex", "claude"] as const) {
    if (!preferences[provider].enabled) continue;
    const u = usage[provider];
    const section = text("section", "", "usage-provider");
    section.dataset.provider = provider;
    section.setAttribute(
      "aria-label",
      provider === "codex" ? "Codex 사용량" : "Claude 사용량",
    );
    const overview = text("div", "", "usage-provider-overview");
    const details = text("div", "", "usage-provider-details");
    const header = text("div", "", "usage-provider-head");
    const heading = text("div", "", "usage-provider-title");
    heading.append(text("h3", provider === "codex" ? "Codex" : "Claude"));
    if (u?.accounts?.length) {
      const active = u.accounts.filter((account) => account.active).length;
      heading.append(
        text(
          "span",
          `${u.accounts.length}개 계정 · ${active}개 활성`,
          "usage-provider-meta",
        ),
      );
    }
    const badges = text("div", "", "usage-provider-badges");
    badges.append(
      text(
        "span",
        usageSourceLabel(provider, preferences[provider].source),
        "usage-badge",
      ),
    );
    const plan =
      provider === "codex" ? codexPlanLabel(u?.plan_type) : undefined;
    if (plan) badges.append(text("span", plan, "usage-badge usage-plan"));
    header.append(heading, badges);
    const observed = observationLabel(u?.status.quota_observed_at);

    if (u?.status.stale && validUsage(u)) {
      const delayed = text("span", "갱신 지연", "usage-badge");
      delayed.title =
        "새 조회가 지연되어 마지막으로 확인한 사용량을 표시합니다.";
      badges.append(delayed);
    }
    const summary = text("div", "", "usage-account-gauges usage-summary");
    summary.append(
      usageGauge(text, "주간", representative(u), {
        at: u?.weekly_observed ? u.weekly.resets_at : undefined,
      }),
    );
    if (showFiveHourSummary(u))
      summary.append(
        usageGauge(
          text,
          "5시간",
          validUsage(u) ? remaining(u?.rolling_5h) : "—",
        ),
      );
    overview.append(
      header,
      text(
        "p",
        provider === "codex" && preferences.codex.source === "codex-lb"
          ? "통합 잔여량"
          : "현재 계정 잔여량",
        "usage-section-label",
      ),
      summary,
    );
    if (observed) overview.append(text("p", observed, "usage-observation"));
    if (!validUsage(u)) {
      overview.append(
        text("p", "현재 사용량을 확인할 수 없습니다.", "usage-message muted"),
      );
    }
    section.append(overview);
    {
      const accounts = text("div", "", "usage-accounts");
      for (const a of u?.accounts || []) {
        const row = text("div", "", "usage-account");
        const info = text("div", "", "usage-account-info");
        const title = text("div", "", "usage-account-head");
        const badges = text("div", "", "usage-account-badges");
        title.append(
          text(
            "strong",
            (provider === "claude" ? a.email : a.display_name) ||
              `계정 ${a.number}`,
          ),
        );
        const accountPlan =
          provider === "codex" ? codexPlanLabel(a.plan_type) : undefined;
        if (accountPlan)
          badges.append(text("span", accountPlan, "usage-badge usage-plan"));
        if (a.active) badges.append(text("span", "활성", "usage-active"));
        if (a.status !== "ok")
          badges.append(
            text("span", accountStatus(a.status), "usage-account-status"),
          );
        // codex-lb last_refresh_at describes account credential refresh,
        // not this account-list observation. Pool quota is fetched separately.
        const observedAt =
          provider === "codex" && preferences.codex.source === "codex-lb"
            ? u?.accounts_updated_at
            : a.last_refresh_at ||
              u?.status.quota_observed_at ||
              (a.status === "ok" ? u?.generated_at_utc : undefined);
        const fresh = validAccountMeasurement(observedAt);
        const updated = observationLabel(observedAt);
        const timestamp = updated
          ? text("p", updated, "usage-account-updated")
          : undefined;
        const gauges = text("div", "", "usage-account-gauges");
        gauges.append(
          usageGauge(text, "주간", remaining(fresh ? a.seven_day : undefined), {
            at: a.seven_day?.resets_at,
          }),
        );
        if (a.five_hour)
          gauges.append(
            usageGauge(
              text,
              "5시간",
              remaining(fresh ? a.five_hour : undefined),
            ),
          );
        info.append(title, badges);
        if (timestamp) info.append(timestamp);
        row.append(info, gauges);
        accounts.append(row);
      }
      if (u?.accounts?.length) {
        details.append(
          text(
            "h4",
            "계정별 잔여량",
            "usage-section-label usage-accounts-label",
          ),
          accounts,
        );
      }
      if (!u?.accounts?.length && preferences[provider].source !== "cli")
        details.append(
          text("p", "연결된 계정 정보가 없습니다.", "usage-message muted"),
        );
    }
    if (details.childElementCount) section.append(details);
    providers.append(section);
  }
  if (!providers.childElementCount)
    providers.append(
      text(
        "p",
        "설정의 사용량 항목에서 표시할 서비스를 선택하세요.",
        "usage-message muted",
      ),
    );
  panel.append(
    text("p", metricsText || "Home · 사용량 대기 중", "usage-host muted"),
  );
  body.append(panel);
}

function accountStatus(status: string) {
  const labels: Record<string, string> = {
    keychain_unavailable: "키체인 확인 필요",
    stale: "업데이트 필요",
    token_expired: "갱신 대기",
    no_credentials: "로그인 정보 없음",
    rate_limited: "조회 일시 제한",
    relogin_required: "재로그인 필요",
    disabled: "사용 안 함",
    paused: "일시 중지",
    reauth_required: "재로그인 필요",
    unavailable: "사용량 확인 불가",
  };
  return labels[status] || "정보 없음";
}
