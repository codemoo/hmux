package webgateway

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/client"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/filestage"
	"github.com/codemoo/hmux/internal/hostmetrics"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/sharedworkspace"
	usagestream "github.com/codemoo/token-terrier/server-go/stream"
	"github.com/coder/websocket"
)

type homeTerminal struct {
	view  *client.AppViewPTY
	input chan Message
}

type homeUpload struct {
	cancel context.CancelFunc
	input  chan Message
}

var (
	homeFileStageRoot   = filestage.DefaultRoot
	homeFileStageVerify = client.VerifyFileStageSession
	homeFileStageSweep  = filestage.SweepExpired
)

const webFileStageTTL = 3 * time.Hour

// ConnectHome keeps a single outbound TLS connection. Reconnects never restart
// providers; a browser explicitly reopens a validated tmux view after recovery.
func ConnectHome(ctx context.Context, endpoint, token string, cfg config.ClientConfig) error {
	u, err := url.Parse(endpoint)
	if err != nil || u.Scheme != "wss" || u.Host == "" || u.Path != "/connect" || u.User != nil || u.RawQuery != "" || u.Fragment != "" || cfg.Role != "home" {
		return errors.New("Home role and wss://host/connect required")
	}
	root, err := homeFileStageRoot()
	if err != nil {
		return err
	}
	go runHomeFileStageSweeper(ctx, root, time.Minute, time.Now)
	for {
		if ctx.Err() != nil {
			return ctx.Err()
		}
		_ = connectOnce(ctx, endpoint, token, cfg)
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(3 * time.Second):
		}
	}
}
func connectOnce(parent context.Context, endpoint, token string, cfg config.ClientConfig) error {
	ctx, cancel := context.WithCancel(parent)
	defer cancel()
	dialCtx, stop := context.WithTimeout(ctx, 15*time.Second)
	conn, _, err := websocket.Dial(dialCtx, endpoint, &websocket.DialOptions{HTTPHeader: http.Header{"Authorization": {"Bearer " + token}}})
	stop()
	if err != nil {
		return err
	}
	defer conn.CloseNow()
	conn.SetReadLimit(maxMessage)
	p := &peer{conn: conn}
	go heartbeat(ctx, p)
	if err := p.send(ctx, Message{Type: "hello", Capabilities: []string{"web-upload-v1", "codex-completion-v1"}}); err != nil {
		return err
	}
	var mu sync.Mutex
	terminals := map[string]*homeTerminal{}
	requests := map[string]context.CancelFunc{}
	uploads := map[string]*homeUpload{}
	var workers sync.WaitGroup
	defer func() {
		cancel()
		mu.Lock()
		for _, t := range terminals {
			_ = t.view.Close()
		}
		for _, upload := range uploads {
			upload.cancel()
		}
		mu.Unlock()
		workers.Wait()
	}()
	// One shared catalog collector and one usage collector for all web sessions.
	var latestMu sync.Mutex
	var latest json.RawMessage
	tracker := catalog.CompletionTracker{}
	completion := newCompletionWorker(tracker.Observe, p.send)
	workers.Add(1)
	go func() {
		defer workers.Done()
		completion.run(ctx)
	}()
	workers.Add(1)
	go func() {
		defer workers.Done()
		defer cancel()
		_ = client.StreamCatalogsObserved(ctx, cfg, completion.enqueue, func(c model.Catalog) error {
			// Enrich only the web stream so older strict native decoders continue
			// receiving the existing negotiated host-metrics shape.
			if used, total, ok := hostmetrics.DiskUsage(); ok {
				if c.HostMetrics == nil || model.ValidateHostMetrics(c.HostMetrics) != nil {
					c.HostMetrics = &model.HostMetrics{ObservedAt: time.Now().UTC()}
				} else {
					copy := *c.HostMetrics
					c.HostMetrics = &copy
				}
				c.HostMetrics.DiskUsedBytes, c.HostMetrics.DiskTotalBytes = &used, &total
			}
			raw, e := json.Marshal(c)
			if e != nil {
				return e
			}
			latestMu.Lock()
			latest = raw
			latestMu.Unlock()
			return p.send(ctx, Message{Type: "catalog", Payload: raw})
		})
	}()
	workers.Add(1)
	go func() {
		defer workers.Done()
		tick := time.NewTicker(5 * time.Second)
		defer tick.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-tick.C:
				latestMu.Lock()
				raw := latest
				latestMu.Unlock()
				if raw != nil && p.send(ctx, Message{Type: "catalog", Payload: raw}) != nil {
					cancel()
					return
				}
			}
		}
	}()
	reader, writer := io.Pipe()
	workers.Add(2)
	go func() { defer workers.Done(); defer writer.Close(); _ = client.StreamUsage(ctx, cfg, writer) }()
	go func() {
		defer workers.Done()
		defer reader.Close()
		defer func() {
			if ctx.Err() == nil {
				_ = p.send(ctx, Message{Type: "usage-unavailable"})
			}
		}()
		decoder, err := usagestream.NewDecoder(reader)
		if err != nil {
			return
		}
		for {
			f, err := decoder.Decode()
			if err != nil {
				return
			}
			if f.Type == "snapshot" && p.send(ctx, Message{Type: "usage", Payload: f.Snapshot}) != nil {
				return
			}
		}
	}()

	// Closing the socket and pipe unblocks readers on cancellation.
	go func() { <-ctx.Done(); _ = conn.CloseNow(); _ = reader.Close(); _ = writer.Close() }()
	slots := make(chan struct{}, 8)
	for {
		m, err := p.read(ctx)
		if err != nil {
			return err
		}
		if len(m.ID) > 64 || m.ID == "" {
			return errors.New("invalid request ID")
		}
		switch m.Type {
		case "upload-start":
			if m.Header == nil || m.ID != m.Header.RequestID || filestage.ValidateHeader(*m.Header) != nil || len(m.Data) != 0 {
				return errors.New("invalid upload request")
			}
			mu.Lock()
			if len(uploads) >= maxGlobalUploads || uploads[m.ID] != nil || requests[m.ID] != nil {
				mu.Unlock()
				_ = p.send(ctx, Message{Type: "upload-error", ID: m.ID, Error: "Home upload unavailable"})
				continue
			}
			uploadCtx, uploadCancel := context.WithCancel(ctx)
			upload := &homeUpload{cancel: uploadCancel, input: make(chan Message, 1)}
			uploads[m.ID] = upload
			mu.Unlock()
			workers.Add(1)
			go func(id string, header filestage.Header, upload *homeUpload) {
				defer workers.Done()
				runHomeUpload(uploadCtx, p, header, upload.input)
				uploadCancel()
				mu.Lock()
				if uploads[id] == upload {
					delete(uploads, id)
				}
				mu.Unlock()
			}(m.ID, *m.Header, upload)
		case "upload-data", "upload-finish", "upload-cancel":
			if m.Header != nil || m.Type == "upload-data" && (len(m.Data) < 1 || len(m.Data) > maxUploadChunk) ||
				m.Type != "upload-data" && len(m.Data) != 0 {
				return errors.New("invalid upload frame")
			}
			mu.Lock()
			upload := uploads[m.ID]
			if upload != nil && m.Type == "upload-cancel" {
				upload.cancel()
			}
			mu.Unlock()
			if upload == nil || m.Type == "upload-cancel" {
				continue
			}
			select {
			case upload.input <- m:
			default:
				upload.cancel()
				_ = p.send(ctx, Message{Type: "upload-error", ID: m.ID, Error: "Home upload unavailable"})
			}
		case "request", "open":
			select {
			case slots <- struct{}{}:
			default:
				_ = p.send(ctx, Message{Type: "response", ID: m.ID, Error: "Home is busy"})
				continue
			}
			requestCtx, requestCancel := context.WithCancel(ctx)
			mu.Lock()
			if _, exists := requests[m.ID]; exists {
				mu.Unlock()
				requestCancel()
				<-slots
				return errors.New("duplicate request ID")
			}
			requests[m.ID] = requestCancel
			mu.Unlock()
			workers.Add(1)
			go func(m Message) {
				defer workers.Done()
				defer func() {
					<-slots
					if m.Type != "open" {
						requestCancel()
						mu.Lock()
						delete(requests, m.ID)
						mu.Unlock()
					}
				}()
				var data any
				var err error
				if m.Type == "open" {
					if model.ValidateSessionID(m.Session.ID) != nil || m.Session.CreatedAt < 1 || !validSize(m) {
						err = errors.New("invalid terminal request")
					} else {
						mu.Lock()
						if len(terminals) >= maxTerminals || terminals[m.ID] != nil {
							err = errors.New("terminal limit reached")
						} else {
							var view *client.AppViewPTY
							view, err = client.OpenAppViewPTY(requestCtx, cfg, m.Session, m.Cols, m.Rows)
							if err == nil {
								t := &homeTerminal{view: view, input: make(chan Message, 32)}
								terminals[m.ID] = t
								workers.Add(2)
								go func() {
									defer workers.Done()
									defer view.Close()
									var lastRefresh time.Time
									for {
										select {
										case <-ctx.Done():
											return
										case <-view.Done:
											return
										case f := <-t.input:
											if f.Type == "input" {
												if _, e := view.Write(f.Data); e != nil {
													return
												}
											} else if f.Type == "refresh" {
												if time.Since(lastRefresh) >= time.Second {
													lastRefresh = time.Now()
													// A redraw failure must not end a working terminal.
													reply := Message{Type: "refresh-result", ID: m.ID}
													if view.Refresh(ctx) != nil {
														reply.Error = "Terminal refresh unavailable"
													}
													if p.send(ctx, reply) != nil {
														return
													}
												}
											} else if view.Resize(f.Cols, f.Rows) != nil {
												return
											}
										}
									}
								}()
								go func() {
									defer workers.Done()
									defer func() {
										_ = view.Close()
										mu.Lock()
										delete(terminals, m.ID)
										delete(requests, m.ID)
										requestCancel()
										mu.Unlock()
										_ = p.send(ctx, Message{Type: "exit", ID: m.ID})
									}()
									buf := make([]byte, 16<<10)
									for {
										n, e := view.Read(buf)
										if n > 0 && p.send(ctx, Message{Type: "data", ID: m.ID, Data: buf[:n]}) != nil {
											return
										}
										if e != nil {
											return
										}
									}
								}()
							}
						}
						mu.Unlock()
					}
					data = map[string]bool{"ok": err == nil}
				} else {
					bounded, stop := context.WithTimeout(requestCtx, 15*time.Second)
					data, err = homeAction(bounded, cfg, m)
					stop()
				}
				reply := Message{Type: "response", ID: m.ID}
				if err != nil {
					reply.Error = "Home operation unavailable"
					if m.Type == "open" {
						requestCancel()
						mu.Lock()
						delete(requests, m.ID)
						mu.Unlock()
					}
				} else {
					reply.Payload, _ = json.Marshal(data)
				}
				if p.send(ctx, reply) != nil {
					cancel()
				}
			}(m)
		case "input", "resize", "refresh", "close", "cancel":
			if len(m.Data) > 32<<10 || m.Type == "resize" && !validSize(m) {
				return errors.New("invalid terminal frame")
			}
			mu.Lock()
			if m.Type == "close" || m.Type == "cancel" {
				if stop := requests[m.ID]; stop != nil {
					stop()
				}
			}
			t := terminals[m.ID]
			if t != nil {
				if m.Type == "close" || m.Type == "cancel" {
					_ = t.view.Close()
				} else {
					select {
					case t.input <- m:
					default:
						_ = t.view.Close()
					}
				}
			}
			mu.Unlock()
		default:
			return errors.New("unknown operation")
		}
	}
}

func runHomeUpload(ctx context.Context, p *peer, header filestage.Header, input <-chan Message) {
	fail := func() {
		_ = p.send(ctx, Message{Type: "upload-error", ID: header.RequestID, Error: "Home upload unavailable"})
	}
	root, err := homeFileStageRoot()
	if err != nil {
		fail()
		return
	}
	reader, writer := io.Pipe()
	var response bytes.Buffer
	receiveDone := make(chan error, 1)
	go func() {
		err := filestage.ReceiveWithTTL(ctx, root, reader, &response, homeFileStageVerify, func() time.Time { return time.Now().UTC() }, webFileStageTTL)
		_ = reader.CloseWithError(err)
		receiveDone <- err
	}()
	receiverFinished := false
	waitReceiver := func() error {
		if !receiverFinished {
			err = <-receiveDone
			receiverFinished = true
		}
		return err
	}
	defer func() {
		if !receiverFinished {
			_ = writer.CloseWithError(context.Canceled)
			_ = reader.CloseWithError(context.Canceled)
			_ = waitReceiver()
		}
	}()
	if err := filestage.WriteHeader(ctx, writer, header); err != nil {
		_ = writer.CloseWithError(err)
		fail()
		return
	}
	if p.send(ctx, Message{Type: "upload-ready", ID: header.RequestID}) != nil {
		_ = writer.CloseWithError(errors.New("gateway disconnected"))
		return
	}
	var received int64
	for {
		select {
		case <-ctx.Done():
			_ = writer.CloseWithError(ctx.Err())
			return
		case err := <-receiveDone:
			receiverFinished = true
			_ = writer.CloseWithError(err)
			fail()
			return
		case message := <-input:
			switch message.Type {
			case "upload-data":
				if int64(len(message.Data)) > header.TotalBytes-received {
					_ = writer.CloseWithError(errors.New("upload exceeds declared size"))
					fail()
					return
				}
				n, writeErr := writer.Write(message.Data)
				if writeErr != nil || n != len(message.Data) {
					_ = writer.CloseWithError(writeErr)
					fail()
					return
				}
				received += int64(n)
				if p.send(ctx, Message{Type: "upload-ack", ID: header.RequestID, Received: received}) != nil {
					_ = writer.CloseWithError(errors.New("gateway disconnected"))
					return
				}
			case "upload-finish":
				if received != header.TotalBytes {
					_ = writer.CloseWithError(errors.New("upload is incomplete"))
					fail()
					return
				}
				if err := writer.Close(); err != nil {
					fail()
					return
				}
				if err := waitReceiver(); err != nil || response.Len() < 1 || response.Len() > filestage.MaximumResponseBytes {
					fail()
					return
				}
				_ = p.send(ctx, Message{Type: "upload-complete", ID: header.RequestID, Payload: append(json.RawMessage(nil), response.Bytes()...)})
				return
			}
		}
	}
}

func runHomeFileStageSweeper(ctx context.Context, root string, interval time.Duration, now func() time.Time) {
	if interval <= 0 || now == nil {
		return
	}
	sweep := func() {
		sweepCtx, cancel := context.WithTimeout(ctx, 30*time.Second)
		_ = homeFileStageSweep(sweepCtx, root, now().UTC())
		cancel()
	}
	sweep()
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			sweep()
		}
	}
}
func homeAction(ctx context.Context, cfg config.ClientConfig, m Message) (any, error) {
	switch m.Operation {
	case "workspace":
		var q struct {
			Change *sharedworkspace.Change `json:"change"`
		}
		if strictPayload(m.Payload, &q) != nil {
			return nil, errors.New("invalid workspace request")
		}
		return client.SharedWorkspace(ctx, cfg, q.Change)
	case "profiles":
		inventory, err := config.LoadInventory(cfg.InventoryPath)
		if err != nil {
			return nil, err
		}
		profiles := []map[string]string{}
		for _, p := range inventory.Profiles {
			profiles = append(profiles, map[string]string{"id": p.ID, "label": p.Label})
		}
		return profiles, nil
	case "create":
		var q struct {
			Profile string `json:"profile"`
			Name    string `json:"name"`
		}
		if strictPayload(m.Payload, &q) != nil {
			return nil, errors.New("invalid create")
		}
		inv, err := config.LoadInventory(cfg.InventoryPath)
		if err != nil {
			return nil, err
		}
		return client.CreateSession(ctx, cfg, inv, q.Profile, q.Name)
	}
	if model.ValidateSessionID(m.Session.ID) != nil || m.Session.CreatedAt < 1 {
		return nil, errors.New("invalid session")
	}
	switch m.Operation {
	case "conversation":
		return client.Conversation(ctx, cfg, m.Session.ID, m.Session.CreatedAt)
	case "alias":
		var q struct {
			Alias string `json:"alias"`
		}
		if strictPayload(m.Payload, &q) != nil {
			return nil, errors.New("invalid alias")
		}
		return map[string]bool{"ok": true}, client.SetAliasExpected(ctx, cfg, m.Session.ID, m.Session.CreatedAt, q.Alias)
	case "hidden":
		var q struct {
			Hidden *bool `json:"hidden"`
		}
		if strictPayload(m.Payload, &q) != nil || q.Hidden == nil {
			return nil, errors.New("invalid hidden state")
		}
		return map[string]bool{"ok": true}, client.SetHiddenExpected(ctx, cfg, m.Session.ID, m.Session.CreatedAt, *q.Hidden)
	}
	return nil, errors.New("operation not permitted")
}
func strictPayload(raw json.RawMessage, v any) error {
	if len(raw) > 16<<10 {
		return errors.New("payload too large")
	}
	d := json.NewDecoder(strings.NewReader(string(raw)))
	d.DisallowUnknownFields()
	if err := d.Decode(v); err != nil {
		return err
	}
	if d.Decode(&struct{}{}) != io.EOF {
		return errors.New("trailing payload")
	}
	return nil
}
