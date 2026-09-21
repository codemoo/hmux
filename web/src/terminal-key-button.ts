// Keep the terminal textarea focused while tapping its accessory keys. Cancel
// touchend before Safari synthesizes a focus-changing click; mouse and keyboard
// activation still use the button's normal click path.
export function bindTerminalKeyButton(
  button: HTMLButtonElement,
  activate: () => void,
) {
  let start: { x: number; y: number; id: number } | undefined;
  let moved = false;
  let lastTouch = -Infinity;
  button.onpointerdown = (event) => event.preventDefault();
  button.addEventListener(
    "touchstart",
    (event) => {
      const touch = event.touches[0];
      start =
        event.touches.length === 1
          ? { x: touch.clientX, y: touch.clientY, id: touch.identifier }
          : undefined;
      moved = false;
    },
    { passive: true },
  );
  button.addEventListener(
    "touchmove",
    (event) => {
      const touch = Array.from(event.touches).find(
        (touch) => touch.identifier === start?.id,
      );
      if (
        !start ||
        !touch ||
        event.touches.length !== 1 ||
        Math.hypot(touch.clientX - start.x, touch.clientY - start.y) > 8
      )
        moved = true;
    },
    { passive: true },
  );
  button.addEventListener(
    "touchend",
    (event) => {
      lastTouch = Date.now();
      event.preventDefault();
      const touch = Array.from(event.changedTouches).find(
        (touch) => touch.identifier === start?.id,
      );
      const tapped =
        start &&
        touch &&
        !moved &&
        event.touches.length === 0 &&
        Math.hypot(touch.clientX - start.x, touch.clientY - start.y) <= 8;
      start = undefined;
      if (tapped && !button.disabled) activate();
    },
    { passive: false },
  );
  button.addEventListener("touchcancel", () => {
    start = undefined;
    lastTouch = Date.now();
  });
  button.onclick = (event) => {
    // Some engines still deliver a compatibility click after a handled touch.
    // detail=0 is keyboard/assistive activation and must remain available.
    if (event.detail > 0 && Date.now() - lastTouch < 750) return;
    if (!button.disabled) activate();
  };
}
