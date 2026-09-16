export const terminalFontFamily =
  '"HMux Mono", "Monatendard Nerd Font Mono", "Apple SD Gothic Neo", monospace';
export function preferredFontSize(mobile: boolean, stored: string | null) {
  const value = stored === null ? (mobile ? 10 : 14) : Number(stored);
  return Math.min(
    24,
    Math.max(8, Number.isFinite(value) ? value : mobile ? 10 : 14),
  );
}
export function viewportGeometry(height: number, offsetTop: number) {
  return {
    height: Math.max(1, Math.round(height)),
    top: Math.max(0, Math.round(offsetTop)),
  };
}

// visualViewport may already exclude part of the device's bottom inset.
export function remainingBottomInset(
  inset: number,
  layoutHeight: number,
  visibleHeight: number,
  offsetTop: number,
) {
  const excluded = Math.max(0, layoutHeight - visibleHeight - offsetTop);
  return Math.max(0, inset - excluded);
}

// Android resize-content shrinks both viewports, so their difference is zero.
// Keep an orientation-scoped reference instead of subtracting keyboard height.
export function createKeyboardTracker() {
  let orientationKey = "";
  let expandedHeight = 0;
  let visible = false;
  return (orientation: string, height: number, editing: boolean) => {
    if (orientation !== orientationKey) {
      orientationKey = orientation;
      expandedHeight = height;
      visible = false;
    }
    expandedHeight = Math.max(expandedHeight, height);
    const reduced =
      expandedHeight - height > Math.max(120, expandedHeight * 0.18);
    visible = reduced && (editing || visible);
    return visible;
  };
}
