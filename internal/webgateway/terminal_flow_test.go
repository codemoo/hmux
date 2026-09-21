package webgateway

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/coder/websocket"
)

var errOutputAcknowledgement = errors.New("invalid terminal output acknowledgement")

func TestOutputWindowBoundsAndCancellation(t *testing.T) {
	for _, size := range []int{1, terminalOutputChunk} {
		w := newOutputWindow()
		for i := 0; i < terminalOutputFrames; i++ {
			if !w.add(size) {
				t.Fatal("early saturation")
			}
		}
		if w.add(size) || w.acknowledge(0) || w.acknowledge(int64(size+1)) {
			t.Fatal("invalid credit accepted")
		}
		ctx, cancel := context.WithCancel(context.Background())
		waiting := make(chan error, 1)
		go func() { waiting <- w.wait(ctx, nil, size) }()
		select {
		case <-waiting:
			t.Fatal("window did not block")
		case <-time.After(20 * time.Millisecond):
		}
		if !w.acknowledge(int64(size)) {
			t.Fatal("valid ACK rejected")
		}
		select {
		case err := <-waiting:
			if err != nil {
				t.Fatal(err)
			}
		case <-time.After(time.Second):
			t.Fatal("ACK did not resume")
		}
		go func() { waiting <- w.wait(ctx, nil, size) }()
		cancel()
		select {
		case err := <-waiting:
			if err != context.Canceled {
				t.Fatal(err)
			}
		case <-time.After(time.Second):
			t.Fatal("cancel did not unblock")
		}
		done := make(chan struct{})
		close(done)
		if err := w.wait(context.Background(), done, size); err != io.EOF {
			t.Fatal(err)
		}
	}
}

// A PTY-like reader with thousands of tiny frames followed by substantial output.
// It catches frame-count saturation that a byte-only budget misses.
type outputFixture struct{ tiny, large int }

func (r *outputFixture) Read(p []byte) (int, error) {
	if r.tiny > 0 {
		r.tiny--
		p[0] = byte(r.tiny)
		return 1, nil
	}
	if r.large > 0 {
		r.large--
		for i := range p {
			p[i] = byte(i + r.large)
		}
		return len(p), nil
	}
	return 0, io.EOF
}

func TestTerminalRenderedFlowOverWebSocket(t *testing.T) {
	if os.Getenv("HMUX_RUN_WEB_SOCKET_TEST") != "1" {
		t.Skip("isolated fake Home; no tmux")
	}
	s := testServer(t)
	ts := httptest.NewServer(s)
	defer ts.Close()
	defer s.Close()
	s.origin, s.host = ts.URL, strings.TrimPrefix(ts.URL, "http://")
	token := loginForTest(t, s)
	ctx, cancel := context.WithTimeout(context.Background(), 75*time.Second)
	defer cancel()
	url := "ws" + strings.TrimPrefix(ts.URL, "http")
	home, _, err := websocket.Dial(ctx, url+"/connect", &websocket.DialOptions{HTTPHeader: http.Header{"Authorization": {"Bearer " + s.token}}})
	if err != nil {
		t.Fatal(err)
	}
	defer home.CloseNow()
	hp := &peer{conn: home}
	if err := hp.send(ctx, Message{Type: "hello", Capabilities: []string{terminalFlowCapability}}); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(time.Second)
	for {
		s.hub.mu.Lock()
		capable := s.hub.outputCap
		s.hub.mu.Unlock()
		if capable {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("capability not registered")
		}
		time.Sleep(time.Millisecond)
	}
	var mu sync.Mutex
	windows := map[string]*outputWindow{}
	stops := map[string]context.CancelFunc{}
	var sent atomic.Int64
	errors := make(chan error, 8)
	resized := make(chan struct{}, 1)
	go func() {
		for {
			m, err := hp.read(ctx)
			if err != nil {
				return
			}
			switch m.Type {
			case "open":
				if !hasTerminalFlow(m.Capabilities) {
					errors <- errOutputAcknowledgement
					return
				}
				w := newOutputWindow()
				run, stop := context.WithCancel(ctx)
				mu.Lock()
				windows[m.ID], stops[m.ID] = w, stop
				mu.Unlock()
				if err := hp.send(ctx, Message{Type: "response", ID: m.ID, Payload: json.RawMessage(`{"ok":true}`)}); err != nil {
					errors <- err
					return
				}
				go func(id string) {
					err := streamTerminalOutput(run, nil, &outputFixture{4096, 1024}, w, func(data []byte) error {
						sent.Add(1)
						return hp.send(run, Message{Type: "data", ID: id, Data: data})
					})
					if err != nil && err != io.EOF && run.Err() == nil {
						errors <- err
					}
				}(m.ID)
			case "output-ack":
				mu.Lock()
				w := windows[m.ID]
				mu.Unlock()
				if w == nil || !w.acknowledge(m.Received) {
					errors <- errOutputAcknowledgement
					return
				}
			case "resize":
				resized <- struct{}{}
			case "close":
				mu.Lock()
				stop := stops[m.ID]
				mu.Unlock()
				if stop != nil {
					stop()
				}
			}
		}
	}()
	dial := func(flow bool) *websocket.Conn {
		t.Helper()
		c, _, err := websocket.Dial(ctx, url+"/api/terminal", &websocket.DialOptions{HTTPHeader: http.Header{"Cookie": {cookieName + "=" + token}, "Origin": {s.origin}}})
		if err != nil {
			t.Fatal(err)
		}
		m := Message{Type: "open", Cols: 80, Rows: 24}
		m.Session.ID, m.Session.CreatedAt = "$1", 42
		if flow {
			m.Capabilities = []string{terminalFlowCapability}
		}
		raw, _ := json.Marshal(m)
		if err := c.Write(ctx, websocket.MessageText, raw); err != nil {
			t.Fatal(err)
		}
		_, raw, err = c.Read(ctx)
		var ready struct {
			OutputFlow bool `json:"output_flow"`
		}
		if err != nil || json.Unmarshal(raw, &ready) != nil || ready.OutputFlow != flow {
			t.Fatalf("ready: %s %v", raw, err)
		}
		return c
	}
	slow := dial(true)
	defer slow.CloseNow()
	// Hold all rendering credits for more than one app heartbeat. Home must stop
	// at 32 tiny frames while control traffic and another view continue to work.
	time.Sleep(100 * time.Millisecond)
	if n := sent.Load(); n != terminalOutputFrames {
		t.Fatalf("unbounded producer: %d", n)
	}
	if err := slow.Write(ctx, websocket.MessageText, []byte(`{"type":"resize","cols":100,"rows":30}`)); err != nil {
		t.Fatal(err)
	}
	select {
	case <-resized:
	case <-ctx.Done():
		t.Fatal("control stalled")
	}
	healthy := dial(true)
	defer healthy.CloseNow()
	expected := sha256.New()
	buf := make([]byte, terminalOutputChunk)
	fixture := &outputFixture{4096, 1024}
	for {
		n, e := fixture.Read(buf)
		expected.Write(buf[:n])
		if e != nil {
			break
		}
	}
	consume := func(c *websocket.Conn, ack bool) {
		t.Helper()
		hash := sha256.New()
		for count := 0; count < 5120; {
			kind, data, err := c.Read(ctx)
			if err != nil {
				t.Fatal(err)
			}
			if kind != websocket.MessageBinary {
				continue
			}
			hash.Write(data)
			count++
			if ack {
				raw, _ := json.Marshal(Message{Type: "output-ack", Received: int64(len(data))})
				if err := c.Write(ctx, websocket.MessageText, raw); err != nil {
					t.Fatal(err)
				}
			}
		}
		if !bytes.Equal(hash.Sum(nil), expected.Sum(nil)) {
			t.Fatal("output dropped, duplicated or reordered")
		}
	}
	s.auth.mu.Lock()
	beforeACK := s.auth.sessions[sessionKey(token)].seen
	s.auth.mu.Unlock()
	consume(healthy, true)
	s.auth.mu.Lock()
	afterACK := s.auth.sessions[sessionKey(token)].seen
	s.auth.mu.Unlock()
	if !afterACK.Equal(beforeACK) {
		t.Fatal("passive rendering credits extended login activity")
	}
	// The stalled view still receives application heartbeat rather than a close.
	heartbeatSeen := false
	for count := 0; count <= terminalOutputFrames; count++ {
		kind, raw, err := slow.Read(ctx)
		if err != nil {
			t.Fatal(err)
		}
		if kind == websocket.MessageText && strings.Contains(string(raw), "heartbeat") {
			heartbeatSeen = true
			break
		}
	}
	if !heartbeatSeen {
		t.Fatal("stalled renderer lost heartbeat")
	}
	// Retire the 32 tiny frames already read, then verify the remaining stream.
	for i := 0; i < terminalOutputFrames; i++ {
		if err := slow.Write(ctx, websocket.MessageText, []byte(`{"type":"output-ack","received":1}`)); err != nil {
			t.Fatal(err)
		}
	}
	hash := sha256.New()
	prefix := &outputFixture{4096, 1024}
	for i := 0; i < terminalOutputFrames; i++ {
		n, _ := prefix.Read(buf)
		hash.Write(buf[:n])
	}
	for count := terminalOutputFrames; count < 5120; {
		kind, data, err := slow.Read(ctx)
		if err != nil {
			t.Fatal(err)
		}
		if kind != websocket.MessageBinary {
			continue
		}
		hash.Write(data)
		count++
		raw, _ := json.Marshal(Message{Type: "output-ack", Received: int64(len(data))})
		if err := slow.Write(ctx, websocket.MessageText, raw); err != nil {
			t.Fatal(err)
		}
	}
	if !bytes.Equal(hash.Sum(nil), expected.Sum(nil)) {
		t.Fatal("slow output mismatch")
	}
	// A legacy browser still works with a new Home, paced at gateway writes.
	legacy := dial(false)
	defer legacy.CloseNow()
	consume(legacy, false)
	// A fabricated credit is rejected at the browser boundary, scoped to its view.
	if err := slow.Write(ctx, websocket.MessageText, []byte(`{"type":"output-ack","received":0}`)); err != nil {
		t.Fatal(err)
	}
	for {
		_, _, err := slow.Read(ctx)
		if err != nil {
			if websocket.CloseStatus(err) != websocket.StatusProtocolError {
				t.Fatal(err)
			}
			break
		}
	}
	// Keep reading healthy idle sockets so WebSocket ping/pong remains live
	// throughout the stalled renderer deadline.
	go func() {
		for {
			if _, _, err := healthy.Read(ctx); err != nil {
				return
			}
		}
	}()
	go func() {
		for {
			if _, _, err := legacy.Read(ctx); err != nil {
				return
			}
		}
	}()
	frozen := dial(true)
	defer frozen.CloseNow()
	deadline = time.Now().Add(terminalOutputStall + 7*time.Second)
	for {
		_, _, err := frozen.Read(ctx)
		if err != nil {
			if websocket.CloseStatus(err) != terminalOutputFull {
				t.Fatalf("frozen rendering: %v", err)
			}
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("frozen renderer was not released")
		}
	}
	// Stalled rendering must not close the shared Home or a healthy view.
	if err := healthy.Write(ctx, websocket.MessageText, []byte(`{"type":"resize","cols":100,"rows":30}`)); err != nil {
		t.Fatal(err)
	}
	select {
	case <-resized:
	case <-ctx.Done():
		t.Fatal("stalled renderer disrupted another view")
	}

	select {
	case err := <-errors:
		t.Fatal(err)
	default:
	}
}

func TestOutputWindowStallDeadline(t *testing.T) {
	w := newOutputWindow()
	if w.stalled(time.Now().Add(time.Hour)) {
		t.Fatal("idle view is not a stalled renderer")
	}
	w.add(1)
	if w.stalled(time.Now()) || !w.stalled(time.Now().Add(terminalOutputStall)) {
		t.Fatal("missing rendering deadline")
	}
	w.mu.Lock()
	w.frames[0].queued = time.Now().Add(-terminalOutputStall)
	w.mu.Unlock()
	if w.acknowledge(2) || !w.stalled(time.Now()) {
		t.Fatal("invalid credit reset deadline")
	}
	w.add(1)
	if !w.acknowledge(1) || w.stalled(time.Now()) {
		t.Fatal("valid progress failed to reset deadline")
	}
	w.acknowledge(1)
	if w.stalled(time.Now().Add(time.Hour)) {
		t.Fatal("drained output kept deadline")
	}
}

func TestTerminalHomeGenerationBinding(t *testing.T) {
	h := newHub()
	previous := &peer{}
	h.home = &peer{} // replacement has no connection: calling send would panic
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	for _, kind := range []string{"open", "output-ack", "close"} {
		if err := h.sendTo(ctx, previous, Message{Type: kind, ID: "old-view", Received: 1}); err == nil {
			t.Fatal("stale view reached replacement Home")
		}
	}
	if _, err := h.requestTo(ctx, Message{Type: "open", ID: "old-view"}, previous); err == nil {
		t.Fatal("open used a different Home capability")
	}
	if len(h.pending) != 0 {
		t.Fatal("failed open leaked pending request")
	}
}

func TestOutputWindowTrickleCannotKeepOldOutputAlive(t *testing.T) {
	w := newOutputWindow()
	w.add(1)
	w.add(2)
	w.mu.Lock()
	for i := range w.frames {
		w.frames[i].queued = time.Now().Add(-terminalOutputStall)
	}
	w.mu.Unlock()
	if !w.acknowledge(1) || !w.stalled(time.Now()) {
		t.Fatal("trickle ACK kept old output alive")
	}
}

func TestOutputSendDeadlineIsNotRenderingStall(t *testing.T) {
	err := streamTerminalOutput(context.Background(), nil, bytes.NewReader([]byte("output")), newOutputWindow(), func([]byte) error { return context.DeadlineExceeded })
	if err != context.DeadlineExceeded || errors.Is(err, errTerminalOutputStalled) {
		t.Fatal("send deadline mislabeled as rendering stall", err)
	}
}
