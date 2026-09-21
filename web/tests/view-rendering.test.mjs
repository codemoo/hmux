import test from "node:test";
import assert from "node:assert/strict";
import { renderMarkdown } from "../src/markdown.ts";
import { renderConversation } from "../src/conversation-view.ts";
import { renderUsageFooter, renderUsagePanel } from "../src/usage-view.ts";

// A small document fixture: textContent stays text and does not parse HTML.
function element(tagName = "div", doc) {
  const node = {
    tagName,
    ownerDocument: doc,
    children: [],
    attrs: {},
    dataset: {},
    className: "",
    style: {
      setProperty(name, value) {
        this[name] = value;
      },
    },
    append(...children) {
      this.children.push(...children);
    },
    prepend(...children) {
      this.children.unshift(...children);
    },
    replaceChildren(...children) {
      this._text = "";
      this.children = children;
    },
    setAttribute(name, value) {
      this.attrs[name] = value;
    },
    get textContent() {
      return (
        (this._text || "") + this.children.map((n) => n.textContent).join("")
      );
    },
    set textContent(value) {
      this._text = value;
      this.children = [];
    },
  };
  node.classList = {
    add(name) {
      node.classList.toggle(name, true);
    },
    toggle(name, enabled) {
      const values = new Set(node.className.split(" ").filter(Boolean));
      if (enabled) values.add(name);
      else values.delete(name);
      node.className = [...values].join(" ");
    },
  };
  return node;
}
function root() {
  const doc = { createElement: (tag) => element(tag, doc) };
  return element("section", doc);
}
const all = (node) => [node, ...node.children.flatMap(all)];
const articles = (node) => all(node).filter((n) => n.tagName === "article");

test("markdown renders aligned tables, nested lists, task lists and inline formatting", () => {
  const view = root();
  renderMarkdown(
    view,
    `# Summary

| Name | Result | Count |
| :--- | :---: | ---: |
| **Korean 한글** | [Docs](https://example.com) | 12 |
| A \\| B | \`x < y\` | 3 |

> Quoted *text* &amp; more

3. First
   - Nested
4. Second

- [x] Done
- [ ] Pending

~~Removed~~
`,
  );
  const nodes = all(view);
  assert.equal(nodes.filter((n) => n.tagName === "h1").length, 1);
  const headers = nodes.filter((n) => n.tagName === "th");
  assert.deepEqual(
    headers.map((n) => n.style.textAlign),
    ["left", "center", "right"],
  );
  assert.ok(headers.every((n) => n.attrs.scope === "col"));
  assert.equal(nodes.filter((n) => n.tagName === "td").length, 6);
  assert.ok(view.textContent.includes("A | B"));
  assert.ok(view.textContent.includes("& more"));
  assert.equal(nodes.find((n) => n.tagName === "ol").attrs.start, "3");
  assert.equal(nodes.filter((n) => n.tagName === "strong").length, 1);
  assert.equal(nodes.filter((n) => n.tagName === "em").length, 1);
  assert.equal(nodes.filter((n) => n.tagName === "del").length, 1);
  const checks = nodes.filter((n) => n.tagName === "input");
  assert.deepEqual(
    checks.map((n) => n.checked),
    [true, false],
  );
  assert.ok(checks.every((n) => n.disabled));
});

test("markdown treats HTML as text and blocks unsafe URLs and automatic image loads", () => {
  const view = root();
  renderMarkdown(
    view,
    `<script>alert(1)</script>

<img src=x onerror=alert(1)>

[bad](javascript:alert%281%29) [encoded](jav&#x61;script:alert%281%29)
[data](data:text/html,test) [local](file:///etc/passwd) [relative](/api/logout)
[credentials](https://user:password@example.com) [safe](https://example.com?a=1&amp;b=2)
![image](https://example.com/tracker.png)

&lt;img src=x onerror=alert(1)&gt;`,
  );
  const nodes = all(view);
  assert.ok(
    !nodes.some((n) => ["img", "script", "iframe"].includes(n.tagName)),
  );
  assert.ok(view.textContent.includes("<script>alert(1)</script>"));
  const links = nodes.filter((n) => n.tagName === "a");
  assert.deepEqual(
    links.map((n) => n.attrs.href),
    ["https://example.com/?a=1&b=2", "https://example.com/tracker.png"],
  );
  assert.ok(
    links.every(
      (n) =>
        n.attrs.target === "_blank" && n.attrs.rel === "noopener noreferrer",
    ),
  );
});

test("code toggle covers tilde fences and indented blocks without hiding inline code", () => {
  const view = root();
  const source =
    "Inline `keep`\n\n~~~ts\nconst a = '<img>';\n~~~\n\n    indented()\n";
  renderMarkdown(view, source, false);
  assert.equal(all(view).filter((n) => n.tagName === "pre").length, 0);
  assert.equal(
    all(view).filter((n) => n.className === "markdown-code-hidden").length,
    2,
  );
  assert.ok(view.textContent.includes("keep"));
  renderMarkdown(view, source, true);
  assert.equal(all(view).filter((n) => n.tagName === "pre").length, 2);
  assert.ok(view.textContent.includes("const a = '<img>';"));
  assert.ok(!all(view).some((n) => n.tagName === "img"));
});

test("reader preserves safe text, question/code toggles and return action", () => {
  const reader = root();
  const unsafe = '<img src=x onerror="unexpected()">';
  const question = "user code\n```sh\necho example\n```";
  const answer = unsafe + "\n\n```sh\nprintf example\n```";
  let returned = 0;
  assert.equal(
    renderConversation(
      reader,
      {
        status: "ready",
        truncated: true,
        messages: [
          { role: "user", text: question },
          { role: "assistant", text: answer },
        ],
      },
      () => returned++,
    ),
    true,
  );
  assert.equal(articles(reader).length, 2);
  assert.ok(articles(reader)[0].textContent.includes("echo example"));
  assert.ok(articles(reader)[1].textContent.includes(unsafe));
  assert.ok(articles(reader)[1].textContent.includes("[코드 숨김]"));
  assert.ok(!all(reader).some((n) => n.tagName === "img"));
  assert.ok(reader.textContent.includes("최근 대화 일부"));
  const [include, code] = all(reader).filter((n) => n.tagName === "input");
  include.checked = false;
  include.onchange();
  assert.equal(articles(reader).length, 1);
  code.checked = true;
  code.onchange();
  assert.ok(articles(reader)[0].textContent.includes("printf example"));
  const buttons = all(reader).filter((n) => n.tagName === "button");
  reader.scrollHeight = 99;
  buttons[0].onclick();
  assert.equal(reader.scrollTop, 99);
  buttons[1].onclick();
  assert.equal(returned, 1);
  assert.equal(
    renderConversation(
      reader,
      { status: "unavailable", messages: [], truncated: false },
      () => {},
    ),
    false,
  );
  assert.equal(articles(reader).length, 0);
});

test("usage panel keeps missing data distinct from zero remaining capacity", () => {
  const panel = root();
  renderUsagePanel(panel, { online: false }, "");
  assert.equal(all(panel).filter((n) => n.attrs.role === "meter").length, 0);
  assert.ok(panel.textContent.includes("확인 대기"));
  const now = new Date().toISOString();
  const usage = {
    provider: "codex",
    generated_at_utc: now,
    weekly_observed: true,
    weekly: { used_pct: 1 },
    status: { stale: false, state: "ok", quota_observed_at: now },
    accounts: [
      {
        number: 1,
        display_name: "<script>example</script>",
        active: true,
        status: "ok",
        seven_day: { used_pct: 1 },
      },
    ],
  };
  const ready = root();
  renderUsagePanel(
    ready,
    { online: true, usage: { codex: { sources: { "codex-lb": usage } } } },
    "Home metrics",
  );
  const meters = all(ready).filter((n) => n.attrs.role === "meter");
  assert.ok(meters.length >= 2);
  for (const meter of meters) {
    assert.equal(meter.attrs["aria-valuenow"], "0");
    assert.equal(meter.dataset.level, "low");
  }
  assert.ok(ready.textContent.includes("<script>example</script>"));
  assert.ok(!all(ready).some((n) => n.tagName === "script"));
  assert.ok(ready.textContent.includes("Home metrics"));
});

test("footer replaces stale metrics and animation when host goes offline", () => {
  const elements = { dog: root(), usageButton: root(), metrics: root() };
  renderUsageFooter(
    {
      online: true,
      catalog: {
        host_metrics: {
          observed_at: new Date().toISOString(),
          cpu_percent: 12,
          memory_used_bytes: 50,
          memory_total_bytes: 100,
        },
      },
    },
    elements,
  );
  assert.ok(elements.metrics.textContent.includes("CPU 12%"));
  assert.ok(elements.metrics.textContent.includes("RAM 50%"));
  elements.dog.className = "running";
  renderUsageFooter({ online: false }, elements);
  assert.equal(elements.metrics.textContent, "Home · 사용량 대기 중");
  assert.equal(elements.dog.className, "");
  assert.equal(elements.usageButton.children.length, 5);
});

test("usage visibility hides disabled providers and keeps host metrics", async () => {
  const { defaultUsagePreferences } =
    await import("../src/usage-preferences.ts");
  const preferences = defaultUsagePreferences();
  preferences.claude.enabled = false;
  preferences.codex.enabled = false;
  const elements = { dog: root(), usageButton: root(), metrics: root() };
  renderUsageFooter({ online: false }, elements, preferences);
  assert.equal(elements.usageButton.hidden, true);
  assert.equal(elements.dog.hidden, true);
  assert.equal(elements.usageButton.children.length, 0);
  assert.ok(elements.metrics.textContent.includes("Home"));
  const panel = root();
  renderUsagePanel(panel, { online: false }, "Host", preferences);
  assert.equal(
    all(panel).filter((n) => n.className === "usage-provider").length,
    0,
  );
});

test("both providers show weekly reset, absent 5h is hidden and Codex plans stay account specific", () => {
  const now = new Date().toISOString();
  const reset = new Date(Date.now() + 2 * 86400000).toISOString();
  const usage = (provider) => ({
    provider,
    generated_at_utc: now,
    weekly_observed: true,
    weekly: { used_pct: 0.5, resets_at: reset },
    rolling_5h_observed: false,
    status: { stale: false, state: "ok" },
    accounts: [
      {
        number: 1,
        status: "ok",
        plan_type: "plus",
        seven_day: { used_pct: 0.3, resets_at: reset },
      },
      {
        number: 2,
        status: "ok",
        plan_type: "pro",
        seven_day: { used_pct: 0.4, resets_at: reset },
      },
    ],
  });
  const claude = usage("claude");
  const codex = usage("codex");
  const snapshot = {
    online: true,
    usage: {
      claude: { sources: { cswap: claude } },
      codex: { sources: { "codex-lb": codex } },
    },
  };
  const panel = root();
  renderUsagePanel(panel, snapshot, "");
  const providers = all(panel).filter((n) => n.className === "usage-provider");
  assert.equal(providers.length, 2);
  for (const provider of providers) {
    assert.ok(provider.textContent.includes("리셋까지 2일"));
    assert.ok(!provider.textContent.includes("5시간"));
  }
  assert.ok(!providers[0].textContent.includes("Plus"));
  assert.deepEqual(
    all(providers[1])
      .filter((n) => n.className.includes("usage-plan"))
      .map((n) => n.textContent),
    ["Plus", "Pro"],
  );
  codex.accounts[1].five_hour = { used_pct: 1 };
  const withFive = root();
  renderUsagePanel(withFive, snapshot, "");
  assert.equal(
    all(withFive).filter(
      (n) => n.className === "usage-gauge" && n.textContent.includes("5시간"),
    ).length,
    1,
  );
  assert.ok(withFive.textContent.includes("5시간 잔여0%"));
});

test("unavailable quota retains source reset time with a last-observed label", () => {
  const reset = new Date(Date.now() + 86400000).toISOString();
  const panel = root();
  const usage = {
    provider: "claude",
    generated_at_utc: new Date().toISOString(),
    weekly_observed: true,
    weekly: { used_pct: 0.2, resets_at: reset },
    status: { stale: true, state: "networkError" },
    accounts: [
      {
        number: 1,
        status: "token_expired",
        seven_day: { used_pct: 0.2, resets_at: reset },
      },
    ],
  };
  renderUsagePanel(
    panel,
    { online: true, usage: { claude: { sources: { cswap: usage } } } },
    "",
  );
  assert.ok(panel.textContent.includes("리셋까지 1일 · 최근 조회 기준"));
  assert.ok(!panel.textContent.includes("80%"));
  assert.ok(panel.textContent.includes("로그인 갱신 필요"));
});
