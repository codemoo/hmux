import type { Terminal } from "@xterm/xterm";

// Native editable controls own long press and selection. Share this across iOS
// and Android without intercepting input, composition or the actual paste event.
export function preserveNativeEditableGestures(
  host: HTMLElement,
  textarea: HTMLTextAreaElement,
) {
  const types = [
    "touchstart",
    "touchmove",
    "touchend",
    "touchcancel",
    "mousedown",
    "mouseup",
    "click",
    "contextmenu",
  ];
  const preserve = (event: Event) => {
    if (event.target === textarea) event.stopImmediatePropagation();
  };
  for (const type of types) host.addEventListener(type, preserve, true);
  return () => {
    for (const type of types) host.removeEventListener(type, preserve, true);
  };
}

export function hasNativeSelection(host: HTMLElement) {
  const selection = host.ownerDocument?.getSelection();
  return (
    !!selection &&
    !selection.isCollapsed &&
    !!selection.anchorNode &&
    !!selection.focusNode &&
    host.contains(selection.anchorNode) &&
    host.contains(selection.focusNode)
  );
}

// Use the browser's selection handles, Copy menu and clipboard payload. Do not
// rebuild text in a dialog, read Clipboard API automatically, or emulate menus.
export function installNativeClipboard(term: Terminal, host: HTMLElement) {
  term.element!.classList.add("native-clipboard");
  let lastTouch = -Infinity;
  let touchEnded = -Infinity;
  let touchActive = false;
  let moved = false;
  let startX = 0;
  let startY = 0;
  let suppressFocus = false;
  const recentTouch = () => touchActive || Date.now() - touchEnded < 1000;
  host.addEventListener(
    "touchstart",
    (event) => {
      lastTouch = Date.now();
      touchActive = true;
      moved = event.touches?.length > 1;
      startX = event.touches?.[0]?.clientX ?? 0;
      startY = event.touches?.[0]?.clientY ?? 0;
      suppressFocus = false;
    },
    { passive: true },
  );
  host.addEventListener(
    "touchmove",
    (event) => {
      const point = event.touches?.[0];
      if (
        point &&
        Math.hypot(point.clientX - startX, point.clientY - startY) > 5
      )
        moved = true;
    },
    { passive: true },
  );
  const endTouch = () => {
    touchEnded = Date.now();
    suppressFocus = moved || touchEnded - lastTouch >= 350;
    touchActive = false;
  };
  host.addEventListener("touchend", endTouch, { passive: true });
  host.addEventListener("touchcancel", endTouch, { passive: true });
  host.addEventListener(
    "mousedown",
    (event) => {
      // A compatibility mousedown after touch must not let xterm prevent native
      // word selection or focus its hidden input over the browser's selected text.
      if (recentTouch()) event.stopImmediatePropagation();
    },
    true,
  );
  host.addEventListener(
    "contextmenu",
    (event) => {
      // iOS may dispatch contextmenu before exposing its selected range. While
      // the textarea is focused, xterm's desktop helper would overwrite/select
      // that textarea and steal selection from the rendered terminal text.
      if (recentTouch() || hasNativeSelection(host))
        event.stopImmediatePropagation();
    },
    true,
  );
  host.addEventListener(
    "copy",
    (event) => {
      if (!hasNativeSelection(host)) return;
      // Do not override browser serialization (including native range/newlines).
      // Only stop xterm from substituting its separate internal selection.
      event.stopImmediatePropagation();
    },
    true,
  );
  host.addEventListener(
    "click",
    (event) => {
      if (hasNativeSelection(host) || (recentTouch() && suppressFocus))
        event.stopImmediatePropagation();
      else if (recentTouch()) term.focus();
    },
    true,
  );
}
