import { createPreferences } from "./preferences.ts";

export type Locale = "en" | "ko";
export type TextValue = string | (() => string);
export const localePreferenceKey = "hmux.locale";

export function createLocaleState(
  storage: () => Pick<Storage, "getItem" | "setItem" | "removeItem">,
) {
  const preferences = createPreferences(storage);
  let current: Locale =
    preferences.get(localePreferenceKey) === "ko" ? "ko" : "en";
  const listeners = new Set<() => void>();
  return {
    get: () => current,
    set(value: string) {
      if (value !== "en" && value !== "ko") return;
      preferences.set(localePreferenceKey, value);
      if (value === current) return;
      current = value;
      for (const listener of listeners) listener();
    },
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}

// English is deliberately the default, independent of browser/OS preferences.
// Storage is optional: a blocked storage API must not block login or switching.
const state = createLocaleState(() => localStorage);
export const getLocale = state.get;
export const setLocale = state.set;
export const onLocaleChange = state.subscribe;
export const localeTag = () => (getLocale() === "ko" ? "ko-KR" : "en-US");
export const t = (en: string, ko: string) => (getLocale() === "ko" ? ko : en);
export const msg = (en: string, ko: string) => () => t(en, ko);

type TextBinding = {
  read: () => string;
  node: ChildNode | null;
};
type AttributeBinding = { read: () => string; previous: string };
type Binding = {
  text?: TextBinding;
  attributes: Map<string, AttributeBinding>;
};
const marker = "data-hmux-i18n";
// Weak ownership avoids retaining closed dialogs or old session-list nodes.
// Only explicitly marked application chrome is visited on a language change;
// terminal, transcript, provider and user content is never scanned/translated.
const bindings = new WeakMap<HTMLElement, Binding>();

function bindingFor(node: HTMLElement): Binding {
  let binding = bindings.get(node);
  if (!binding) {
    binding = { attributes: new Map() };
    bindings.set(node, binding);
  }
  node.setAttribute(marker, "");
  return binding;
}

function releaseEmpty(node: HTMLElement, binding: Binding | undefined) {
  if (binding && !binding.text && !binding.attributes.size) {
    bindings.delete(node);
    node.removeAttribute(marker);
  }
}

export function bindText(node: HTMLElement, value: TextValue): void {
  const translated = typeof value === "function" ? value() : value;
  let owned: ChildNode | null = null;
  if (node.childNodes) {
    const textNodes = Array.from(node.childNodes).filter(
      (child) => child.nodeType === 3,
    );
    owned = textNodes[0] ?? node.ownerDocument.createTextNode("");
    if (!owned.parentNode) node.insertBefore(owned, node.firstChild);
    owned.textContent = translated;
    for (const extra of textNodes.slice(1)) node.removeChild(extra);
  } else {
    node.textContent = translated;
  }
  if (typeof value === "function") {
    bindingFor(node).text = { read: value, node: owned };
  } else {
    const binding = bindings.get(node);
    if (binding) delete binding.text;
    releaseEmpty(node, binding);
  }
}

export function bindAttribute(
  node: HTMLElement,
  name: string,
  value: TextValue,
): void {
  const translated = typeof value === "function" ? value() : value;
  node.setAttribute(name, translated);
  if (typeof value === "function") {
    bindingFor(node).attributes.set(name, {
      read: value,
      previous: translated,
    });
  } else {
    const binding = bindings.get(node);
    binding?.attributes.delete(name);
    releaseEmpty(node, binding);
  }
}

function refreshNode(node: HTMLElement) {
  const binding = bindings.get(node);
  if (!binding) return;
  if (binding.text) {
    const text = binding.text;
    if (!text.node) {
      // An initially empty localized label may acquire child controls later.
      // Insert its own Text node instead of replacing any of those controls.
      text.node = node.ownerDocument.createTextNode("");
      node.insertBefore(text.node, node.firstChild);
    }
    if (text.node.parentNode === node) text.node.textContent = text.read();
    else delete binding.text; // A newer renderer already owns this content.
  }
  for (const [name, attribute] of binding.attributes) {
    if (node.getAttribute(name) !== attribute.previous) {
      binding.attributes.delete(name);
      continue;
    }
    attribute.previous = attribute.read();
    node.setAttribute(name, attribute.previous);
  }
  releaseEmpty(node, binding);
}

export function applyLocale(root?: Document | HTMLElement): void {
  const target =
    root ?? (typeof document === "undefined" ? undefined : document);
  if (!target) return;
  const doc =
    target.nodeType === 9 ? (target as Document) : target.ownerDocument;
  if (doc) doc.documentElement.lang = getLocale();
  if (target.nodeType === 1 && (target as HTMLElement).hasAttribute(marker))
    refreshNode(target as HTMLElement);
  for (const node of target.querySelectorAll<HTMLElement>(`[${marker}]`))
    refreshNode(node);
}

state.subscribe(() => applyLocale());
if (typeof document !== "undefined")
  document.documentElement.lang = getLocale();
