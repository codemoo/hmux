// Package api wires HTTP routes for the server.
package api

import (
	"context"
	"encoding/json"
	"errors"
	"log/slog"
	"net"
	"net/http"
	"sync"
	"sync/atomic"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/sse"
	"github.com/codemoo/token-terrier/server-go/internal/state"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

const SnapshotSchema = 1

const (
	defaultSSETotalConnections = 64
	defaultSSEConnectionsPerIP = 8
	sseWriteTimeout            = 15 * time.Second
)

// BuildInfo is the machine-readable provenance returned by /version.
// Pointer fields encode unknown values as JSON null instead of pretending a
// development binary is clean or reproducibly timestamped.
type BuildInfo struct {
	Name         string   `json:"name"`
	Version      string   `json:"version"`
	GitSHA       string   `json:"git_sha"`
	Dirty        *bool    `json:"dirty"`
	BuildTime    *string  `json:"build_time"`
	Schema       int      `json:"schema"`
	Capabilities []string `json:"capabilities"`
}

// OptionalSourceDiagnostics is the common API shape adapters use for local
// account snapshots and pollers. Package-specific readers keep their own rich
// status types; main converts them through WithOptionalSource so this package
// does not depend on every optional source implementation.
type OptionalSourceDiagnostics struct {
	Name                  string  `json:"name"`
	Enabled               bool    `json:"enabled"`
	Observed              bool    `json:"observed"`
	State                 string  `json:"state"`
	LastScanAt            *string `json:"last_scan_at"`
	LastSuccessAt         *string `json:"last_success_at"`
	LastErrorAt           *string `json:"last_error_at"`
	LastErrorKind         string  `json:"last_error_kind,omitempty"`
	SourceUpdatedAt       *string `json:"source_updated_at"`
	UsingLastGood         bool    `json:"using_last_good"`
	LastGoodExpired       bool    `json:"last_good_expired"`
	AgeSeconds            *int64  `json:"age_seconds"`
	MaxLastGoodAgeSeconds int64   `json:"max_last_good_age_seconds"`
}

type optionalSourceFunc func() OptionalSourceDiagnostics

// DefaultCapabilities are compiled into every standalone server binary.
// Runtime enablement is reported separately by /readyz and diagnostics.
var DefaultCapabilities = []string{
	"health",
	"readiness",
	"version",
	"snapshot",
	"sse",
	"oauth_refresh",
	"provider_diagnostics",
	"jsonl",
	"hermes",
	"claude_swap_accounts",
	"codex_lb",
}

// Option customizes runtime metadata without expanding New's stable core
// arguments.
type Option func(*Server)

// WithBuildInfo installs build provenance for /version.
func WithBuildInfo(info BuildInfo) Option {
	return func(s *Server) {
		info.Capabilities = append([]string(nil), info.Capabilities...)
		s.buildInfo = info
	}
}

// WithRootContext ties SSE-triggered refreshes to the daemon lifecycle.
func WithRootContext(ctx context.Context) Option {
	return func(s *Server) {
		if ctx != nil {
			s.rootCtx = ctx
		}
	}
}

// WithProviderEnabled controls whether snapshot/SSE work is accepted for a
// provider. Diagnostics remain available so disabled is distinguishable from
// broken.
func WithProviderEnabled(provider wire.Provider, enabled bool) Option {
	return func(s *Server) { s.providerEnabled[provider] = enabled }
}

// WithOptionalSource registers a live adapter for one optional source. The
// callback runs only for an authenticated diagnostics request.
func WithOptionalSource(provider wire.Provider, name string, status func() OptionalSourceDiagnostics) Option {
	return func(s *Server) {
		if status == nil {
			return
		}
		s.optionalSources[provider] = append(s.optionalSources[provider], func() OptionalSourceDiagnostics {
			result := status()
			result.Name = name
			return result
		})
	}
}

// WithSSEConnectionLimits overrides the bounded defaults, primarily for
// focused tests. Non-positive values retain their default.
func WithSSEConnectionLimits(total, perIP int) Option {
	return func(s *Server) {
		if total > 0 {
			s.sseLimiter.maxTotal = total
		}
		if perIP > 0 {
			s.sseLimiter.maxPerIP = perIP
		}
	}
}

type sseConnectionLimiter struct {
	mu       sync.Mutex
	total    int
	byIP     map[string]int
	maxTotal int
	maxPerIP int
}

func newSSEConnectionLimiter(total, perIP int) *sseConnectionLimiter {
	return &sseConnectionLimiter{
		byIP:     map[string]int{},
		maxTotal: total,
		maxPerIP: perIP,
	}
}

func (l *sseConnectionLimiter) acquire(remoteAddr string) (string, bool) {
	ip := remoteAddr
	if host, _, err := net.SplitHostPort(remoteAddr); err == nil {
		ip = host
	}
	if ip == "" {
		ip = "unknown"
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.total >= l.maxTotal || l.byIP[ip] >= l.maxPerIP {
		return ip, false
	}
	l.total++
	l.byIP[ip]++
	return ip, true
}

func (l *sseConnectionLimiter) release(ip string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.total > 0 {
		l.total--
	}
	if l.byIP[ip] <= 1 {
		delete(l.byIP, ip)
	} else {
		l.byIP[ip]--
	}
}

// Server bundles runtime state needed by HTTP handlers.
type Server struct {
	Tokens   wire.BearerTokens
	Producer wire.ProducerInfo
	Logger   *slog.Logger

	// per-provider live state and SSE hub
	claudeState *state.State
	codexState  *state.State
	claudeHub   *sse.Hub
	codexHub    *sse.Hub

	buildInfo          BuildInfo
	providerEnabled    map[wire.Provider]bool
	rootCtx            context.Context
	backgroundCtx      context.Context
	backgroundStop     context.CancelFunc
	backgroundMu       sync.Mutex
	backgroundWG       sync.WaitGroup
	backgroundClosed   bool
	optionalSources    map[wire.Provider][]optionalSourceFunc
	sseLimiter         *sseConnectionLimiter
	jsonEncodeFailures atomic.Uint64
	jsonWriteFailures  atomic.Uint64
}

// New constructs a Server. Caller passes per-provider state + hub so the
// main can wire the same instances into periodic-refresh tasks.
func New(
	tokens wire.BearerTokens,
	producer wire.ProducerInfo,
	claude, codex *state.State,
	claudeHub, codexHub *sse.Hub,
	logger *slog.Logger,
	options ...Option,
) *Server {
	if logger == nil {
		logger = slog.Default()
	}
	s := &Server{
		Tokens:      tokens,
		Producer:    producer,
		Logger:      logger,
		claudeState: claude,
		codexState:  codex,
		claudeHub:   claudeHub,
		codexHub:    codexHub,
		rootCtx:     context.Background(),
		providerEnabled: map[wire.Provider]bool{
			wire.ProviderClaude: true,
			wire.ProviderCodex:  true,
		},
		optionalSources: map[wire.Provider][]optionalSourceFunc{},
		sseLimiter: newSSEConnectionLimiter(
			defaultSSETotalConnections,
			defaultSSEConnectionsPerIP,
		),
		buildInfo: BuildInfo{
			Name:         "token-terrier-server",
			Version:      "0.0.0-dev",
			GitSHA:       "unknown",
			Schema:       SnapshotSchema,
			Capabilities: append([]string(nil), DefaultCapabilities...),
		},
	}
	for _, option := range options {
		option(s)
	}
	s.backgroundCtx, s.backgroundStop = context.WithCancel(s.rootCtx)
	return s
}

// Routes returns the HTTP handler ready to mount on a listener.
func (s *Server) Routes() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /healthz", s.handleHealthz)
	mux.HandleFunc("GET /readyz", s.handleReadyz)
	mux.HandleFunc("GET /version", s.handleVersion)
	mux.HandleFunc("GET /claude/snapshot", s.requireProvider(wire.ProviderClaude, s.handleSnapshot))
	mux.HandleFunc("GET /codex/snapshot", s.requireProvider(wire.ProviderCodex, s.handleSnapshot))
	mux.HandleFunc("GET /claude/sse", s.requireProvider(wire.ProviderClaude, s.handleSSE))
	mux.HandleFunc("GET /codex/sse", s.requireProvider(wire.ProviderCodex, s.handleSSE))
	mux.HandleFunc("GET /claude/diagnostics", s.requireBearer(wire.ProviderClaude, s.handleDiagnostics))
	mux.HandleFunc("GET /codex/diagnostics", s.requireBearer(wire.ProviderCodex, s.handleDiagnostics))
	return mux
}

func (s *Server) requireProvider(provider wire.Provider, next func(http.ResponseWriter, *http.Request, wire.Provider)) http.HandlerFunc {
	return s.requireBearer(provider, func(w http.ResponseWriter, r *http.Request, provider wire.Provider) {
		if !s.providerEnabled[provider] {
			writeJSONError(w, http.StatusServiceUnavailable, "provider_disabled", "")
			return
		}
		next(w, r, provider)
	})
}

func (s *Server) requireBearer(provider wire.Provider, next func(http.ResponseWriter, *http.Request, wire.Provider)) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if !wire.IsAuthorized(r.Header.Get("Authorization"), s.Tokens.Token(provider)) {
			writeJSONError(w, http.StatusUnauthorized, "unauthorized", "")
			return
		}
		next(w, r, provider)
	}
}

func (s *Server) handleHealthz(w http.ResponseWriter, _ *http.Request) {
	s.writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func (s *Server) handleReadyz(w http.ResponseWriter, _ *http.Request) {
	providers := map[wire.Provider]map[string]bool{
		wire.ProviderClaude: {"enabled": s.providerEnabled[wire.ProviderClaude]},
		wire.ProviderCodex:  {"enabled": s.providerEnabled[wire.ProviderCodex]},
	}
	ready := s.providerEnabled[wire.ProviderClaude] || s.providerEnabled[wire.ProviderCodex]
	status := http.StatusOK
	if !ready {
		status = http.StatusServiceUnavailable
	}
	// Readiness intentionally does not depend on an external quota API. An
	// upstream outage is diagnostic state, not a reason for a supervisor to
	// restart an otherwise functioning local server.
	s.writeJSON(w, status, map[string]any{
		"ready":     ready,
		"basis":     "at_least_one_provider_enabled",
		"providers": providers,
	})
}

func (s *Server) handleVersion(w http.ResponseWriter, _ *http.Request) {
	s.writeJSON(w, http.StatusOK, s.buildInfo)
}

func (s *Server) handleDiagnostics(w http.ResponseWriter, _ *http.Request, provider wire.Provider) {
	optional := make([]OptionalSourceDiagnostics, 0, len(s.optionalSources[provider]))
	for _, status := range s.optionalSources[provider] {
		optional = append(optional, status())
	}
	s.writeJSON(w, http.StatusOK, map[string]any{
		"provider":                  provider,
		"enabled":                   s.providerEnabled[provider],
		"quota":                     s.stateFor(provider).Diagnostics(),
		"optional_sources":          optional,
		"sse_subscribers":           s.hubFor(provider).ClientCount(),
		"sse_encode_failures":       s.hubFor(provider).EncodeFailures(),
		"http_json_encode_failures": s.jsonEncodeFailures.Load(),
		"http_json_write_failures":  s.jsonWriteFailures.Load(),
	})
}

// handleSnapshot fetches via UsageState (cache + sticky + retry-under-refresh
// + 429 backoff) and publishes the result through the SSE hub so subscribers
// also get the freshly-fetched snapshot. Returns the same snapshot to the
// caller in case they don't have an SSE connection.
func (s *Server) handleSnapshot(w http.ResponseWriter, r *http.Request, provider wire.Provider) {
	ctx, cancel := context.WithTimeout(r.Context(), 25*time.Second)
	defer cancel()
	st := s.stateFor(provider)
	hub := s.hubFor(provider)
	update := st.Refresh(ctx, time.Now())
	if err := publishUpdate(hub, update); err != nil {
		s.Logger.Error("snapshot SSE encode failed", "provider", provider, "err", err)
	}
	s.writeJSON(w, http.StatusOK, update.Snapshot)
}

// handleSSE upgrades the connection to text/event-stream and pumps frames
// from the per-provider Hub until the client disconnects. Schedules a
// background refresh on connect so initial slow upstream calls don't delay
// the headers + heartbeat — clients see headers immediately, then the
// freshly-fetched snapshot arrives through the stream like any other frame.
func (s *Server) handleSSE(w http.ResponseWriter, r *http.Request, provider wire.Provider) {
	_, ok := w.(http.Flusher)
	if !ok {
		writeJSONError(w, http.StatusInternalServerError, "no_flusher", "")
		return
	}
	ip, admitted := s.sseLimiter.acquire(r.RemoteAddr)
	if !admitted {
		w.Header().Set("Retry-After", "30")
		writeJSONError(w, http.StatusTooManyRequests, "sse_connection_limit", "")
		return
	}
	defer s.sseLimiter.release(ip)

	hdr := w.Header()
	hdr.Set("Content-Type", "text/event-stream; charset=utf-8")
	hdr.Set("Cache-Control", "no-cache")
	hdr.Set("Connection", "keep-alive")
	hdr.Set("X-Accel-Buffering", "no")
	w.WriteHeader(http.StatusOK)
	if err := flushSSE(w); err != nil {
		return
	}

	hub := s.hubFor(provider)
	st := s.stateFor(provider)

	ctx, cancel := context.WithCancel(r.Context())
	defer cancel()
	events, unsubscribe := hub.Subscribe(ctx)
	defer unsubscribe()

	// Background refresh — let header bytes go out first. It is registered
	// with the server lifecycle so shutdown can cancel and wait for it.
	s.startBackground(func(root context.Context) {
		bgCtx, bgCancel := context.WithTimeout(root, 25*time.Second)
		defer bgCancel()
		update := st.Refresh(bgCtx, time.Now())
		if err := publishUpdate(hub, update); err != nil {
			s.Logger.Error("background snapshot SSE encode failed", "provider", provider, "err", err)
		}
	})

	for {
		select {
		case <-ctx.Done():
			return
		case event, open := <-events:
			if !open {
				return
			}
			if err := writeSSEEvent(w, event); err != nil {
				return
			}
		}
	}
}

func flushSSE(w http.ResponseWriter) error {
	controller := http.NewResponseController(w)
	if err := controller.SetWriteDeadline(time.Now().Add(sseWriteTimeout)); err != nil && !errors.Is(err, http.ErrNotSupported) {
		return err
	}
	err := controller.Flush()
	clearErr := controller.SetWriteDeadline(time.Time{})
	if err != nil {
		return err
	}
	if clearErr != nil && !errors.Is(clearErr, http.ErrNotSupported) {
		return clearErr
	}
	return nil
}

func writeSSEEvent(w http.ResponseWriter, event wire.SSEEvent) error {
	controller := http.NewResponseController(w)
	if err := controller.SetWriteDeadline(time.Now().Add(sseWriteTimeout)); err != nil && !errors.Is(err, http.ErrNotSupported) {
		return err
	}
	_, writeErr := w.Write([]byte(event.Text))
	flushErr := controller.Flush()
	clearErr := controller.SetWriteDeadline(time.Time{})
	if writeErr != nil {
		return writeErr
	}
	if flushErr != nil {
		return flushErr
	}
	if clearErr != nil && !errors.Is(clearErr, http.ErrNotSupported) {
		return clearErr
	}
	return nil
}

// BeginShutdown prevents new detached refresh work and cancels work already
// started by SSE connections. It is idempotent.
func (s *Server) BeginShutdown() {
	s.backgroundMu.Lock()
	if s.backgroundClosed {
		s.backgroundMu.Unlock()
		return
	}
	s.backgroundClosed = true
	s.backgroundStop()
	s.backgroundMu.Unlock()
}

// WaitBackground waits for every SSE-triggered refresh registered before
// BeginShutdown.
func (s *Server) WaitBackground() {
	s.backgroundWG.Wait()
}

func (s *Server) startBackground(work func(context.Context)) bool {
	s.backgroundMu.Lock()
	if s.backgroundClosed {
		s.backgroundMu.Unlock()
		return false
	}
	s.backgroundWG.Add(1)
	ctx := s.backgroundCtx
	s.backgroundMu.Unlock()
	go func() {
		defer s.backgroundWG.Done()
		work(ctx)
	}()
	return true
}

func (s *Server) stateFor(provider wire.Provider) *state.State {
	if provider == wire.ProviderCodex {
		return s.codexState
	}
	return s.claudeState
}

func (s *Server) hubFor(provider wire.Provider) *sse.Hub {
	if provider == wire.ProviderCodex {
		return s.codexHub
	}
	return s.claudeHub
}

// Snapshots are the sole state-bearing protocol event. Auth transitions are
// represented by snapshot.status.state; a second control frame would add a
// competing queue item without providing additional UI state.
func publishUpdate(hub *sse.Hub, u state.UsageUpdate) error {
	return hub.PublishSnapshot(u.Snapshot)
}

func (s *Server) writeJSON(w http.ResponseWriter, status int, v any) {
	data, err := json.Marshal(v)
	if err != nil {
		s.jsonEncodeFailures.Add(1)
		s.Logger.Error("HTTP JSON encode failed", "err", err)
		writeJSONError(w, http.StatusInternalServerError, "json_encode_failed", "")
		return
	}
	data = append(data, '\n')
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	if _, err := w.Write(data); err != nil {
		s.jsonWriteFailures.Add(1)
		s.Logger.Warn("HTTP JSON write failed", "err", err)
	}
}

func writeJSONError(w http.ResponseWriter, status int, code, detail string) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	payload := map[string]string{"error": code}
	if detail != "" {
		payload["detail"] = detail
	}
	_ = json.NewEncoder(w).Encode(payload)
}
