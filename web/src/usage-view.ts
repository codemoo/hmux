import { createTextFactory } from "./dom.ts";
import {
  diskCapacity,
  weeklyResetLabel,
  codexPlanLabel,
  bedlCycle,
  remaining,
  representative,
  validUsage,
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
  for (const provider of ["claude", "codex"] as const) {
    if (!preferences[provider].enabled) continue;
    if (descriptions.length) usageButton.append(text("i"));
    const label = usageSourceLabel(provider, preferences[provider].source);
    usageButton.append(
      text("span", label, `provider-label provider-${provider}`),
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
  reset?: { at?: string; stale?: boolean },
) {
  const box = text("div", "", "usage-gauge");
  const known = value !== "—";
  const percent = known ? Number.parseInt(value, 10) : 0;
  const heading = text("div", "", "usage-gauge-heading");
  heading.append(
    text("span", label),
    text("strong", known ? value : "확인 대기"),
  );
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
  } else track.classList.add("unknown");
  box.append(heading, track);
  if (reset) {
    const countdown = text("p", weeklyResetLabel(reset.at), "usage-reset");
    if (reset.at && Number.isFinite(Date.parse(reset.at))) {
      countdown.title = new Date(reset.at).toLocaleString();
      if (reset.stale) countdown.append(text("span", " · 최근 조회 기준"));
    }
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
  intro.append(
    text("span", "REMAINING CAPACITY", "usage-eyebrow"),
    text("p", "얼마나 더 사용할 수 있는지 한눈에 확인하세요."),
  );
  panel.append(intro);
  const usage = selectedUsage(snapshot, preferences);
  for (const provider of ["claude", "codex"] as const) {
    if (!preferences[provider].enabled) continue;
    const u = usage[provider];
    const section = text("section", "", "usage-provider");
    section.dataset.provider = provider;
    const header = text("div", "", "usage-provider-head");
    header.append(
      text("h3", provider === "claude" ? "Claude" : "Codex"),
      text(
        "span",
        usageSourceLabel(provider, preferences[provider].source),
        "usage-badge",
      ),
    );
    const plan =
      provider === "codex" ? codexPlanLabel(u?.plan_type) : undefined;
    if (plan) header.append(text("span", plan, "usage-badge usage-plan"));
    const summary = text("div", "", "usage-account-gauges");
    summary.append(
      usageGauge(text, "주간 잔여량", representative(u), {
        at: u?.weekly_observed ? u.weekly.resets_at : undefined,
        stale: !validUsage(u),
      }),
    );
    if (u?.rolling_5h_observed)
      summary.append(
        usageGauge(
          text,
          "5시간 잔여량",
          validUsage(u) ? remaining(u.rolling_5h) : "—",
        ),
      );
    section.append(header, summary);
    if (!validUsage(u)) {
      section.append(text("p", "최신 사용량을 확인하는 중입니다.", "muted"));
    }
    {
      const accounts = text("div", "", "usage-accounts");
      for (const a of u?.accounts || []) {
        const row = text("div", "", "usage-account");
        const title = text("div", "", "usage-account-head");
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
          title.append(text("span", accountPlan, "usage-badge usage-plan"));
        if (a.active) title.append(text("span", "현재 활성", "usage-active"));
        if (a.status !== "ok")
          title.append(text("span", accountStatus(a.status), "muted"));
        const age =
          Date.now() -
          Date.parse(
            a.last_refresh_at ||
              u?.status.quota_observed_at ||
              u?.generated_at_utc ||
              "",
          );
        const fresh =
          Number.isFinite(age) &&
          age >= -60000 &&
          age < 1800000 &&
          a.status === "ok";
        const gauges = text("div", "", "usage-account-gauges");
        gauges.append(
          usageGauge(
            text,
            "1주 잔여",
            remaining(fresh ? a.seven_day : undefined),
            { at: a.seven_day?.resets_at, stale: !fresh },
          ),
        );
        if (a.five_hour)
          gauges.append(
            usageGauge(
              text,
              "5시간 잔여",
              remaining(fresh ? a.five_hour : undefined),
            ),
          );
        row.append(title, gauges);
        accounts.append(row);
      }
      section.append(accounts);
      if (!u?.accounts?.length && preferences[provider].source !== "cli")
        section.append(
          text("p", "선택한 소스의 계정 정보를 기다리고 있습니다.", "muted"),
        );
    }
    panel.append(section);
  }
  panel.append(
    text("p", metricsText || "Home · 사용량 대기 중", "usage-host muted"),
  );
  body.append(panel);
}

function accountStatus(status: string) {
  const labels: Record<string, string> = {
    keychain_unavailable: "키체인 접근 확인 필요",
    stale: "최근 정보 확인 필요",
    token_expired: "로그인 갱신 필요",
    no_credentials: "로그인 정보 없음",
    rate_limited: "조회 제한 · 잠시 후 확인",
    relogin_required: "다시 로그인 필요",
    disabled: "사용 안 함",
    paused: "사용 일시중지",
    reauth_required: "다시 로그인 필요",
    unavailable: "사용량 확인 불가",
  };
  return labels[status] || "사용량 확인 대기";
}
