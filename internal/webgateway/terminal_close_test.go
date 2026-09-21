package webgateway

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"
)

func TestTerminalOverflowIsBoundedAndIsolated(t *testing.T) {
	h := newHub()
	slow := &terminalOutput{frames: make(chan Message, 64)}
	healthy := &terminalOutput{frames: make(chan Message, 64)}
	h.terminals["slow"], h.terminals["healthy"] = slow, healthy
	h.mu.Lock()
	for i := 0; i < 65; i++ {
		h.deliverTerminalLocked(Message{Type: "data", ID: "slow"})
	}
	h.deliverTerminalLocked(Message{Type: "data", ID: "slow"}) // late output is ignored
	h.deliverTerminalLocked(Message{Type: "data", ID: "healthy"})
	h.mu.Unlock()
	count := 0
	for range slow.frames {
		count++
	}
	if count != 64 || slow.closeCode != terminalOutputFull || len(h.terminals) != 1 {
		t.Fatal("overflow lost its cause or leaked the view")
	}
	select {
	case <-healthy.frames:
	default:
		t.Fatal("slow view disrupted another view")
	}
}

func TestTerminalCloseCategories(t *testing.T) {
	if os.Getenv("HMUX_RUN_WEB_SOCKET_TEST") != "1" {
		t.Skip("opt-in isolated fake Home sockets; no tmux")
	}
	for _, tc := range []struct {
		name string
		code websocket.StatusCode
	}{
		{"invalid-resize", websocket.StatusProtocolError},
		{"view-exit", terminalViewExited},
		{"home-offline", terminalHomeOffline},
		{"output-full", terminalOutputFull},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s := testServer(t)
			ts := httptest.NewServer(s)
			defer ts.Close()
			s.origin = ts.URL
			s.host = strings.TrimPrefix(ts.URL, "http://")
			token := loginForTest(t, s)
			ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			defer cancel()
			url := "ws" + strings.TrimPrefix(ts.URL, "http")
			home, _, err := websocket.Dial(ctx, url+"/connect", &websocket.DialOptions{HTTPHeader: http.Header{"Authorization": {"Bearer " + s.token}}})
			if err != nil {
				t.Fatal(err)
			}
			defer home.CloseNow()
			hp := &peer{conn: home}
			opened := make(chan string, 1)
			resized := make(chan Message, 1)
			go func() {
				for {
					m, err := hp.read(ctx)
					if err != nil {
						return
					}
					switch m.Type {
					case "open":
						_ = hp.send(ctx, Message{Type: "response", ID: m.ID, Payload: json.RawMessage(`{"ok":true}`)})
						opened <- m.ID
					case "resize":
						resized <- m
					}
				}
			}()
			terminal, _, err := websocket.Dial(ctx, url+"/api/terminal", &websocket.DialOptions{HTTPHeader: http.Header{"Cookie": {cookieName + "=" + token}, "Origin": {s.origin}}})
			if err != nil {
				t.Fatal(err)
			}
			defer terminal.CloseNow()
			if err := terminal.Write(ctx, websocket.MessageText, []byte(`{"type":"open","session":{"id":"$1","created_at":42},"cols":80,"rows":24}`)); err != nil {
				t.Fatal(err)
			}
			_, raw, err := terminal.Read(ctx)
			if err != nil || !strings.Contains(string(raw), `"type":"ready"`) {
				t.Fatalf("ready: %s %v", raw, err)
			}
			id := <-opened
			// The corrected two-row resize is accepted and forwarded before a failure.
			if err := terminal.Write(ctx, websocket.MessageText, []byte(`{"type":"resize","cols":80,"rows":2}`)); err != nil {
				t.Fatal(err)
			}
			select {
			case m := <-resized:
				if m.Rows != 2 {
					t.Fatal(m.Rows)
				}
			case <-ctx.Done():
				t.Fatal("valid resize not forwarded")
			}
			switch tc.name {
			case "invalid-resize":
				_ = terminal.Write(ctx, websocket.MessageText, []byte(`{"type":"resize","cols":80,"rows":1}`))
			case "view-exit":
				_ = hp.send(ctx, Message{Type: "exit", ID: id, Error: "private Home detail"})
			case "home-offline":
				_ = home.CloseNow()
			case "output-full":
				// Deterministic pressure signal; queue saturation itself is covered above.
				s.hub.mu.Lock()
				s.hub.terminals[id].close(terminalOutputFull)
				delete(s.hub.terminals, id)
				s.hub.mu.Unlock()
			}
			for {
				_, raw, err = terminal.Read(ctx)
				if err == nil {
					continue
				}
				if websocket.CloseStatus(err) != tc.code {
					t.Fatalf("wanted close %d, got %v", tc.code, err)
				}
				if strings.Contains(err.Error(), "private Home detail") {
					t.Fatal("raw Home error leaked")
				}
				break
			}
		})
	}
}
