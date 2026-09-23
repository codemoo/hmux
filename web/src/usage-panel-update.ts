import { renderUsagePanel } from "./usage-view.ts";
import { selectedUsage, type UsagePreferences } from "./usage-preferences.ts";
import type { Snapshot } from "./types.ts";

// Keep mounted nodes (and selection/scroll) while updating presentation values.
function reconcile(current: Node, next: Node) {
  if (
    current.nodeType !== next.nodeType ||
    current.nodeName !== next.nodeName
  ) {
    current.parentNode?.replaceChild(next, current);
    return;
  }
  if (current.nodeType === 3) {
    if (current.nodeValue !== next.nodeValue)
      current.nodeValue = next.nodeValue;
    return;
  }
  if (current.nodeType === 1) {
    const a = current as Element,
      b = next as Element;
    for (const attribute of Array.from(a.attributes))
      if (!b.hasAttribute(attribute.name)) a.removeAttribute(attribute.name);
    for (const attribute of Array.from(b.attributes))
      if (a.getAttribute(attribute.name) !== attribute.value)
        a.setAttribute(attribute.name, attribute.value);
  }
  const children = Array.from(next.childNodes);
  for (let i = 0; i < children.length; i++) {
    const child = current.childNodes[i];
    if (child) reconcile(child, children[i]);
    else current.appendChild(children[i]);
  }
  while (current.childNodes.length > children.length)
    current.removeChild(current.lastChild!);
}
export function createUsagePanelUpdater(body: HTMLElement) {
  let previous = "";
  return (
    snapshot: Snapshot,
    metrics: string,
    preferences: UsagePreferences,
    now = Date.now(),
  ) => {
    const signature = JSON.stringify([
      selectedUsage(snapshot, preferences),
      preferences,
      Math.floor(now / 30000),
    ]);
    if (signature !== previous) {
      const next = body.ownerDocument.createElement("div");
      renderUsagePanel(next, snapshot, metrics, preferences);
      const scroll = body.scrollTop;
      // Reconcile children only; retain the dialog container and its classes.
      const children = Array.from(next.childNodes);
      for (let i = 0; i < children.length; i++) {
        if (body.childNodes[i]) reconcile(body.childNodes[i], children[i]);
        else body.appendChild(children[i]);
      }
      while (body.childNodes.length > children.length) body.lastChild?.remove();
      body.scrollTop = scroll;
      previous = signature;
    }
    const host = body.querySelector(".usage-host");
    const label = metrics || "Home · 사용량 대기 중";
    if (host && host.textContent !== label) host.textContent = label;
  };
}
