import { createTextFactory } from "./dom.ts";
import {
  diskCapacity,
  bedlCycle,
  remaining,
  representative,
  validUsage,
} from "./usage.ts";
import type { Snapshot } from "./types.ts";

type UsageFooterElements = {
  dog: HTMLElement;
  usageButton: HTMLElement;
  metrics: HTMLElement;
};

export function renderUsageFooter(
  snapshot: Snapshot,
  elements: UsageFooterElements,
) {
  const { dog, usageButton, metrics } = elements;
  const text = createTextFactory(usageButton.ownerDocument);
  const usage = snapshot.usage || {};
  const duration = snapshot.online ? bedlCycle([usage.claude, usage.codex]) : 0;
  dog.classList.toggle("running", duration > 0);
  dog.style.setProperty("--bedl-cycle", `${duration || 1}s`);
  usageButton.replaceChildren(
    text("span", "Claude", "provider-label provider-claude"),
    text("strong", representative(usage.claude)),
    text("i", ""),
    text("span", "Codex", "provider-label provider-codex"),
    text("strong", representative(usage.codex)),
  );
  usageButton.setAttribute(
    "aria-label",
    `계정별 사용량: Claude ${representative(usage.claude)}, Codex ${representative(usage.codex)}`,
  );
  usageButton.title = "계정별 사용량 보기";
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
  return box;
}
export function renderUsagePanel(
  body: HTMLElement,
  snapshot: Snapshot,
  metricsText: string,
) {
  const text = createTextFactory(body.ownerDocument);
  const panel = text("div", "", "usage-panel");
  const intro = text("div", "", "usage-intro");
  intro.append(
    text("span", "REMAINING CAPACITY", "usage-eyebrow"),
    text("p", "얼마나 더 사용할 수 있는지 한눈에 확인하세요."),
  );
  panel.append(intro);
  for (const provider of ["claude", "codex"]) {
    const u = snapshot.usage?.[provider];
    const section = text("section", "", "usage-provider");
    section.dataset.provider = provider;
    const header = text("div", "", "usage-provider-head");
    header.append(
      text("h3", provider === "claude" ? "Claude" : "Codex"),
      text(
        "span",
        provider === "claude" ? "활성 계정 기준" : "전체 풀 기준",
        "usage-badge",
      ),
    );
    section.append(header, usageGauge(text, "주간 잔여량", representative(u)));
    if (!validUsage(u)) {
      section.append(text("p", "최신 사용량을 확인하는 중입니다.", "muted"));
    } else {
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
        if (a.active) title.append(text("span", "현재 활성", "usage-active"));
        const gauges = text("div", "", "usage-account-gauges");
        gauges.append(
          usageGauge(
            text,
            "1주 잔여",
            remaining(a.status === "ok" ? a.seven_day : undefined),
          ),
          usageGauge(
            text,
            "5시간 잔여",
            remaining(a.status === "ok" ? a.five_hour : undefined),
          ),
        );
        row.append(title, gauges);
        accounts.append(row);
      }
      section.append(accounts);
      if (!u?.accounts?.length)
        section.append(text("p", "계정별 정보가 없습니다.", "muted"));
    }
    panel.append(section);
  }
  panel.append(
    text("p", metricsText || "Home · 사용량 대기 중", "usage-host muted"),
  );
  body.append(panel);
}
