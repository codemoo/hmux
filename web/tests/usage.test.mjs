import test from "node:test";
import assert from "node:assert/strict";
import { remaining, representative, validUsage } from "../src/usage.ts";
const now = Date.parse("2026-09-09T00:00:00Z");
const snapshot = {
  provider: "codex",
  generated_at_utc: new Date(now).toISOString(),
  weekly_observed: true,
  weekly: { used_pct: 0.35 },
  status: { stale: false },
  accounts: [
    { seven_day: { used_pct: 0.9 } },
    { seven_day: { used_pct: 0.1 } },
  ],
};
test("Home fractional weekly pool is used without summing accounts", () =>
  assert.equal(representative(snapshot, now), "65%"));
test("missing quota never displays free capacity", () =>
  assert.equal(
    representative({ ...snapshot, weekly_observed: false }, now),
    "—",
  ));
test("stale, future and expired observations are unknown", () => {
  assert.equal(
    validUsage({ ...snapshot, status: { stale: true } }, now),
    false,
  );
  assert.equal(
    validUsage(
      { ...snapshot, generated_at_utc: new Date(now + 120000).toISOString() },
      now,
    ),
    false,
  );
  assert.equal(
    validUsage(
      { ...snapshot, generated_at_utc: new Date(now - 1800001).toISOString() },
      now,
    ),
    false,
  );
});
test("window reset and invalid quota cannot show misleading 100%", () => {
  assert.equal(
    remaining({ used_pct: 0, resets_at: new Date(now - 1).toISOString() }, now),
    "—",
  );
  assert.equal(remaining({ used_pct: NaN }, now), "—");
  assert.equal(remaining({ used_pct: 25 }, now), "—");
  assert.equal(remaining({ used_pct: 1 }, now), "0%");
});

import { diskCapacity } from "../src/usage.ts";
test("disk capacity displays used/total with units, never a percentage", () => {
  assert.equal(diskCapacity(250e9, 500e9), "250.0 / 500.0 GB");
  assert.equal(diskCapacity(750e9, 2e12), "0.8 / 2.0 TB");
  assert.equal(diskCapacity(0, 500e9), "0.0 / 500.0 GB");
  assert.equal(diskCapacity(undefined, 500e9), "—");
  assert.equal(diskCapacity(600e9, 500e9), "—");
  assert.equal(diskCapacity(1, 0), "—");
});

import { weeklyResetLabel, codexPlanLabel } from "../src/usage.ts";
test("weekly reset countdown uses source time with no invented reset", () => {
  assert.equal(
    weeklyResetLabel(
      new Date(now + (2 * 1440 + 3 * 60 + 4) * 60000).toISOString(),
      now,
    ),
    "2일 3시간 후 초기화",
  );
  assert.equal(
    weeklyResetLabel(new Date(now + 1000).toISOString(), now),
    "1분 후 초기화",
  );
  assert.equal(
    weeklyResetLabel(new Date(now).toISOString(), now),
    "초기화 확인 중",
  );
  assert.equal(weeklyResetLabel(undefined, now), "초기화 일정 없음");
  assert.equal(weeklyResetLabel("invalid", now), "초기화 일정 없음");
});
test("Codex plan labels distinguish verified Plus and Pro without inference", () => {
  assert.equal(codexPlanLabel("plus"), "Plus");
  assert.equal(codexPlanLabel("pro"), "Pro");
  assert.equal(codexPlanLabel(undefined), undefined);
  assert.equal(codexPlanLabel("unknown"), undefined);
});

import { showFiveHourSummary } from "../src/usage.ts";
test("Codex pool hides 5h when any active account has no 5h window", () => {
  const limited = { active: true, five_hour: { used_pct: 0 } };
  const noLimit = { active: true };
  const usage = {
    provider: "codex",
    rolling_5h_observed: true,
    accounts: [limited, noLimit],
  };
  assert.equal(showFiveHourSummary(usage), false);
  assert.equal(
    showFiveHourSummary({
      ...usage,
      accounts: [limited, { ...noLimit, active: false }],
    }),
    true,
  );
  assert.equal(showFiveHourSummary({ ...usage, accounts: [limited] }), true);
  assert.equal(showFiveHourSummary({ ...usage, accounts: undefined }), true);
  assert.equal(
    showFiveHourSummary({
      ...usage,
      rolling_5h_observed: false,
      accounts: [limited],
    }),
    false,
  );
  assert.equal(showFiveHourSummary({ ...usage, provider: "claude" }), true);
});

test("recent measured quota survives refresh failure but not expired observation", () => {
  const source = {
    ...snapshot,
    status: {
      state: "networkError",
      stale: true,
      quota_observed_at: new Date(now - 180000).toISOString(),
    },
  };
  assert.equal(representative(source, now), "65%");
  assert.equal(
    representative(
      {
        ...source,
        status: {
          ...source.status,
          quota_observed_at: new Date(now - 1800000).toISOString(),
        },
      },
      now,
    ),
    "—",
  );
  assert.equal(
    representative({ ...source, status: { stale: true } }, now),
    "—",
  );
  assert.equal(
    representative(
      {
        ...source,
        weekly: { used_pct: 0.35, resets_at: new Date(now - 1).toISOString() },
      },
      now,
    ),
    "—",
  );
});
