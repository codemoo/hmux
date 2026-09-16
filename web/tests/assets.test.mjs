import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";
const root = fileURLToPath(new URL("../", import.meta.url));
test("stylesheet assets resolve after release cleanup", () => {
  for (const name of readdirSync(path.join(root, "src")).filter((n) =>
    n.endsWith(".css"),
  )) {
    const css = readFileSync(path.join(root, "src", name), "utf8");
    for (const [, url] of css.matchAll(/url\(["']?(\/[^\s"')]+)["']?\)/g)) {
      assert.ok(
        existsSync(path.join(root, "public", url)),
        `${name}: missing ${url}`,
      );
    }
  }
});
