package webgateway

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sync"
	"time"

	"github.com/codemoo/hmux/internal/filestage"
)

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

type hub struct {
	report       func(string)
	onCompletion func(completionEvent)
	mu           sync.Mutex
	home         *peer
	pending      map[string]chan Message
	terminals    map[string]*terminalOutput
	uploads      map[string]*gatewayUpload
	uploadCap    bool
	outputCap    bool
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

// Terminal capabilities and all later frames belong to the same Home connection.
func (h *hub) sendTo(ctx context.Context, p *peer, m Message) error {
	h.mu.Lock()
	current := p != nil && h.home == p
	h.mu.Unlock()
	if !current {
		return errors.New("Home connection changed")
	}
	return p.send(ctx, m)
}

func (h *hub) request(ctx context.Context, m Message) (json.RawMessage, error) {
	return h.requestTo(ctx, m, nil)
}

func (h *hub) requestTo(ctx context.Context, m Message, target *peer) (result json.RawMessage, resultErr error) {
	start := time.Now()
	var sendDuration time.Duration
	defer func() {
		if h.report != nil {
			stage := "request-complete"
			if m.Type == "open" {
				stage = "terminal-open-complete"
			}
			reason := "ok"
			if resultErr != nil {
				reason = connectionErrorCategory(resultErr)
			}
			h.report(fmt.Sprintf("stage=%s operation=%s reason=%s duration_ms=%d send_ms=%d", stage, logOperation(m), reason, time.Since(start).Milliseconds(), sendDuration.Milliseconds()))
		}
	}()
	send := h.send
	if target != nil {
		send = func(ctx context.Context, m Message) error { return h.sendTo(ctx, target, m) }
	}
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
	sendStart := time.Now()
	sendErr := send(ctx, m)
	sendDuration = time.Since(sendStart)
	if sendErr != nil {
		return nil, sendErr
	}
	select {
	case <-ctx.Done():
		c, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		_ = send(c, Message{Type: "cancel", ID: m.ID})
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
	h.outputCap = false
	h.catalog = nil
	h.usage = map[string]json.RawMessage{}
	h.mu.Unlock()
	p.trace("home-accepted", nil)
	firstCatalog := true
	defer func() {
		p.trace("home-disconnected", nil)
		h.mu.Lock()
		if h.home == p {
			h.home = nil
		}
		h.uploadCap = false
		h.outputCap = false
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
			p.trace("home-read", err)
			return true
		}
		h.mu.Lock()
		switch m.Type {
		case "hello":
			p.trace("home-hello", nil)
			for _, capability := range m.Capabilities {
				if capability == terminalFlowCapability {
					h.outputCap = true
				}
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
			if firstCatalog {
				p.trace("home-catalog-ready", nil)
				firstCatalog = false
			}
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

func (h *hub) send(ctx context.Context, m Message) error {
	h.mu.Lock()
	p := h.home
	h.mu.Unlock()
	if p == nil {
		return errors.New("Home is offline")
	}
	return p.send(ctx, m)
}
