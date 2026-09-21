import test from "node:test";
import assert from "node:assert/strict";
import {
  defaultUsagePreferences,
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
