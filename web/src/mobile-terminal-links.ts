import type { Terminal } from "@xterm/xterm";
import { hasNativeSelection } from "./native-clipboard.ts";
import { installTerminalLinks } from "./terminal-links.ts";

// Match native-clipboard's thresholds so long presses and handle drags always
// remain browser-owned. Crossing the movement threshold cancels the whole tap.
export class LinkTap {
  private start?: { id: number; x: number; y: number; time: number };
  cancel() {
    this.start = undefined;
  }
  begin(points: TouchList, time: number) {
    this.cancel();
    if (points.length === 1) {
      const p = points[0];
      this.start = { id: p.identifier, x: p.clientX, y: p.clientY, time };
    }
  }
  move(points: TouchList) {
    const p = points[0];
    if (
      this.start &&
      (points.length !== 1 ||
        p.identifier !== this.start.id ||
        Math.hypot(p.clientX - this.start.x, p.clientY - this.start.y) > 5)
    )
      this.cancel();
  }
  end(points: TouchList, time: number) {
    this.move(points);
    const valid = !!this.start && time - this.start.time < 350;
    this.cancel();
    return valid;
  }
}

export function installMobileTerminalLinks(
  term: Terminal,
  host: HTMLElement,
  enabled: () => boolean,
) {
  const links = installTerminalLinks(term, host, enabled);
  const screen = term.element!.querySelector<HTMLElement>(".xterm-screen")!;
  const tap = new LinkTap();
  let suppressClick: { x: number; y: number; until: number } | undefined;
  const hide = () => {
    tap.cancel();
    links.hide();
  };
  const selectable = () =>
    enabled() && !hasNativeSelection(host) && !term.hasSelection();
  const start = (event: TouchEvent) => {
    hide();
    suppressClick = undefined;
    // The existing input textarea owns Paste and all editable gestures.
    if (
      !selectable() ||
      !screen.contains(event.target as Node) ||
      (event.target as Element).closest(
        "textarea,input,select,button,a,[contenteditable]",
      )
    )
      return;
    tap.begin(event.touches, Date.now());
  };
  const move = (event: TouchEvent) => tap.move(event.touches);
  const end = (event: TouchEvent) => {
    if (
      event.touches.length ||
      !tap.end(event.changedTouches, Date.now()) ||
      !selectable()
    ) {
      tap.cancel();
      return;
    }
    const point = event.changedTouches[0];
    const rect = screen.getBoundingClientRect();
    if (
      point.clientX < rect.left ||
      point.clientX >= rect.right ||
      point.clientY < rect.top ||
      point.clientY >= rect.bottom
    )
      return;

    // xterm 6's web/OSC8 providers resolve synchronously through public hover
    // callbacks. A non-bubbling hover reaches the screen's linkifier only, never
    // the parent's PTY mouse-reporting listeners (including all-motion mode).
    // Visit a different row first to invalidate the previous hover/line cache,
    // including repeated taps at the same position after terminal output changes.
    const hover = (x: number, y: number) =>
      screen.dispatchEvent(
        new MouseEvent("mousemove", {
          clientX: x,
          clientY: y,
          bubbles: false,
          buttons: 0,
        }),
      );
    const rowHeight = rect.height / term.rows;
    const otherY =
      point.clientY < rect.top + rect.height / 2
        ? rect.bottom - rowHeight / 2
        : rect.top + rowHeight / 2;
    hover(
      point.clientX < rect.left + rect.width / 2
        ? rect.right - rect.width / term.cols / 2
        : rect.left + rect.width / term.cols / 2,
      otherY,
    );
    screen.dispatchEvent(new MouseEvent("mouseleave"));
    hover(point.clientX, point.clientY);
    const url = links.hoveredURL;
    screen.dispatchEvent(new MouseEvent("mouseleave"));
    if (!url) return;
    links.show(
      {
        clientX: point.clientX,
        clientY: point.clientY,
        preventDefault: () => event.preventDefault(),
      },
      url,
    );
    // Prevent compatibility clicks from refocusing the keyboard underneath the
    // popup. Let touchend propagate so native clipboard/scroll tracking settles.
    suppressClick = {
      x: point.clientX,
      y: point.clientY,
      until: Date.now() + 800,
    };
  };
  const click = (event: MouseEvent) => {
    const pending = suppressClick;
    suppressClick = undefined;
    if (
      pending &&
      Date.now() < pending.until &&
      Math.hypot(event.clientX - pending.x, event.clientY - pending.y) <= 5
    ) {
      event.preventDefault();
      event.stopImmediatePropagation();
    }
  };
  const cancel = () => tap.cancel();
  host.addEventListener("touchstart", start, { capture: true, passive: true });
  host.addEventListener("touchmove", move, { capture: true, passive: true });
  host.addEventListener("touchend", end, { capture: true, passive: false });
  host.addEventListener("touchcancel", cancel, true);
  host.addEventListener("contextmenu", cancel, true);
  host.addEventListener("click", click, true);
  const scroll = term.onScroll(hide);
  const resize = term.onResize(hide);
  const output = term.onWriteParsed(cancel);
  host.ownerDocument.defaultView!.addEventListener("blur", hide);
  return {
    hide,
    dispose() {
      hide();
      links.dispose();
      scroll.dispose();
      resize.dispose();
      output.dispose();
      host.removeEventListener("touchstart", start, true);
      host.removeEventListener("touchmove", move, true);
      host.removeEventListener("touchend", end, true);
      host.removeEventListener("touchcancel", cancel, true);
      host.removeEventListener("contextmenu", cancel, true);
      host.removeEventListener("click", click, true);
      host.ownerDocument.defaultView!.removeEventListener("blur", hide);
    },
  };
}
