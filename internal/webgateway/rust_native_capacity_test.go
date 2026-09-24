package webgateway

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/coder/websocket"
	"golang.org/x/sys/unix"
)

// The surrounding full-gateway fixture owns all processes, paths and sockets.
// This opt-in slice checks the native view limit without touching real tmux.
type fullGatewayCapacityFixture struct {
	root                string
	gatewayPID, homePID int
	open                func() (*websocket.Conn, *int)
	openIdentity        func(int) *websocket.Conn
	readUntil           func(*websocket.Conn, *int, string) []byte
	input               func(*websocket.Conn, string)
	wait                func(string, func() bool)
	noViews             func() bool
	stateOnline         func() bool
}

func fullGatewayCapacity(t *testing.T, f fullGatewayCapacityFixture) {
	t.Helper()
	const limit = 8 // hmux_protocol::wire::MAX_TERMINALS
	viewCount := func() int {
		t.Helper()
		// The synthetic detached hook may reap a finished disposable view.
		_ = f.noViews()
		entries, err := os.ReadDir(filepath.Join(f.root, "views"))
		if err != nil {
			t.Fatal(err)
		}
		return len(entries)
	}
	f.wait("capacity precondition retained a disposable view", func() bool { return viewCount() == 0 })
	fullGatewayResourceSample(t, "capacity-before", f.gatewayPID, f.homePID)
	type view struct {
		conn     *websocket.Conn
		received *int
	}
	views := make([]view, 0, limit)
	defer func() {
		for _, item := range views {
			_ = item.conn.CloseNow()
		}
	}()
	for index := 0; index < limit; index++ {
		conn, received := f.open()
		f.readUntil(conn, received, "HMUX-READY")
		views = append(views, view{conn, received})
	}
	f.wait("eight native views did not become active", func() bool { return viewCount() == limit })
	created := fullGatewayCapacityNewSessions(t, f.root)
	fullGatewayResourceSample(t, "capacity-at-limit", f.gatewayPID, f.homePID)

	ninth := f.openIdentity(42)
	readCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	_, _, err := ninth.Read(readCtx)
	cancel()
	_ = ninth.CloseNow()
	if websocket.CloseStatus(err) != websocket.StatusTryAgainLater {
		t.Fatalf("ninth view: want prompt 1013 capacity close, got %v", err)
	}
	if got := fullGatewayCapacityNewSessions(t, f.root); got != created {
		t.Fatalf("rejected view created a Home PTY: new-session count %d -> %d", created, got)
	}
	if got := viewCount(); got != limit {
		t.Fatalf("rejected view changed live PTY count: got %d, want %d", got, limit)
	}
	for index, item := range views {
		marker := fmt.Sprintf("capacity-%d", index)
		f.input(item.conn, marker+"\n")
		f.readUntil(item.conn, item.received, "INPUT="+marker)
	}
	if !f.stateOnline() {
		t.Fatal("capacity rejection lost original session or Home connection")
	}

	_ = views[0].conn.CloseNow()
	views = views[1:]
	f.wait("closed view did not release capacity", func() bool { return viewCount() == limit-1 })
	replacement, received := f.open()
	f.readUntil(replacement, received, "HMUX-READY")
	views = append(views, view{replacement, received})
	f.wait("replacement view did not use released slot", func() bool { return viewCount() == limit })
	f.input(replacement, "capacity-replacement\n")
	f.readUntil(replacement, received, "INPUT=capacity-replacement")
	fullGatewayResourceSample(t, "capacity-recovered", f.gatewayPID, f.homePID)
	for _, item := range views {
		_ = item.conn.CloseNow()
	}
	views = nil
	f.wait("capacity views retained disposable PTYs", f.noViews)
	if got := fullGatewayCapacityNewSessions(t, f.root); got != created+1 {
		t.Fatalf("capacity run created unexpected PTYs: got %d new-session calls, want %d", got, created+1)
	}
	if !f.stateOnline() {
		t.Fatal("capacity cleanup lost original session or Home connection")
	}
	fullGatewayResourceSample(t, "capacity-after-drain", f.gatewayPID, f.homePID)
	t.Log("native eight-view capacity rejection, surviving echoes, replacement and drain passed")
}

func fullGatewayCapacityNewSessions(t *testing.T, root string) int {
	t.Helper()
	file, err := os.Open(filepath.Join(root, "commands.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	if err := unix.Flock(int(file.Fd()), unix.LOCK_SH); err != nil {
		t.Fatal(err)
	}
	defer unix.Flock(int(file.Fd()), unix.LOCK_UN) //nolint:errcheck
	// The fixture's own command log stays small in this no-churn mode.
	info, err := file.Stat()
	if err != nil || info.Size() > 1<<20 {
		t.Fatalf("capacity command log unavailable or oversized: %v", err)
	}
	scanner := bufio.NewScanner(io.LimitReader(file, 1<<20))
	count := 0
	for scanner.Scan() {
		var args []string
		if err := json.Unmarshal(bytes.TrimSpace(scanner.Bytes()), &args); err != nil {
			t.Fatal(err)
		}
		if len(args) > 0 && args[0] == "new-session" {
			count++
		}
	}
	if err := scanner.Err(); err != nil {
		t.Fatal(err)
	}
	return count
}
