import {
  viewportGeometry,
  remainingBottomInset,
  createKeyboardTracker,
} from "./mobile";

export function createViewportController(
  app: HTMLElement,
  isIOS: boolean,
  isAndroid: boolean,
) {
  // Read the physical inset independently of our retained keyboard inset.
  const insetProbe = document.createElement("div");
  insetProbe.style.cssText =
    "position:fixed;visibility:hidden;pointer-events:none;padding-top:env(safe-area-inset-top,0px);padding-bottom:env(safe-area-inset-bottom,0px)";
  document.body.append(insetProbe);
  let insetOrientation = "";
  const androidKeyboardVisible = createKeyboardTracker();
  function update() {
    const orientation =
      screen.orientation?.type ||
      (screen.height >= screen.width ? "portrait" : "landscape");
    if (orientation !== insetOrientation) {
      insetOrientation = orientation;
      document.documentElement.style.removeProperty("--device-safe-top");
    }
    const physicalInsets = getComputedStyle(insetProbe);
    const inset = parseFloat(physicalInsets.paddingTop);
    if (inset > 0)
      document.documentElement.style.setProperty(
        "--device-safe-top",
        `${inset}px`,
      );
    const viewport = window.visualViewport;
    const geometry = viewportGeometry(
      viewport?.height ?? window.innerHeight,
      viewport?.offsetTop ?? 0,
    );
    document.documentElement.style.setProperty(
      "--device-safe-bottom",
      `${remainingBottomInset(
        parseFloat(physicalInsets.paddingBottom) || 0,
        Math.max(window.innerHeight, document.documentElement.clientHeight),
        viewport?.height ?? window.innerHeight,
        viewport?.offsetTop ?? 0,
      )}px`,
    );
    document.documentElement.style.setProperty(
      "--viewport-height",
      `${geometry.height}px`,
    );
    document.documentElement.style.setProperty(
      "--viewport-top",
      `${geometry.top}px`,
    );
    document.documentElement.classList.toggle(
      "keyboard-visible",
      Math.max(window.innerHeight, document.documentElement.clientHeight) -
        geometry.height >
        100 ||
        (isAndroid &&
          androidKeyboardVisible(
            orientation,
            geometry.height,
            (document.activeElement instanceof HTMLInputElement &&
              ![
                "range",
                "checkbox",
                "radio",
                "button",
                "submit",
                "reset",
                "color",
                "file",
                "hidden",
              ].includes(document.activeElement.type)) ||
              document.activeElement instanceof HTMLTextAreaElement ||
              (document.activeElement instanceof HTMLElement &&
                document.activeElement.isContentEditable),
          )) ||
        (isIOS &&
          document.activeElement?.classList.contains("xterm-helper-textarea") &&
          geometry.height < screen.height * 0.72),
    );
    if (!document.documentElement.classList.contains("keyboard-visible")) {
      app.classList.remove("floating-tabs-open");
      document
        .querySelector("#floating-tabs")
        ?.setAttribute("aria-expanded", "false");
    }
    return document.documentElement.classList.contains("keyboard-visible");
  }

  return { update };
}
