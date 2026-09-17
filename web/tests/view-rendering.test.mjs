import test from "node:test";
import assert from "node:assert/strict";
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

test("reader preserves safe text, question/code toggles and return action", () => {
  const reader = root();
  const unsafe = '<img src=x onerror="unexpected()">';
  const question = "user code\n```sh\necho example\n```";
  const answer = unsafe + "\n```sh\nprintf example\n```";
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
  assert.ok(articles(reader)[0].textContent.includes(question));
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
  assert.ok(articles(reader)[0].textContent.includes(answer));
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
    { online: true, usage: { codex: usage } },
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
