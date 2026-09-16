import test from "node:test";
import assert from "node:assert/strict";
import { createPreferences } from "../src/preferences.ts";
test("blocked browser storage does not prevent boot or logout", () => {
  const prefs = createPreferences(() => {
    throw new Error("SecurityError");
  });
  assert.equal(prefs.get("hmux.font"), null);
  prefs.set("hmux.tabs", "[]");
  assert.equal(prefs.get("hmux.tabs"), "[]");
  prefs.remove("hmux.tabs");
  assert.equal(prefs.get("hmux.tabs"), null);
});
test("quota failure retains preferences in memory", () => {
  const prefs = createPreferences(() => ({
    getItem: () => null,
    setItem: () => {
      throw new Error("QuotaExceededError");
    },
    removeItem: () => {},
  }));
  prefs.set("hmux.font", "18");
  assert.equal(prefs.get("hmux.font"), "18");
});
