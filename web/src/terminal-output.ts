// xterm.reset() does not discard pending writes. Keep the byte budget attached
// to the terminal, across socket generations, and drain before reopening it.
export function createTerminalOutput(
  write: (data: Uint8Array, done: () => void) => void,
  drained: () => void,
) {
  let bytes = 0;
  return {
    pending: () => bytes,
    enqueue(data: Uint8Array, rendered?: () => void) {
      if (bytes + data.byteLength > 1 << 20) return false;
      bytes += data.byteLength;
      // Pinned xterm 6 consumes its write buffer and callbacks in FIFO order.
      write(data, () => {
        bytes -= data.byteLength;
        try {
          rendered?.();
        } finally {
          if (bytes === 0) drained();
        }
      });
      return true;
    },
  };
}
