import test from "node:test";
import assert from "node:assert/strict";
import {
  defaultUsagePreferences,
  effectiveUsageSource,
  parseUsagePreferences,
  selectedUsage,
} from "../src/usage-preferences.ts";

test("each provider selects only its explicit source and respects visibility", () => {
  const preferences = defaultUsagePreferences();
  const cli = { provider: "claude", weekly: { used_pct: 0.9 } };
  const cswap = { provider: "claude", weekly: { used_pct: 0.1 } };
  const lb = { provider: "codex", weekly: { used_pct: 0.3 } };
  const snapshot = {
    usage: {
      claude: { sources: { cli, cswap } },
      codex: { sources: { "codex-lb": lb } },
    },
  };
  assert.deepEqual(selectedUsage(snapshot, preferences), {
    claude: cswap,
    codex: lb,
  });
  preferences.claude.source = "cli";
  preferences.codex.enabled = false;
  assert.deepEqual(selectedUsage(snapshot, preferences), { claude: cli });
  preferences.claude.enabled = false;
  assert.deepEqual(selectedUsage(snapshot, preferences), {});
});
test("missing sources never relabel legacy or pooled quota as CLI data", () => {
  const preferences = defaultUsagePreferences();
  preferences.codex.source = "cli";
  assert.equal(
    selectedUsage(
      { usage: { codex: { weekly: { used_pct: 0.2 } } } },
      preferences,
    ).codex,
    undefined,
  );
  assert.equal(
    selectedUsage(
      {
        usage: {
          codex: { sources: { "codex-lb": { weekly: { used_pct: 0.2 } } } },
        },
      },
      preferences,
    ).codex,
    undefined,
  );
});
test("settings validator rejects malformed cross-provider selections", () => {
  const value = defaultUsagePreferences();
  assert.deepEqual(parseUsagePreferences(value), value);
  for (const bad of [
    null,
    {},
    { ...value, revision: -1 },
    { ...value, revision: NaN },
    { ...value, claude: { enabled: true, source: "codex-lb" } },
    { ...value, codex: { enabled: "false", source: "cli" } },
  ])
    assert.equal(parseUsagePreferences(bad), undefined);
  const copy = parseUsagePreferences(value);
  copy.claude.enabled = false;
  assert.equal(value.claude.enabled, true);
});

test("an unavailable pooled source falls back to a valid CLI measurement", () => {
  const now = new Date().toISOString();
  const ok = { status: { state: "ok", stale: false, quota_observed_at: now } };
  const down = { status: { state: "networkError", stale: true } };
  const snapshot = (claude, codex) => ({
    usage: { claude: { sources: claude }, codex: { sources: codex } },
  });
  const preferences = defaultUsagePreferences();
  const hostWithoutTools = snapshot(
    { cli: { ...ok, provider: "claude" }, cswap: down },
    { cli: { ...ok, provider: "codex" } },
  );
  assert.equal(
    effectiveUsageSource(hostWithoutTools, preferences, "claude"),
    "cli",
  );
  assert.equal(
    effectiveUsageSource(hostWithoutTools, preferences, "codex"),
    "cli",
  );
  assert.equal(
    selectedUsage(hostWithoutTools, preferences).claude.provider,
    "claude",
  );
  // A working pooled source keeps the explicit choice.
  const pooled = snapshot({ cli: ok, cswap: ok }, { cli: ok, "codex-lb": ok });
  assert.equal(effectiveUsageSource(pooled, preferences, "claude"), "cswap");
  assert.equal(effectiveUsageSource(pooled, preferences, "codex"), "codex-lb");
  // No fallback when the CLI source has nothing valid either.
  const nothing = snapshot({ cli: down, cswap: down }, {});
  assert.equal(effectiveUsageSource(nothing, preferences, "claude"), "cswap");
  assert.equal(selectedUsage(nothing, preferences).codex, undefined);
});
