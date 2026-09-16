import type { Terminal } from "@xterm/xterm";
import { installTerminalLinks } from "./terminal-links.ts";
export { safeTerminalURL } from "./terminal-links.ts";

export function isTerminalCopy(
  term: Pick<Terminal, "hasSelection">,
  event: KeyboardEvent,
  mac = /Mac/.test(globalThis.navigator?.platform ?? ""),
) {
  return (
    event.code === "KeyC" &&
    (mac ? event.metaKey : event.ctrlKey) &&
    !event.altKey &&
    !event.isComposing &&
    term.hasSelection()
  );
}

// tmux enables mouse reporting, which disables xterm's ordinary drag selection.
// Delay primary clicks until release; promote drags to xterm forced selection.
// This avoids sending a partial drag to tmux and retains native xterm copy.
export function installDesktopTerminal(
  term: Terminal,
  host: HTMLElement,
  enabled: () => boolean,
  beforeSelect: () => void,
) {
  const doc = host.ownerDocument;
  const screen = term.element!.querySelector<HTMLElement>(".xterm-screen")!;
  let drag: { event: MouseEvent; moved: boolean; url?: string } | undefined;
  let replaying = false;
  const oldForce = term.options.macOptionClickForcesSelection;
  term.options.macOptionClickForcesSelection = true;
  const isMac = /Mac/.test(navigator.platform);
  const links = installTerminalLinks(term, host, enabled);
  const { hide, show } = links;
  const replay = (event: MouseEvent, type: string, force = false) => {
    replaying = true;
    try {
      screen.dispatchEvent(
        new MouseEvent(type, {
          bubbles: true,
          cancelable: true,
          clientX: event.clientX,
          clientY: event.clientY,
          button: 0,
          buttons: type === "mouseup" ? 0 : 1,
          detail: event.detail || 1,
          altKey: force && isMac,
          shiftKey: force && !isMac,
        }),
      );
    } finally {
      replaying = false;
    }
  };
  const down = (event: MouseEvent) => {
    if (
      replaying ||
      !enabled() ||
      event.button !== 0 ||
      !screen.contains(event.target as Node)
    )
      return;
    hide();
    beforeSelect();
    if (
      term.modes.mouseTrackingMode === "none" ||
      event.altKey ||
      event.ctrlKey ||
      event.metaKey ||
      event.shiftKey
    )
      return;
    event.preventDefault();
    event.stopImmediatePropagation();
    const url = links.hoveredURL;
    term.clearSelection();
    drag = { event, moved: false, url };
  };
  const move = (event: MouseEvent) => {
    if (!drag || replaying) return;
    if (!enabled() || !(event.buttons & 1)) {
      drag = undefined;
      return;
    }
    if (
      !drag.moved &&
      Math.hypot(
        event.clientX - drag.event.clientX,
        event.clientY - drag.event.clientY,
      ) > 4
    ) {
      drag.moved = true;
      term.clearSelection();
      replay(drag.event, "mousedown", true);
    }
    // Once started, xterm owns selection, wide cells, autoscroll and copy.
    if (!drag.moved) {
      event.preventDefault();
      event.stopImmediatePropagation();
    }
  };
  const up = (event: MouseEvent) => {
    if (!drag || replaying || event.button !== 0) return;
    const current = drag;
    drag = undefined;
    if (!current.moved) {
      event.preventDefault();
      event.stopImmediatePropagation();
      if (enabled()) {
        if (current.url) show(event, current.url);
        else {
          replay(current.event, "mousedown");
          replay(event, "mouseup");
        }
      }
    }
  };
  const copy = (event: ClipboardEvent) => {
    if (!enabled() || !term.hasSelection() || !event.clipboardData) return;
    event.clipboardData.setData("text/plain", term.getSelection());
    event.preventDefault();
    event.stopImmediatePropagation();
  };
  const blur = () => {
    drag = undefined;
    hide();
  };
  host.addEventListener("mousedown", down, true);
  host.addEventListener("copy", copy, true);
  doc.addEventListener("mousemove", move, true);
  doc.addEventListener("mouseup", up, true);
  doc.defaultView!.addEventListener("blur", blur);
  return {
    hide,
    dispose() {
      drag = undefined;
      hide();
      links.dispose();
      term.options.macOptionClickForcesSelection = oldForce;
      host.removeEventListener("mousedown", down, true);
      host.removeEventListener("copy", copy, true);
      doc.removeEventListener("mousemove", move, true);
      doc.removeEventListener("mouseup", up, true);
      doc.defaultView!.removeEventListener("blur", blur);
    },
  };
}
