import { msg, bindText, bindAttribute } from "./i18n.ts";
import type { Terminal } from "@xterm/xterm";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { hasNativeSelection } from "./native-clipboard.ts";

export function safeTerminalURL(value: string): string | undefined {
  try {
    const url = new URL(value);
    if (
      !["http:", "https:"].includes(url.protocol) ||
      url.username ||
      url.password
    )
      return;
    return url.href;
  } catch {
    return;
  }
}

export function installTerminalLinks(
  term: Terminal,
  host: HTMLElement,
  enabled: () => boolean,
) {
  const doc = host.ownerDocument;
  let hoveredURL: string | undefined;
  let popover: HTMLElement | undefined;
  const hide = () => {
    popover?.remove();
    popover = undefined;
  };
  const show = (
    event: Pick<MouseEvent, "clientX" | "clientY" | "preventDefault">,
    value: string,
  ) => {
    if (!enabled() || term.hasSelection() || hasNativeSelection(host)) return;
    const url = safeTerminalURL(value);
    if (!url) return;
    event.preventDefault();
    hide();
    popover = doc.createElement("div");
    popover.className = "terminal-link-popover xterm-hover";
    popover.setAttribute("role", "dialog");
    bindAttribute(popover, "aria-label", msg("Open link", "링크 열기"));
    const address = doc.createElement("span");
    address.textContent = url;
    const open = doc.createElement("a");
    open.href = url;
    open.target = "_blank";
    open.rel = "noopener noreferrer";
    bindText(open, msg("Open in new window ↗", "새 창에서 열기 ↗"));
    const close = doc.createElement("button");
    close.type = "button";
    bindText(close, msg("Close", "닫기"));
    close.onclick = hide;
    open.onclick = hide;
    popover.append(address, open, close);
    const view = doc.defaultView!;
    const viewport = view.visualViewport;
    const left = viewport?.offsetLeft ?? 0;
    const top = viewport?.offsetTop ?? 0;
    const width = viewport?.width ?? view.innerWidth;
    const height = viewport?.height ?? view.innerHeight;
    popover.style.maxWidth = `${Math.min(360, width - 16)}px`;
    popover.style.maxHeight = `${Math.max(44, height - 16)}px`;
    doc.body.append(popover);
    const rect = popover.getBoundingClientRect();
    popover.style.left = `${Math.max(left + 8, Math.min(event.clientX, left + width - rect.width - 8))}px`;
    popover.style.top = `${Math.max(top + 8, Math.min(event.clientY + 12, top + height - rect.height - 8))}px`;
  };
  const hover = (_event: MouseEvent, uri: string) => {
    hoveredURL = safeTerminalURL(uri);
  };
  const leave = () => {
    hoveredURL = undefined;
  };
  const links = new WebLinksAddon(show, { hover, leave });
  term.loadAddon(links);
  const previousHandler = term.options.linkHandler;
  term.options.linkHandler = { activate: show, hover, leave };
  const outside = (event: Event) => {
    if (popover && !popover.contains(event.target as Node)) hide();
  };
  const key = (event: KeyboardEvent) => {
    if (event.key === "Escape" && popover) {
      event.preventDefault();
      event.stopImmediatePropagation();
      hide();
    }
  };
  doc.addEventListener("pointerdown", outside, true);
  doc.addEventListener("keydown", key, true);
  doc.defaultView!.addEventListener("blur", hide);
  doc.defaultView!.addEventListener("resize", hide);
  doc.defaultView!.visualViewport?.addEventListener("resize", hide);
  doc.defaultView!.visualViewport?.addEventListener("scroll", hide);
  const scroll = term.onScroll(hide);
  const resize = term.onResize(hide);
  return {
    show,
    hide,
    get hoveredURL() {
      return hoveredURL;
    },
    dispose() {
      hide();
      links.dispose();
      term.options.linkHandler = previousHandler;
      scroll.dispose();
      resize.dispose();
      doc.removeEventListener("pointerdown", outside, true);
      doc.removeEventListener("keydown", key, true);
      doc.defaultView!.removeEventListener("blur", hide);
      doc.defaultView!.removeEventListener("resize", hide);
      doc.defaultView!.visualViewport?.removeEventListener("resize", hide);
      doc.defaultView!.visualViewport?.removeEventListener("scroll", hide);
    },
  };
}
