export function workspaceShortcut(
  event: Pick<
    KeyboardEvent,
    "altKey" | "shiftKey" | "ctrlKey" | "metaKey" | "isComposing" | "code"
  >,
) {
  if (!event.altKey || event.ctrlKey || event.metaKey || event.isComposing)
    return undefined;
  if (!event.shiftKey) {
    if (event.code === "KeyQ") return "logout";
    if (event.code === "KeyW") return "close";
    if (event.code === "KeyL" || event.code === "Backquote") return "sidebar";
    if (/^Digit[1-9]$/.test(event.code))
      return { tabIndex: Number(event.code.slice(-1)) - 1 };
    return undefined;
  }
  if (event.code === "ArrowLeft") return "previous";
  if (event.code === "ArrowRight") return "next";
  return undefined;
}
