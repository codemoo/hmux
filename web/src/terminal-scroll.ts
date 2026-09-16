import type { Terminal } from "@xterm/xterm";

export function scrollSteps(distance: number, cellHeight: number) {
  const step = Math.max(8, cellHeight);
  const lines = Math.max(-8, Math.min(8, Math.trunc(distance / step)));
  return { lines, remaining: distance - lines * step };
}

// Touch gestures need to become terminal wheel events when tmux owns history.
// Scrolling the DOM viewport alone cannot reach tmux's alternate-screen history.
export function installTerminalScroll(
  term: Terminal,
  host: HTMLElement,
  enabled: () => boolean,
  nativeSelection: () => boolean = () => false,
) {
  let lastY = 0;
  let distance = 0;
  let tracking = false;
  let dragged = false;
  let suppressClickUntil = 0;
  let startedAt = 0;
  let held = false;
  host.addEventListener(
    "touchstart",
    (event) => {
      if (!enabled() || event.touches.length !== 1) {
        tracking = false;
        return;
      }
      startedAt = Date.now();
      held = false;
      lastY = event.touches[0].clientY;
      distance = 0;
      dragged = false;
      tracking = true;
    },
    { passive: true },
  );
  host.addEventListener(
    "touchmove",
    (event) => {
      if (!tracking || !enabled() || event.touches.length !== 1) {
        tracking = false;
        return;
      }
      // Long press and selection-handle drags belong to the OS. Never turn
      // them into tmux wheel input or prevent the browser selection gesture.
      if (nativeSelection() || (!dragged && Date.now() - startedAt >= 400)) {
        held = true;
        tracking = false;
        return;
      }
      const point = event.touches[0];
      distance += lastY - point.clientY;
      lastY = point.clientY;
      const height =
        host.getBoundingClientRect().height / Math.max(1, term.rows);
      const result = scrollSteps(distance, height);
      if (Math.abs(distance) <= 5 && !dragged) return;
      dragged = true;
      event.preventDefault();
      event.stopPropagation();
      distance = result.remaining;
      if (!result.lines) return;
      if (term.modes.mouseTrackingMode === "none") {
        term.scrollLines(result.lines);
        return;
      }
      // A view can temporarily disable stdin. Allow mouse reporting only
      // during this synchronous wheel dispatch on the enabled terminal.
      const disabled = term.options.disableStdin;
      term.options.disableStdin = false;
      try {
        for (let i = 0; i < Math.abs(result.lines); i++) {
          term.element?.dispatchEvent(
            new WheelEvent("wheel", {
              bubbles: true,
              cancelable: true,
              clientX: point.clientX,
              clientY: point.clientY,
              deltaY: Math.sign(result.lines),
              deltaMode: WheelEvent.DOM_DELTA_LINE,
            }),
          );
        }
      } finally {
        term.options.disableStdin = disabled;
      }
    },
    { passive: false },
  );
  const end = () => {
    tracking = false;
    if (dragged || held || Date.now() - startedAt >= 400)
      suppressClickUntil = Date.now() + 500;
  };
  host.addEventListener("touchend", end, { passive: true });
  host.addEventListener("touchcancel", end, { passive: true });
  host.addEventListener(
    "click",
    (event) => {
      if (Date.now() < suppressClickUntil) {
        event.preventDefault();
        event.stopImmediatePropagation();
      }
    },
    { capture: true },
  );
}
