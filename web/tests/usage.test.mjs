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
