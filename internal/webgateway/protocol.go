package webgateway

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"sync"
	"time"

	"github.com/codemoo/hmux/internal/filestage"
	"github.com/codemoo/hmux/internal/model"
	"github.com/coder/websocket"
)

const maxMessage = 4 << 20
const maxTerminals = 8
const maxUploadChunk = 256 << 10

const (
	terminalHomeOffline websocket.StatusCode = 4001
	terminalOutputFull  websocket.StatusCode = 4002
	terminalViewExited  websocket.StatusCode = 4003
)

type terminalOutput struct {
	frames chan Message
	// Written under hub.mu before closing frames; read only after receiving !ok.
	closeCode websocket.StatusCode
}

func (o *terminalOutput) close(code websocket.StatusCode) {
	o.closeCode = code
	close(o.frames)
}

// Caller holds hub.mu. Overflow only ends the slow disposable view.
func (h *hub) deliverTerminalLocked(m Message) {
	if output := h.terminals[m.ID]; output != nil {
		select {
		case output.frames <- m:
		default:
			output.close(terminalOutputFull)
			delete(h.terminals, m.ID)
		}
	}
}

type Message struct {
	Type         string                `json:"type"`
	ID           string                `json:"id,omitempty"`
	Operation    string                `json:"operation,omitempty"`
	Session      model.SessionIdentity `json:"session,omitempty"`
	Payload      json.RawMessage       `json:"payload,omitempty"`
	Data         []byte                `json:"data,omitempty"`
	Cols         uint16                `json:"cols,omitempty"`
	Rows         uint16                `json:"rows,omitempty"`
	Error        string                `json:"error,omitempty"`
	Header       *filestage.Header     `json:"header,omitempty"`
	Received     int64                 `json:"received,omitempty"`
	Capabilities []string              `json:"capabilities,omitempty"`
}
type peer struct {
	conn      *websocket.Conn
	writeOnce sync.Once
	writeLock chan struct{}
}

func (p *peer) acquireWriter(ctx context.Context) error {
	p.writeOnce.Do(func() { p.writeLock = make(chan struct{}, 1) })
	select {
	case p.writeLock <- struct{}{}:
		if err := ctx.Err(); err != nil {
			<-p.writeLock
			return err
		}
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (p *peer) send(ctx context.Context, m Message) error {
	raw, err := json.Marshal(m)
	if err != nil {
		return err
	}
	c, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	if len(raw) > maxMessage {
		return errors.New("web frame exceeds 4 MiB")
	}
	if err := p.acquireWriter(c); err != nil {
		return err
	}
	defer func() { <-p.writeLock }()
	return p.conn.Write(c, websocket.MessageText, raw)
}
func (p *peer) read(ctx context.Context) (Message, error) {
	var m Message
	_, raw, err := p.conn.Read(ctx)
	if err != nil {
		return m, err
	}
	d := json.NewDecoder(bytes.NewReader(raw))
	d.DisallowUnknownFields()
	if err = d.Decode(&m); err != nil {
		return m, err
	}
	if d.Decode(&struct{}{}) != io.EOF {
		return m, errors.New("trailing frame")
	}
	dataLimit := 32 << 10
	if m.Type == "upload-data" {
		dataLimit = maxUploadChunk
	}
	if len(m.Data) > dataLimit || len(m.ID) > 64 || len(m.Error) > 128 || len(m.Capabilities) > 16 {
		return m, errors.New("frame field exceeds limit")
	}
	for _, capability := range m.Capabilities {
		if len(capability) < 1 || len(capability) > 64 {
			return m, errors.New("frame field exceeds limit")
		}
	}
	return m, nil
}
func heartbeat(ctx context.Context, p *peer) {
	t := time.NewTicker(15 * time.Second)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			c, cancel := context.WithTimeout(ctx, 10*time.Second)
			err := p.conn.Ping(c)
			cancel()
			if err != nil {
				_ = p.conn.CloseNow()
				return
			}
		}
	}
}

type hub struct {
	onCompletion func(completionEvent)
	mu           sync.Mutex
	home         *peer
	pending      map[string]chan Message
	terminals    map[string]*terminalOutput
	uploads      map[string]*gatewayUpload
	uploadCap    bool
	catalog      json.RawMessage
	usage        map[string]json.RawMessage
	updated      time.Time
}

type gatewayUpload struct {
	peer   *peer
	events chan Message
}

func newHub() *hub {
	return &hub{pending: map[string]chan Message{}, terminals: map[string]*terminalOutput{}, uploads: map[string]*gatewayUpload{}}
}

func (h *hub) openUpload(header filestage.Header) (*gatewayUpload, error) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.home == nil || !h.uploadCap {
		return nil, errors.New("Home upload unavailable")
	}
	if h.uploads[header.RequestID] != nil {
		return nil, errors.New("duplicate upload")
	}
	upload := &gatewayUpload{peer: h.home, events: make(chan Message, 2)}
	h.uploads[header.RequestID] = upload
	return upload, nil
}

func (h *hub) closeUpload(id string, upload *gatewayUpload) {
	h.mu.Lock()
	if h.uploads[id] == upload {
		delete(h.uploads, id)
	}
	h.mu.Unlock()
}

func (h *hub) sendUpload(ctx context.Context, upload *gatewayUpload, message Message) error {
	h.mu.Lock()
	bound := upload != nil && h.home == upload.peer && h.uploads[message.ID] == upload
	h.mu.Unlock()
	if !bound {
		return errors.New("Home upload disconnected")
	}
	return upload.peer.send(ctx, message)
}
func (h *hub) send(ctx context.Context, m Message) error {
	h.mu.Lock()
	p := h.home
	h.mu.Unlock()
	if p == nil {
		return errors.New("Home is offline")
	}
	return p.send(ctx, m)
}
func (h *hub) request(ctx context.Context, m Message) (json.RawMessage, error) {
	if m.ID == "" {
		m.ID = RandomToken()
	}
	ch := make(chan Message, 1)
	h.mu.Lock()
	if len(h.pending) >= 16 {
		h.mu.Unlock()
		return nil, errors.New("Home is busy")
	}
	h.pending[m.ID] = ch
	h.mu.Unlock()
	defer func() { h.mu.Lock(); delete(h.pending, m.ID); h.mu.Unlock() }()
	if err := h.send(ctx, m); err != nil {
		return nil, err
	}
	select {
	case <-ctx.Done():
		c, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		_ = h.send(c, Message{Type: "cancel", ID: m.ID})
		return nil, ctx.Err()
	case r := <-ch:
		if r.Error != "" {
			return nil, errors.New(r.Error)
		}
		return r.Payload, nil
	}
}
func (h *hub) serve(ctx context.Context, p *peer) bool {
	h.mu.Lock()
	if h.home != nil {
		h.mu.Unlock()
		return false
	}
	h.home = p
	h.uploadCap = false
	h.catalog = nil
	h.usage = map[string]json.RawMessage{}
	h.mu.Unlock()
	defer func() {
		h.mu.Lock()
		if h.home == p {
			h.home = nil
		}
		h.uploadCap = false
		h.catalog = nil
		h.usage = map[string]json.RawMessage{}
		h.updated = time.Time{}
		for _, ch := range h.pending {
			select {
			case ch <- Message{Error: "Home disconnected"}:
			default:
			}
		}
		for _, ch := range h.terminals {
			ch.close(terminalHomeOffline)
		}
		h.terminals = map[string]*terminalOutput{}
		for id, upload := range h.uploads {
			if upload.peer == p {
				close(upload.events)
				delete(h.uploads, id)
			}
		}
		h.mu.Unlock()
	}()
	for {
		m, err := p.read(ctx)
		if err != nil {
			return true
		}
		h.mu.Lock()
		switch m.Type {
		case "hello":
			for _, capability := range m.Capabilities {
				if capability == "web-upload-v1" {
					h.uploadCap = true
				}
			}
		case "task-complete":
			var payload struct {
				CompletedAt time.Time `json:"completed_at"`
			}
			if h.onCompletion != nil && strictPayload(m.Payload, &payload) == nil {
				h.onCompletion(completionEvent{Session: m.Session, EventID: m.ID, CompletedAt: payload.CompletedAt})
			}
		case "catalog":
			h.catalog = append(json.RawMessage(nil), m.Payload...)
			h.updated = time.Now()
		case "usage-unavailable":
			h.usage = map[string]json.RawMessage{}
		case "usage":
			var provider struct {
				Provider string `json:"provider"`
			}
			if len(m.Payload) <= 1<<20 && json.Unmarshal(m.Payload, &provider) == nil && (provider.Provider == "claude" || provider.Provider == "codex") {
				h.usage[provider.Provider] = append(json.RawMessage(nil), m.Payload...)
			}
		case "response":
			if ch := h.pending[m.ID]; ch != nil {
				select {
				case ch <- m:
				default:
				}
			}
		case "data", "exit", "refresh-result":
			h.deliverTerminalLocked(m)
		case "upload-ready", "upload-ack", "upload-complete", "upload-error":
			if upload := h.uploads[m.ID]; upload != nil && upload.peer == p {
				select {
				case upload.events <- m:
				default:
					close(upload.events)
					delete(h.uploads, m.ID)
				}
			}
		}
		h.mu.Unlock()
	}
}
func (h *hub) snapshot() map[string]any {
	h.mu.Lock()
	defer h.mu.Unlock()
	usage := make(map[string]json.RawMessage, len(h.usage))
	for k, v := range h.usage {
		usage[k] = v
	}
	return map[string]any{"online": h.home != nil && !h.updated.IsZero() && time.Since(h.updated) < 40*time.Second, "catalog": h.catalog, "usage": usage}
}
