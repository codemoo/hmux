import test from "node:test";
import assert from "node:assert/strict";
import {
  applyLocale,
  bindAttribute,
  bindText,
  createLocaleState,
  getLocale,
  localePreferenceKey,
  localeTag,
  msg,
  setLocale,
} from "../src/i18n.ts";

test("English default and only explicit English/Korean settings are accepted", () => {
  const saved = new Map([[localePreferenceKey, "fr"]]);
  const storage = {
    getItem: (key) => saved.get(key) ?? null,
    setItem: (key, value) => saved.set(key, value),
    removeItem: (key) => saved.delete(key),
  };
  const state = createLocaleState(() => storage);
  assert.equal(state.get(), "en");
  state.set("ko-KR");
  assert.equal(state.get(), "en");
  let changed = 0;
  const unsubscribe = state.subscribe(() => changed++);
  state.set("ko");
  assert.equal(saved.get(localePreferenceKey), "ko");
  assert.equal(createLocaleState(() => storage).get(), "ko");
  state.set("ko");
  assert.equal(changed, 1);
  unsubscribe();
  state.set("en");
  assert.equal(changed, 1);
});

test("storage failure does not prevent a language change", () => {
  const state = createLocaleState(() => {
    throw new Error("blocked");
  });
  assert.equal(state.get(), "en");
  state.set("ko");
  assert.equal(state.get(), "ko");
  state.set("arbitrary");
  assert.equal(state.get(), "ko");
});

// A minimal DOM with real Text-node ownership rather than string substitution.
class TextNode {
  nodeType = 3;
  parentNode = null;
  constructor(text) {
    this.textContent = text;
  }
}
class Element {
  nodeType = 1;
  parentNode = null;
  childNodes = [];
  attributes = new Map();
  constructor(doc) {
    this.ownerDocument = doc;
  }
  get firstChild() {
    return this.childNodes[0] ?? null;
  }
  get textContent() {
    return this.childNodes.map((node) => node.textContent).join("");
  }
  set textContent(value) {
    for (const node of this.childNodes) node.parentNode = null;
    this.childNodes = [];
    if (value) this.insertBefore(new TextNode(value), null);
  }
  insertBefore(node, before) {
    const index = before
      ? this.childNodes.indexOf(before)
      : this.childNodes.length;
    this.childNodes.splice(index, 0, node);
    node.parentNode = this;
    return node;
  }
  removeChild(node) {
    this.childNodes.splice(this.childNodes.indexOf(node), 1);
    node.parentNode = null;
  }
  setAttribute(name, value) {
    this.attributes.set(name, value);
  }
  getAttribute(name) {
    return this.attributes.get(name) ?? null;
  }
  hasAttribute(name) {
    return this.attributes.has(name);
  }
  removeAttribute(name) {
    this.attributes.delete(name);
  }
  querySelectorAll() {
    return this.childNodes.flatMap((node) =>
      node.nodeType === 1
        ? [
            ...(node.hasAttribute("data-hmux-i18n") ? [node] : []),
            ...node.querySelectorAll(),
          ]
        : [],
    );
  }
}
function documentFixture() {
  const doc = { nodeType: 9, createTextNode: (value) => new TextNode(value) };
  doc.documentElement = new Element(doc);
  doc.querySelectorAll = () => doc.documentElement.querySelectorAll();
  return doc;
}

test("switching bound chrome preserves controls, drafts, icons and external content", () => {
  setLocale("en");
  const doc = documentFixture();
  const label = new Element(doc);
  const input = new Element(doc);
  input.value = "draft and 비밀 remain verbatim";
  const icon = new Element(doc);
  const transcript = new Element(doc);
  transcript.textContent = "Settings 설정 — user text, not UI";
  doc.documentElement.insertBefore(label, null);
  doc.documentElement.insertBefore(transcript, null);
  label.insertBefore(input, null);
  label.insertBefore(icon, null);
  bindText(label, msg("Settings", "설정"));
  bindAttribute(input, "aria-label", msg("Session name", "세션 이름"));
  assert.equal(input.parentNode, label);
  assert.equal(icon.parentNode, label);
  for (const [locale, text, accessible] of [
    ["ko", "설정", "세션 이름"],
    ["en", "Settings", "Session name"],
  ]) {
    setLocale(locale);
    applyLocale(doc);
    assert.equal(label.firstChild.textContent, text);
    assert.equal(input.getAttribute("aria-label"), accessible);
    assert.equal(input.parentNode, label);
    assert.equal(icon.parentNode, label);
    assert.equal(input.value, "draft and 비밀 remain verbatim");
    assert.equal(transcript.textContent, "Settings 설정 — user text, not UI");
    assert.equal(doc.documentElement.lang, locale);
  }
  assert.equal(getLocale(), "en");
  assert.equal(localeTag(), "en-US");
});

test("newer content and attribute owners are not overwritten by stale bindings", () => {
  setLocale("en");
  const doc = documentFixture();
  const node = new Element(doc);
  doc.documentElement.insertBefore(node, null);
  bindText(node, msg("Old status", "이전 상태"));
  bindAttribute(node, "title", msg("Old title", "이전 제목"));
  node.textContent = "new external content";
  node.setAttribute("title", "new external title");
  setLocale("ko");
  applyLocale(doc);
  assert.equal(node.textContent, "new external content");
  assert.equal(node.getAttribute("title"), "new external title");
  assert.equal(node.hasAttribute("data-hmux-i18n"), false);
  setLocale("en");
});
