package webgateway

import (
	"context"
	"errors"
	"io"
	"sync"
	"time"
)

const terminalFlowCapability = "terminal-output-flow-v1"
const terminalOutputChunk = 16 << 10
const terminalOutputFrames = 32
const terminalOutputBytes = terminalOutputChunk * terminalOutputFrames
const terminalOutputStall = 30 * time.Second

var errTerminalOutputStalled = errors.New("terminal rendering stalled")

func hasTerminalFlow(capabilities []string) bool {
	for _, c := range capabilities {
		if c == terminalFlowCapability {
			return true
		}
	}
	return false
}

// Both bytes and frames are bounded: tiny PTY reads must not exhaust the
// gateway's frame queue. Each ACK retires exactly one rendered frame, in order.
type outputFrame struct {
	bytes  int
	queued time.Time
}

type outputWindow struct {
	mu      sync.Mutex
	frames  []outputFrame
	bytes   int
	changed chan struct{}
}

func newOutputWindow() *outputWindow {
	return &outputWindow{changed: make(chan struct{}, 1)}
}

func (w *outputWindow) add(n int) bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	if n <= 0 || n > terminalOutputChunk || len(w.frames) >= terminalOutputFrames || w.bytes+n > terminalOutputBytes {
		return false
	}
	w.frames = append(w.frames, outputFrame{n, time.Now()})
	w.bytes += n
	return true
}

func (w *outputWindow) wait(ctx context.Context, done <-chan struct{}, n int) error {
	for {
		if ctx.Err() != nil {
			return ctx.Err()
		}
		select {
		case <-done:
			return io.EOF
		default:
		}
		if w.add(n) {
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-done:
			return io.EOF
		case <-w.changed:
		}
	}
}

func (w *outputWindow) acknowledge(n int64) bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	if len(w.frames) == 0 || n != int64(w.frames[0].bytes) {
		return false
	}
	w.bytes -= w.frames[0].bytes
	w.frames = w.frames[1:]
	select {
	case w.changed <- struct{}{}:
	default:
	}
	return true
}

// Waiting affects only this disposable view. The shared Home reader continues
// handling input, ACKs, catalog requests and other terminals. At most one extra
// PTY chunk is held while waiting for rendering credit.
func streamTerminalOutput(ctx context.Context, done <-chan struct{}, reader io.Reader, window *outputWindow, send func([]byte) error) error {
	buf := make([]byte, terminalOutputChunk)
	for {
		n, err := reader.Read(buf)
		if n > 0 {
			if window != nil {
				waitCtx, cancel := context.WithTimeout(ctx, terminalOutputStall+10*time.Second)
				e := window.wait(waitCtx, done, n)
				cancel()
				if e != nil {
					if errors.Is(e, context.DeadlineExceeded) && ctx.Err() == nil {
						return errTerminalOutputStalled
					}
					return e
				}
			}
			if e := send(buf[:n]); e != nil {
				return e
			}
		}
		if err != nil {
			return err
		}
		if ctx.Err() != nil {
			return ctx.Err()
		}
	}
}

func (w *outputWindow) stalled(now time.Time) bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	return len(w.frames) > 0 && now.Sub(w.frames[0].queued) >= terminalOutputStall
}
