package webgateway

// Opt-in elapsed-time evidence. Synthetic tools never touch the user's tmux.
import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"sync"
	"testing"
	"time"

	"github.com/coder/websocket"
	"golang.org/x/sys/unix"
)

func fullGatewaySoakDuration(t *testing.T) time.Duration {
	t.Helper()
	value := os.Getenv("HMUX_NATIVE_SOAK_SECONDS")
	if value == "" {
		return 0
	}
	seconds, err := strconv.Atoi(value)
	if err != nil || seconds < 10 || seconds > 72*60*60 {
		t.Fatal("HMUX_NATIVE_SOAK_SECONDS must be 10..259200")
	}
	return time.Duration(seconds) * time.Second
}

// Keep only a bounded diagnostic tail, even if a failing candidate logs forever.
type fullGatewayBoundedLog struct {
	mu   sync.Mutex
	tail []byte
}

func (b *fullGatewayBoundedLog) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	n := len(p)
	const limit = 64 << 10
	if n >= limit {
		b.tail = append(b.tail[:0], p[n-limit:]...)
	} else {
		if len(b.tail)+n > limit {
			copy(b.tail, b.tail[len(b.tail)+n-limit:])
			b.tail = b.tail[:limit-n]
		}
		b.tail = append(b.tail, p...)
	}
	return n, nil
}

func (b *fullGatewayBoundedLog) Reset() {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.tail = b.tail[:0]
}

func (b *fullGatewayBoundedLog) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return string(b.tail)
}

func fullGatewayRecordCommand(root string, args []string) {
	// Poll results have their own live catalog assertion. Keep all mutation
	// attempts for the final safety audit without retaining 72h of repeated reads.
	if os.Getenv("HMUX_GO_E2E_SOAK") == "1" {
		switch args[0] {
		case "list-sessions", "list-windows", "list-panes", "display-message":
			return
		}
	}
	record, _ := json.Marshal(args)
	file, err := os.OpenFile(filepath.Join(root, "commands.jsonl"), os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0600)
	if err != nil {
		os.Exit(82)
	}
	defer file.Close()
	if unix.Flock(int(file.Fd()), unix.LOCK_EX) != nil {
		os.Exit(82)
	}
	defer unix.Flock(int(file.Fd()), unix.LOCK_UN) //nolint:errcheck
	info, err := file.Stat()
	if err != nil || info.Size()+int64(len(record)+1) > 32<<20 {
		os.Exit(82)
	}
	if _, err = file.Write(append(record, '\n')); err != nil {
		os.Exit(82)
	}
}

func fullGatewaySoak(t *testing.T, ctx context.Context, duration time.Duration, root string, gatewayPID, homePID int,
	open func() (*websocket.Conn, *int), read func(*websocket.Conn, *int, string) []byte,
	input func(*websocket.Conn, string), online func() bool,
	wait func(string, func() bool), noViews func() bool) {
	t.Helper()
	view, received := open()
	defer view.CloseNow()
	read(view, received, "HMUX-READY")
	clockTicks := fullGatewayClockTicks(t)
	started := time.Now()
	deadline := started.Add(duration)
	// At most 289 checkpoints and 4,320 transient views over the maximum 72h.
	checkpointInterval := max(time.Second, (duration+287)/288)
	transientInterval := time.Minute
	if duration < time.Minute {
		transientInterval = 5 * time.Second
	}
	nextCheckpoint, nextTransient := started, started
	lastCheck, lastTransient := started, started
	echoes, transients := 0, 0
	var maxRTT, maxCheckGap, maxTransientGap time.Duration
	checkpoint := func(stage string) {
		fullGatewayPerfResourceSample(t, stage, gatewayPID, homePID, clockTicks)
		t.Logf("native-soak-progress %s", fullGatewayPerfJSON(t, map[string]any{
			"elapsed_seconds": time.Since(started).Seconds(), "requested_seconds": duration.Seconds(),
			"echoes": echoes, "transient_views": transients, "max_socket_rtt_ms": float64(maxRTT) / float64(time.Millisecond),
			"max_check_gap_seconds": maxCheckGap.Seconds(), "max_transient_gap_seconds": maxTransientGap.Seconds(),
		}))
	}
	for time.Now().Before(deadline) {
		if !online() {
			t.Fatal("soak lost authenticated catalog or session identity")
		}
		payload := fmt.Sprintf("soak-%d", echoes)
		before := time.Now()
		input(view, payload+"\n")
		read(view, received, "INPUT="+payload)
		maxRTT = max(maxRTT, time.Since(before))
		echoes++
		if !time.Now().Before(nextTransient) {
			func() {
				other, count := open()
				defer other.CloseNow()
				read(other, count, "HMUX-READY")
				input(other, "transient-"+payload+"\n")
				read(other, count, "INPUT=transient-"+payload)
			}()
			transients++
			wait("soak retained a closed transient PTY", func() bool {
				entries, err := os.ReadDir(filepath.Join(root, "views"))
				return err == nil && len(entries) == 1
			})
			maxTransientGap = max(maxTransientGap, time.Since(lastTransient))
			lastTransient = time.Now()
			if maxTransientGap > transientInterval+5*time.Second {
				t.Fatal("soak transient view cadence exceeded tolerance")
			}
			nextTransient = time.Now().Add(transientInterval)
		}
		maxCheckGap = max(maxCheckGap, time.Since(lastCheck))
		lastCheck = time.Now()
		if maxCheckGap > 5*time.Second {
			t.Fatal("soak authenticated echo stalled for more than five seconds")
		}
		if !time.Now().Before(nextCheckpoint) {
			checkpoint("soak-checkpoint")
			nextCheckpoint = time.Now().Add(checkpointInterval)
		}
		delay := min(time.Second, time.Until(deadline))
		if delay > 0 {
			select {
			case <-ctx.Done():
				t.Fatal("soak context ended before requested duration")
			case <-time.After(delay):
			}
		}
	}
	if echoes < max(1, int(duration.Seconds()/2)) || transients < max(1, int(duration/(transientInterval+5*time.Second))) {
		t.Fatal("soak completed too few checks for the requested duration")
	}
	checkpoint("soak-end")
	_ = view.CloseNow()
	wait("soak retained persistent PTY", noViews)
	t.Logf("native-soak-result %s", fullGatewayPerfJSON(t, map[string]any{
		"requested_seconds": duration.Seconds(), "elapsed_seconds": time.Since(started).Seconds(),
		"echoes": echoes, "transient_views": transients, "errors": 0,
		"max_check_gap_seconds": maxCheckGap.Seconds(), "max_transient_gap_seconds": maxTransientGap.Seconds(),
	}))
}
