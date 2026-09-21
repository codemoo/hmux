// Browser WebSocket does not expose protocol pings. The gateway advertises and
// sends application heartbeats so a silent half-open socket can be replaced.
export function createTerminalHeartbeat(expired: () => void) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let disposed = false;
  const received = () => {
    if (disposed) return;
    clearTimeout(timer);
    timer = setTimeout(() => {
      disposed = true;
      timer = undefined;
      expired();
    }, 20_000);
  };
  received();
  return {
    received,
    dispose() {
      disposed = true;
      clearTimeout(timer);
      timer = undefined;
    },
  };
}
