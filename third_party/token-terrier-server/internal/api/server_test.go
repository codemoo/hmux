package api

import (
	"context"
	"encoding/json"
	"math"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/sse"
	"github.com/codemoo/token-terrier/server-go/internal/state"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

const (
	testClaudeToken = "claude-token-0123456789abcdefghijkl"
	testCodexToken  = "codex-token-0123456789abcdefghijklmn"
)

func TestVersionReturnsBuildProvenance(t *testing.T) {
	dirty := true
	built := "2026-07-16T04:05:06Z"
	info := BuildInfo{
		Name:         "token-terrier-server",
		Version:      "1.2.3",
		GitSHA:       "0123456789abcdef",
		Dirty:        &dirty,
		BuildTime:    &built,
		Schema:       SnapshotSchema,
		Capabilities: []string{"snapshot", "sse"},
	}
	srv := newTestServer(WithBuildInfo(info))
	recorder := httptest.NewRecorder()
	srv.Routes().ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/version", nil))
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200", recorder.Code)
	}
	var got BuildInfo
	if err := json.Unmarshal(recorder.Body.Bytes(), &got); err != nil {
		t.Fatal(err)
	}
	if got.Version != info.Version || got.GitSHA != info.GitSHA || got.Dirty == nil || !*got.Dirty || got.BuildTime == nil || *got.BuildTime != built {
		t.Fatalf("build info = %+v, want %+v", got, info)
	}
	if got.Schema != 1 || strings.Join(got.Capabilities, ",") != "snapshot,sse" {
		t.Fatalf("schema/capabilities = %d/%v", got.Schema, got.Capabilities)
	}
}

func TestWriteJSONReturnsSafe500AndCountsEncodeFailure(t *testing.T) {
	srv := newTestServer()
	recorder := httptest.NewRecorder()
	srv.writeJSON(recorder, http.StatusOK, map[string]float64{"invalid": math.NaN()})
	if recorder.Code != http.StatusInternalServerError {
		t.Fatalf("status = %d, want 500", recorder.Code)
	}
	if !strings.Contains(recorder.Body.String(), `"error":"json_encode_failed"`) {
		t.Fatalf("body = %q", recorder.Body.String())
	}
	if got := srv.jsonEncodeFailures.Load(); got != 1 {
		t.Fatalf("JSON encode failures = %d, want 1", got)
	}
}

func TestHealthAndReadinessHaveDifferentSemantics(t *testing.T) {
	srv := newTestServer(
		WithProviderEnabled(wire.ProviderClaude, false),
		WithProviderEnabled(wire.ProviderCodex, false),
	)
	handler := srv.Routes()

	health := httptest.NewRecorder()
	handler.ServeHTTP(health, httptest.NewRequest(http.MethodGet, "/healthz", nil))
	if health.Code != http.StatusOK {
		t.Fatalf("health status = %d, want 200", health.Code)
	}

	ready := httptest.NewRecorder()
	handler.ServeHTTP(ready, httptest.NewRequest(http.MethodGet, "/readyz", nil))
	if ready.Code != http.StatusServiceUnavailable {
		t.Fatalf("readiness status = %d, want 503", ready.Code)
	}
	var payload struct {
		Ready bool `json:"ready"`
	}
	if err := json.Unmarshal(ready.Body.Bytes(), &payload); err != nil {
		t.Fatal(err)
	}
	if payload.Ready {
		t.Fatal("ready = true with every provider disabled")
	}
}

func TestDiagnosticsAreAuthenticatedAndReportDisabledHonestly(t *testing.T) {
	srv := newTestServer(
		WithProviderEnabled(wire.ProviderClaude, false),
		WithOptionalSource(wire.ProviderClaude, "claude_swap_accounts", func() OptionalSourceDiagnostics {
			return OptionalSourceDiagnostics{Enabled: true, Observed: true, State: "missing"}
		}),
	)
	handler := srv.Routes()

	unauthorized := httptest.NewRecorder()
	handler.ServeHTTP(unauthorized, httptest.NewRequest(http.MethodGet, "/claude/diagnostics", nil))
	if unauthorized.Code != http.StatusUnauthorized {
		t.Fatalf("unauthorized status = %d, want 401", unauthorized.Code)
	}

	req := httptest.NewRequest(http.MethodGet, "/claude/diagnostics", nil)
	req.Header.Set("Authorization", "Bearer "+testClaudeToken)
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, req)
	if recorder.Code != http.StatusOK {
		t.Fatalf("diagnostics status = %d, want 200", recorder.Code)
	}
	var got struct {
		Provider       wire.Provider               `json:"provider"`
		Enabled        bool                        `json:"enabled"`
		Quota          state.Diagnostics           `json:"quota"`
		Optional       []OptionalSourceDiagnostics `json:"optional_sources"`
		SSESubscribers int                         `json:"sse_subscribers"`
	}
	if err := json.Unmarshal(recorder.Body.Bytes(), &got); err != nil {
		t.Fatal(err)
	}
	if got.Provider != wire.ProviderClaude || got.Enabled || got.Quota.Observed || got.Quota.State != nil || got.SSESubscribers != 0 {
		t.Fatalf("diagnostics = %+v", got)
	}
	if len(got.Optional) != 1 || got.Optional[0].Name != "claude_swap_accounts" || got.Optional[0].State != "missing" {
		t.Fatalf("optional source diagnostics = %+v", got.Optional)
	}

	snapshotReq := httptest.NewRequest(http.MethodGet, "/claude/snapshot", nil)
	snapshotReq.Header.Set("Authorization", "Bearer "+testClaudeToken)
	disabled := httptest.NewRecorder()
	handler.ServeHTTP(disabled, snapshotReq)
	if disabled.Code != http.StatusServiceUnavailable {
		t.Fatalf("disabled snapshot status = %d, want 503", disabled.Code)
	}
}

func TestBeginShutdownCancelsAndWaitsForBackgroundRefresh(t *testing.T) {
	srv := newTestServer()
	finished := make(chan struct{})
	if started := srv.startBackground(func(ctx context.Context) {
		<-ctx.Done()
		close(finished)
	}); !started {
		t.Fatal("background work was not admitted")
	}

	srv.BeginShutdown()
	srv.WaitBackground()
	<-finished
	if started := srv.startBackground(func(context.Context) {}); started {
		t.Fatal("background work admitted after BeginShutdown")
	}
}

func TestSSEConnectionLimiterEnforcesPerIPAndTotalCaps(t *testing.T) {
	limiter := newSSEConnectionLimiter(2, 1)
	ip1, ok := limiter.acquire("127.0.0.1:1001")
	if !ok {
		t.Fatal("first client was rejected")
	}
	if _, ok := limiter.acquire("127.0.0.1:1002"); ok {
		t.Fatal("second client from same IP bypassed per-IP cap")
	}
	ip2, ok := limiter.acquire("127.0.0.2:1003")
	if !ok {
		t.Fatal("second IP was rejected before total cap")
	}
	if _, ok := limiter.acquire("127.0.0.3:1004"); ok {
		t.Fatal("third client bypassed total cap")
	}
	limiter.release(ip1)
	if _, ok := limiter.acquire("127.0.0.1:1005"); !ok {
		t.Fatal("released capacity was not reusable")
	}
	limiter.release(ip2)
}

func TestSSEWriteSetsAndClearsDeadline(t *testing.T) {
	w := &deadlineWriter{header: http.Header{}}
	event := wire.SSEEvent{Text: "event: snapshot\ndata: {}\n\n"}
	if err := writeSSEEvent(w, event); err != nil {
		t.Fatal(err)
	}
	if got := w.body.String(); got != event.Text {
		t.Fatalf("body = %q, want %q", got, event.Text)
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	if len(w.deadlines) != 2 || w.deadlines[0].IsZero() || !w.deadlines[1].IsZero() {
		t.Fatalf("deadlines = %v, want non-zero then zero", w.deadlines)
	}
	if w.flushes != 1 {
		t.Fatalf("flushes = %d, want 1", w.flushes)
	}
}

type deadlineWriter struct {
	mu        sync.Mutex
	header    http.Header
	body      strings.Builder
	deadlines []time.Time
	flushes   int
}

func (w *deadlineWriter) Header() http.Header { return w.header }
func (w *deadlineWriter) WriteHeader(int)     {}
func (w *deadlineWriter) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.body.Write(p)
}
func (w *deadlineWriter) FlushError() error {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.flushes++
	return nil
}
func (w *deadlineWriter) SetWriteDeadline(deadline time.Time) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.deadlines = append(w.deadlines, deadline)
	return nil
}

func newTestServer(options ...Option) *Server {
	producer := wire.ProducerInfo{ID: "test", TimeZone: "UTC"}
	claude := state.New(wire.ProviderClaude, nil, nil, nil, nil, producer, nil)
	codex := state.New(wire.ProviderCodex, nil, nil, nil, nil, producer, nil)
	return New(
		wire.BearerTokens{Claude: testClaudeToken, Codex: testCodexToken},
		producer,
		claude,
		codex,
		sse.NewHub(),
		sse.NewHub(),
		nil,
		options...,
	)
}
