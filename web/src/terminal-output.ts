// xterm.reset() does not discard pending writes. Keep the byte budget attached
// to the terminal, across socket generations, and drain before reopening it.
export function createTerminalOutput(
  write: (data: Uint8Array, done: () => void) => void,
  drained: () => void,
) {
  let bytes = 0;
  return {
    pending: () => bytes,
    enqueue(data: Uint8Array) {
      if (bytes + data.byteLength > 1 << 20) return false;
      bytes += data.byteLength;
      write(data, () => {
        bytes -= data.byteLength;
        if (bytes === 0) drained();
      });
      return true;
    },
  };
}
