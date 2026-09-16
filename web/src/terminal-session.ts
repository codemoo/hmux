import type { Tab } from "./types";

// Release only this browser's disposable view; keep the terminal buffer and
// original tmux session. Advance generation before close to reject stale callbacks.
export function releaseTerminalView(tab: Tab): void {
  tab.nativeInput?.flush();
  tab.nativeInput?.cancel();
  tab.generation++;
  tab.ws?.close();
  tab.ws = undefined;
  tab.status = "disconnected";
}
