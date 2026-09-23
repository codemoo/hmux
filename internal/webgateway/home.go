package webgateway

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/url"
	"sync"
	"time"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/filestage"
	"github.com/codemoo/hmux/internal/home"
	"github.com/codemoo/hmux/internal/homeservice"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/timing"
	"github.com/coder/websocket"
)

type homeTerminal struct {
	view   *home.TerminalViewPTY
	input  chan Message
	output *outputWindow
}

type homeUpload struct {
	cancel context.CancelFunc
	input  chan Message
}

// ConnectHome keeps a single outbound TLS connection. Reconnects never restart
// providers; a browser explicitly reopens a validated tmux view after recovery.
func ConnectHome(ctx context.Context, endpoint, token string, cfg config.HomeConfig) error {
	return ConnectHomeLogged(ctx, endpoint, token, cfg, nil)
}

// ConnectHomeLogged emits fixed lifecycle categories only. Remote errors may
// contain private endpoints or headers and must never be forwarded to logs.
func ConnectHomeLogged(ctx context.Context, endpoint, token string, cfg config.HomeConfig, report func(string)) error {
	u, err := url.Parse(endpoint)
	if err != nil || u.Scheme != "wss" || u.Host == "" || u.Path != "/connect" || u.User != nil || u.RawQuery != "" || u.Fragment != "" || cfg.Role != "home" {
		return errors.New("Home role and wss://host/connect required")
	}
	lock, err := homeservice.LockConnector(cfg.StateDir)
	if err != nil {
		return err
	}
	defer lock.Close()
	var notifyMu sync.Mutex
	lastState := ""
	notify := func(state string) {
		notifyMu.Lock()
		defer notifyMu.Unlock()
		if report != nil && state != lastState {
			report(state)
		}
		lastState = state
	}
	notify("Home connector started")
	defer notify("Home connector stopped")
	root, err := homeFileStageRoot()
	if err != nil {
		return err
	}
	go runHomeFileStageSweeper(ctx, root, time.Minute, time.Now)
	for {
		if ctx.Err() != nil {
			return ctx.Err()
		}
		err := connectOnce(ctx, endpoint, token, cfg, notify)
		if ctx.Err() == nil {
			notify("Gateway connection unavailable; retrying; " + homeConnectionSummary(err))
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(3 * time.Second):
		}
	}
}
func connectOnce(parent context.Context, endpoint, token string, cfg config.HomeConfig, report ...func(string)) (result error) {
	var failures homeFailureRecorder
	defer func() { result = failures.result(result) }()
	ctx, cancel := context.WithCancel(parent)
	defer cancel()
	dialCtx, stop := context.WithTimeout(ctx, 15*time.Second)
	conn, response, err := websocket.Dial(dialCtx, endpoint, &websocket.DialOptions{HTTPHeader: http.Header{"Authorization": {"Bearer " + token}}})
	stop()
	if err != nil {
		status := 0
		if response != nil {
			status = response.StatusCode
		}
		return &homeConnectionFailure{stage: "dial", cause: err, httpStatus: status}
	}
	defer conn.CloseNow()
	conn.SetReadLimit(maxMessage)
	var connectionReport func(string)
	if len(report) > 0 {
		connectionReport = report[0]
	}
	p := observedPeer(&peer{conn: conn}, connectionReport)
	p.writeFailure = failures.record
	heartbeatDone := make(chan struct{})
	go func() {
		defer close(heartbeatDone)
		heartbeat(ctx, p, func(err error) {
			if ctx.Err() == nil || errors.Is(err, context.DeadlineExceeded) {
				failures.record("heartbeat", err)
			}
		})
	}()
	defer func() { cancel(); <-heartbeatDone }()
	if err := p.send(ctx, Message{Type: "hello", Capabilities: []string{"web-upload-v1", "codex-completion-v1", terminalFlowCapability}}); err != nil {
		failures.record("hello-write", err)
		return err
	}
	if len(report) > 0 {
		report[0]("Gateway transport opened")
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
	// Operations that change tmux state ask for an immediate catalog poll.
	catalogRefresh := make(chan struct{}, 1)
	refreshCatalog := func() {
		select {
		case catalogRefresh <- struct{}{}:
		default:
		}
	}
	// One shared catalog collector and one usage collector for all web sessions.
	startHomeCatalogCollectors(ctx, cancel, p, cfg, catalogRefresh, &workers, &failures)
	// The usage collector re-reads CLI credentials only once a minute, so a new
	// login or API key restarts it; a fresh collector reads them immediately.
	usageRestart := make(chan struct{}, 1)
	restartUsage := func() {
		select {
		case usageRestart <- struct{}{}:
		default:
		}
	}
	startHomeUsageCollector(ctx, p, usageRestart, &workers)

	// Closing the socket and pipe unblocks readers on cancellation.
	go func() { <-ctx.Done(); _ = conn.CloseNow() }()
	slots := make(chan struct{}, 8)
	for {
		m, err := p.read(ctx)
		if err != nil {
			return &homeConnectionFailure{stage: "read", cause: err}
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
			requestCtx, requestCancel := context.WithCancel(p.timingContext(ctx, logOperation(m)))
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
				doneProcessing := timing.Start(requestCtx, "home-processing", true)
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
							var view *home.TerminalViewPTY
							view, err = home.OpenTerminalViewPTY(requestCtx, cfg, m.Session, m.Cols, m.Rows)
							if err == nil {
								t := &homeTerminal{view: view, input: make(chan Message, 32)}
								if hasTerminalFlow(m.Capabilities) {
									t.output = newOutputWindow()
								}
								terminals[m.ID] = t
								workers.Add(2)
								go func() {
									defer workers.Done()
									defer view.Close()
									defer requestCancel()
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
									exitReason := ""
									defer func() {
										_ = view.Close()
										mu.Lock()
										delete(terminals, m.ID)
										delete(requests, m.ID)
										requestCancel()
										mu.Unlock()
										refreshCatalog()
										_ = p.send(ctx, Message{Type: "exit", ID: m.ID, Error: exitReason})
									}()
									streamErr := streamTerminalOutput(requestCtx, view.Done, view, t.output, func(data []byte) error {
										return p.send(requestCtx, Message{Type: "data", ID: m.ID, Data: data})
									})
									if errors.Is(streamErr, errTerminalOutputStalled) {
										exitReason = "output-stalled"
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
					if err == nil && changesCatalog(m.Operation) {
						refreshCatalog()
					}
					if err == nil && changesProviderAuth(m.Operation, data) {
						restartUsage()
					}
				}
				doneProcessing()
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
				doneSend := timing.Start(requestCtx, "home-response-send", true)
				if p.send(ctx, reply) != nil {
					cancel()
				}
				doneSend()
			}(m)
		case "output-ack":
			mu.Lock()
			t := terminals[m.ID]
			// Late ACKs for a closed view are harmless; malformed ACKs close
			// only their own disposable view, never the shared Home link.
			if t != nil && (t.output == nil || !t.output.acknowledge(m.Received)) {
				if stop := requests[m.ID]; stop != nil {
					stop()
				}
				_ = t.view.Close()
			}
			mu.Unlock()
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
