import { t, msg, bindAttribute, bindText } from "./i18n.ts";
import { createTextFactory } from "./dom.ts";
import type { Usage, Snapshot } from "./types.ts";
import { validUsage } from "./usage.ts";

export type UsagePreferences = {
  version: 1;
  revision: number;
  claude: { enabled: boolean; source: "cli" | "cswap" };
  codex: { enabled: boolean; source: "cli" | "codex-lb" };
};
export function defaultUsagePreferences(): UsagePreferences {
  return {
    version: 1,
    revision: 0,
    claude: { enabled: true, source: "cswap" },
    codex: { enabled: true, source: "codex-lb" },
  };
}
export function parseUsagePreferences(
  value: unknown,
): UsagePreferences | undefined {
  if (!value || typeof value !== "object") return;
  const v = value as UsagePreferences;
  if (
    v.version !== 1 ||
    !Number.isSafeInteger(v.revision) ||
    v.revision < 0 ||
    typeof v.claude?.enabled !== "boolean" ||
    typeof v.codex?.enabled !== "boolean" ||
    !["cli", "cswap"].includes(v.claude.source) ||
    !["cli", "codex-lb"].includes(v.codex.source)
  )
    return;
  return {
    version: 1,
    revision: v.revision,
    claude: { enabled: v.claude.enabled, source: v.claude.source },
    codex: { enabled: v.codex.enabled, source: v.codex.source },
  };
}
export function usageSourceLabel(provider: "claude" | "codex", source: string) {
  return source === "cswap"
    ? "cswap"
    : source === "codex-lb"
      ? "codex-lb"
      : provider === "claude"
        ? "Claude CLI"
        : "Codex CLI";
}
// A pooled source (cswap / codex-lb) that is unavailable on this Home falls back
// to the CLI source when that one has a valid measurement, so hosts without
// those tools still show usage. Labels follow the source actually shown.
export function effectiveUsageSource(
  snapshot: Snapshot,
  preferences: UsagePreferences,
  provider: "claude" | "codex",
): string {
  const choice = preferences[provider].source;
  const sources = snapshot.usage?.[provider]?.sources;
  const selected = sources?.[choice];
  const unavailable =
    !selected ||
    (selected.status?.state !== undefined &&
      selected.status.state !== "ok" &&
      !validUsage(selected));
  const cli = sources?.cli;
  if (choice !== "cli" && unavailable && cli?.status && validUsage(cli))
    return "cli";
  return choice;
}
export function selectedUsage(
  snapshot: Snapshot,
  preferences: UsagePreferences,
) {
  const result: Partial<Record<"claude" | "codex", Usage>> = {};
  for (const provider of ["codex", "claude"] as const) {
    if (preferences[provider].enabled)
      result[provider] =
        snapshot.usage?.[provider]?.sources?.[
          effectiveUsageSource(snapshot, preferences, provider)
        ];
  }
  return result;
}

type API = (
  path: string,
  body?: unknown,
  signal?: AbortSignal,
) => Promise<unknown>;
export function installUsagePreferences(
  root: HTMLElement,
  api: API,
  onChanged: (value: UsagePreferences) => void,
) {
  const controller = new AbortController();
  const make = createTextFactory(root.ownerDocument);
  let current: UsagePreferences | undefined;
  let busy = false;
  const rows: {
    provider: "claude" | "codex";
    toggle: HTMLButtonElement;
    select: HTMLSelectElement;
  }[] = [];
  root.append(
    make("h3", msg("Show usage", "사용량 표시")),
    make(
      "p",
      msg(
        "Choose which services to show and how to check usage.",
        "표시할 서비스와 조회 방식을 선택하세요.",
      ),
      "muted",
    ),
  );
  for (const provider of ["codex", "claude"] as const) {
    const row = make("div", "", "usage-preference-row");
    const heading = make("div", "", "security-toggle-row");
    const name = provider === "claude" ? "Claude" : "Codex";
    const toggle = make("button", "", "security-switch");
    toggle.type = "button";
    toggle.setAttribute("role", "switch");
    bindAttribute(toggle, "aria-label", () =>
      t(`${name} usage display`, `${name} 사용량 표시`),
    );
    toggle.setAttribute("aria-checked", "false");
    toggle.append(make("span"));
    heading.append(make("strong", name), toggle);
    const label = make("label", msg("Usage source", "조회 방식"));
    const select = make("select");
    bindAttribute(select, "aria-label", () =>
      t(`${name} usage source`, `${name} 사용량 소스`),
    );
    for (const source of [
      "cli",
      provider === "claude" ? "cswap" : "codex-lb",
    ]) {
      const option = make("option", usageSourceLabel(provider, source));
      option.value = source;
      select.append(option);
    }
    label.append(select);
    row.append(heading, label);
    root.append(row);
    rows.push({ provider, toggle, select });
    toggle.onclick = () => {
      if (current && !busy) {
        const next = parseUsagePreferences(current)!;
        next[provider].enabled = !next[provider].enabled;
        void save(next);
      }
    };
    select.onchange = () => {
      if (current && !busy) {
        const next = parseUsagePreferences({
          ...current,
          [provider]: { ...current[provider], source: select.value },
        });
        if (next) void save(next);
      }
    };
  }
  const status = make("p", "", "muted");
  status.setAttribute("role", "status");
  const reload = make(
    "button",
    msg("Reload", "다시 불러오기"),
    "subtle-button",
  );
  reload.type = "button";
  reload.hidden = true;
  reload.onclick = () => void load();
  root.append(status, reload);
  function render() {
    for (const { provider, toggle, select } of rows) {
      toggle.disabled = busy || !current;
      select.disabled = busy || !current || !current[provider].enabled;
      toggle.setAttribute(
        "aria-checked",
        String(current?.[provider].enabled ?? false),
      );
      if (current) select.value = current[provider].source;
    }
    reload.disabled = busy;
  }
  function accept(value: unknown) {
    const parsed = parseUsagePreferences(value);
    if (!parsed)
      throw new Error(
        t(
          "Could not verify usage settings.",
          "사용량 설정을 확인하지 못했습니다.",
        ),
      );
    current = parsed;
    onChanged(parsed);
  }
  async function load() {
    if (busy || controller.signal.aborted) return;
    busy = true;
    reload.hidden = true;
    bindText(status, msg("Loading…", "불러오는 중…"));
    render();
    try {
      const value = await api(
        "/api/account/usage",
        undefined,
        controller.signal,
      );
      if (controller.signal.aborted) return;
      accept(value);
      status.textContent = "";
    } catch {
      if (!controller.signal.aborted) {
        bindText(
          status,
          msg("Could not load settings.", "설정을 불러오지 못했습니다."),
        );
        reload.hidden = false;
      }
    } finally {
      busy = false;
      if (!controller.signal.aborted) render();
    }
  }
  async function save(next: UsagePreferences) {
    busy = true;
    reload.hidden = true;
    bindText(status, msg("Saving…", "저장 중…"));
    render();
    try {
      const value = await api("/api/account/usage", next, controller.signal);
      if (controller.signal.aborted) return;
      accept(value);
      bindText(status, msg("Saved.", "저장했습니다."));
    } catch (error) {
      if (!controller.signal.aborted) {
        status.textContent =
          error instanceof Error
            ? error.message
            : t("Could not save settings.", "설정을 저장하지 못했습니다.");
        reload.hidden = false;
      }
    } finally {
      busy = false;
      if (!controller.signal.aborted) render();
    }
  }
  void load();
  return () => controller.abort();
}
