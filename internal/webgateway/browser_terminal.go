package webgateway

import (
	"context"
	"encoding/json"
	"net/http"
	"time"

	"github.com/codemoo/hmux/internal/model"
	"github.com/coder/websocket"
)

func validSize(m Message) bool { return m.Cols >= 2 && m.Cols <= 500 && m.Rows >= 2 && m.Rows <= 250 }

func (s *Server) terminal(w http.ResponseWriter, r *http.Request, token string, done <-chan struct{}) {
	if r.Method != http.MethodGet || r.Header.Get("Origin") != s.origin {
		http.Error(w, "Forbidden", 403)
		return
	}
	conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{OriginPatterns: []string{s.host}})
	if err != nil {
		return
	}
	defer conn.CloseNow()
	conn.SetReadLimit(64 << 10)
	ctx, cancel := context.WithCancel(r.Context())
	defer cancel()
	go func() {
		select {
		case <-done:
			cancel()
		case <-ctx.Done():
		}
	}()
	first, stop := context.WithTimeout(ctx, 10*time.Second)
	_, raw, err := conn.Read(first)
	stop()
	var m Message
	if err != nil || json.Unmarshal(raw, &m) != nil || m.Type != "open" || model.ValidateSessionID(m.Session.ID) != nil || m.Session.CreatedAt < 1 || !validSize(m) {
		_ = conn.Close(websocket.StatusPolicyViolation, "Invalid terminal request")
		return
	}
	id := RandomToken()
	output := &terminalOutput{frames: make(chan Message, 64)}
	s.hub.mu.Lock()
	if s.hub.home == nil {
		s.hub.mu.Unlock()
		if s.hub.report != nil {
			s.hub.report("stage=terminal-open-rejected reason=home-offline")
		}
		_ = conn.Close(terminalHomeOffline, "Home unavailable")
		return
	}
	if len(s.hub.terminals) >= maxTerminals {
		s.hub.mu.Unlock()
		if s.hub.report != nil {
			s.hub.report("stage=terminal-open-rejected reason=capacity")
		}
		_ = conn.Close(websocket.StatusTryAgainLater, "Terminal limit reached")
		return
	}
	homePeer := s.hub.home
	homeFlow := s.hub.outputCap
	browserFlow := homeFlow && hasTerminalFlow(m.Capabilities)
	var window *outputWindow
	if browserFlow {
		window = newOutputWindow()
	}
	s.hub.terminals[id] = output
	s.hub.mu.Unlock()
	defer func() {
		s.hub.mu.Lock()
		delete(s.hub.terminals, id)
		s.hub.mu.Unlock()
		c, stop := context.WithTimeout(context.Background(), 2*time.Second)
		defer stop()
		_ = s.hub.sendTo(c, homePeer, Message{Type: "close", ID: id})
	}()
	openCtx, stop := context.WithTimeout(ctx, 20*time.Second)
	open := Message{Type: "open", ID: id, Session: m.Session, Cols: m.Cols, Rows: m.Rows}
	if homeFlow {
		open.Capabilities = []string{terminalFlowCapability}
	}
	_, err = s.hub.requestTo(openCtx, open, homePeer)
	stop()
	if err != nil {
		_ = conn.Close(websocket.StatusPolicyViolation, "Terminal unavailable")
		return
	}
	ready, _ := json.Marshal(struct {
		Type       string `json:"type"`
		Heartbeat  bool   `json:"heartbeat"`
		OutputFlow bool   `json:"output_flow,omitempty"`
	}{"ready", true, browserFlow})
	if err = conn.Write(ctx, websocket.MessageText, ready); err != nil {
		return
	}
	readStopped := make(chan websocket.StatusCode, 1)
	go func() {
		var code websocket.StatusCode
		defer func() { readStopped <- code }()
		for {
			kind, raw, err := conn.Read(ctx)
			if err != nil {
				return
			}
			var frame Message
			if kind == websocket.MessageBinary {
				if len(raw) > 32<<10 {
					code = websocket.StatusProtocolError
					return
				}
				frame = Message{Type: "input", ID: id, Data: raw}
			} else {
				if json.Unmarshal(raw, &frame) != nil {
					code = websocket.StatusProtocolError
					return
				}
				switch frame.Type {
				case "output-ack":
					if window == nil || !window.acknowledge(frame.Received) {
						code = websocket.StatusProtocolError
						return
					}
					frame = Message{Type: "output-ack", ID: id, Received: frame.Received}
				case "resize":
					if !validSize(frame) {
						code = websocket.StatusProtocolError
						return
					}
					frame = Message{Type: "resize", ID: id, Cols: frame.Cols, Rows: frame.Rows}
				case "refresh":
					frame = Message{Type: "refresh", ID: id}
				default:
					code = websocket.StatusProtocolError
					return
				}
			}
			// Rendering acknowledgements are passive output, not user activity.
			// Still revalidate revocation/expiry before forwarding every frame.
			if _, _, ok := s.auth.get(token, frame.Type != "output-ack"); !ok {
				code = websocket.StatusPolicyViolation
				return
			}
			if s.hub.sendTo(ctx, homePeer, frame) != nil {
				code = terminalHomeOffline
				return
			}
		}
	}()
	tick := time.NewTicker(5 * time.Second)
	defer tick.Stop()
	go heartbeat(ctx, &peer{conn: conn})
	for {
		select {
		case code := <-readStopped:
			if code != 0 {
				_ = conn.Close(code, "Terminal transport ended")
			}
			return
		case <-ctx.Done():
			return
		case <-done:
			return
		case <-tick.C:
			if window != nil && window.stalled(time.Now()) {
				_ = conn.Close(terminalOutputFull, "Terminal rendering stalled")
				return
			}
			if _, _, ok := s.auth.get(token, false); !ok {
				return
			}
			// JS cannot observe WebSocket control pings. A bounded application
			// heartbeat lets suspended or half-open clients detect a dead view.
			c, stop := context.WithTimeout(ctx, 5*time.Second)
			err := conn.Write(c, websocket.MessageText, []byte(`{"type":"heartbeat"}`))
			stop()
			if err != nil {
				return
			}
		case frame, ok := <-output.frames:
			if !ok {
				_ = conn.Close(output.closeCode, "Terminal transport ended")
				return
			}
			if frame.Type == "exit" {
				code := terminalViewExited
				if frame.Error == "output-stalled" {
					code = terminalOutputFull
				}
				_ = conn.Close(code, "Terminal view ended")
				return
			}
			c, stop := context.WithTimeout(ctx, 5*time.Second)
			var err error
			if frame.Type == "refresh-result" {
				raw, _ := json.Marshal(struct {
					Type string `json:"type"`
					OK   bool   `json:"ok"`
				}{"refresh-result", frame.Error == ""})
				err = conn.Write(c, websocket.MessageText, raw)
			} else {
				// Register before writing: a fast browser can ACK during Write.
				if window != nil && !window.add(len(frame.Data)) {
					stop()
					_ = conn.Close(terminalOutputFull, "Terminal output window exceeded")
					return
				}
				err = conn.Write(c, websocket.MessageBinary, frame.Data)
				// Older clients cannot acknowledge xterm rendering. Pace their
				// Home output at socket writes until they load the new client.
				if err == nil && homeFlow && !browserFlow {
					err = s.hub.sendTo(c, homePeer, Message{Type: "output-ack", ID: id, Received: int64(len(frame.Data))})
				}
			}
			stop()
			if err != nil {
				return
			}
		}
	}
}
