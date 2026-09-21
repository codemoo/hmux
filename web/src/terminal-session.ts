import type { Tab } from "./types";

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
