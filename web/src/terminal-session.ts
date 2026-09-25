import type { Tab } from "./types";

// Open, resize and redraw must use the same gateway/PTY bounds. A short
// viewport can legitimately make xterm report one row.
export function terminalSize(cols: number, rows: number) {
  const bound = (value: number, maximum: number) =>
    Number.isFinite(value)
      ? Math.max(2, Math.min(maximum, Math.floor(value)))
      : 2;
  return { cols: bound(cols, 500), rows: bound(rows, 250) };
}

// FitAddon only emits onResize for a local change. Home may still have the
// dimensions sent at open if the viewport changed while it was starting.
export function syncTerminalSize(tab: Pick<Tab, "term" | "ws" | "status">) {
  if (tab.ws?.readyState !== WebSocket.OPEN || tab.status !== "connected")
    return false;
  tab.ws.send(
    JSON.stringify({
      type: "resize",
      ...terminalSize(tab.term.cols, tab.term.rows),
    }),
  );
  return true;
}

// Release only this browser's disposable view; keep the terminal buffer and
// original tmux session. Advance generation before close to reject stale callbacks.
export function releaseTerminalView(tab: Tab): void {
  tab.nativeInput?.flush();
  tab.nativeInput?.cancel();
  tab.generation++;
  clearTimeout(tab.retryTimer);
  clearTimeout(tab.openTimer);
  tab.retryTimer = tab.openTimer = undefined;
  tab.heartbeat?.dispose();
  tab.heartbeat = undefined;
  tab.recovery?.released();
  tab.ws?.close();
  tab.ws = undefined;
  tab.status = "disconnected";
}
