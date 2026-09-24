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
	report       func(string)
	connectionID uint64
	started      time.Time
	writeFailure func(string, error)
	conn         *websocket.Conn
	writeOnce    sync.Once
	writeLock    chan struct{}
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
		p.trace("write-queue", err)
		return err
	}
	defer func() { <-p.writeLock }()
	// Queued work honors its caller cancellation. Once a frame starts, finish
	// it within a fresh bounded transport budget: websocket.Write closes the
	// entire shared connection when its context is canceled mid-frame.
	if err := c.Err(); err != nil {
		return err
	}
	writeCtx, stopWrite := context.WithTimeout(context.WithoutCancel(ctx), 5*time.Second)
	defer stopWrite()
	err = p.conn.Write(writeCtx, websocket.MessageText, raw)
	if err != nil {
		p.trace("transport-write", err)
		if p.writeFailure != nil {
			p.writeFailure("transport-write", err)
		}
	}
	return err
}

func (p *peer) read(ctx context.Context) (Message, error) {
	_, raw, err := p.conn.Read(ctx)
	if err != nil {
		return Message{}, err
	}
	return decodeMessage(raw)
}

// decodeMessage is shared with the cross-language compatibility corpus. Socket
// read limits remain the transport owner's responsibility.
func decodeMessage(raw []byte) (Message, error) {
	var m Message
	d := json.NewDecoder(bytes.NewReader(raw))
	d.DisallowUnknownFields()
	if err := d.Decode(&m); err != nil {
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

func heartbeat(ctx context.Context, p *peer, onFailure ...func(error)) {
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
				if len(onFailure) > 0 && onFailure[0] != nil {
					onFailure[0](err)
				}
				_ = p.conn.CloseNow()
				return
			}
		}
	}
}
