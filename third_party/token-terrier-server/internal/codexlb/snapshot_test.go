package codexlb

import (
	"context"
	"math"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

func TestBuildSnapshotUsesCodexLBAggregateCreditLimits(t *testing.T) {
	now := time.Date(2026, 6, 10, 9, 55, 0, 0, time.UTC)
	reset5h := "2026-06-10T13:18:50Z"
	reset7d := "2026-06-11T00:45:02Z"
	resp := usageResponse{UpstreamLimits: []upstreamLimit{
		{
			LimitType:      "credits",
			LimitWindow:    "5h",
			MaxValue:       2175,
			CurrentValue:   602,
			RemainingValue: 1573,
			ResetAt:        &reset5h,
			Source:         "aggregate",
		},
		{
			LimitType:      "credits",
			LimitWindow:    "7d",
			MaxValue:       73080,
			CurrentValue:   53500,
			RemainingValue: 19580,
			ResetAt:        &reset7d,
			Source:         "aggregate",
		},
	}}

	snap, ok := buildSnapshot(resp, 7, wire.ProducerInfo{ID: "host", TimeZone: "UTC"}, now)
	if !ok {
		t.Fatal("expected snapshot")
	}
	if snap.Provider != wire.ProviderCodex || snap.Seq != 7 {
		t.Fatalf("unexpected identity: provider=%s seq=%d", snap.Provider, snap.Seq)
	}
	if got, want := snap.Rolling5h.UsedPct, 602.0/2175.0; math.Abs(got-want) > 0.0001 {
		t.Fatalf("rolling used pct = %v, want %v", got, want)
	}
	if got, want := snap.Weekly.UsedPct, 53500.0/73080.0; math.Abs(got-want) > 0.0001 {
		t.Fatalf("weekly used pct = %v, want %v", got, want)
	}
	if snap.Rolling5h.ResetsAt == nil || *snap.Rolling5h.ResetsAt != "2026-06-10T13:18:50.000Z" {
		t.Fatalf("rolling reset = %v", snap.Rolling5h.ResetsAt)
	}
	if snap.Extras.LoginMethod == nil || *snap.Extras.LoginMethod != "codex-lb" {
		t.Fatalf("login method = %v, want codex-lb", snap.Extras.LoginMethod)
	}
	if !snap.Rolling5hObserved || !snap.WeeklyObserved {
		t.Fatalf("aggregate windows should be observed: %+v", snap)
	}
}

func TestBuildSnapshotPrefersPooledRemainingAndAcceptsPoolOnly(t *testing.T) {
	primary := 93.0
	secondary := 88.0
	snap, ok := buildSnapshot(usageResponse{
		AccountPoolUsage: &accountPoolUsage{Primary: &primary, Secondary: &secondary},
	}, 4, wire.ProducerInfo{}, time.Date(2026, 6, 10, 9, 55, 0, 0, time.UTC))
	if !ok {
		t.Fatal("pool-only response should produce a snapshot")
	}
	if got, want := snap.Rolling5h.UsedPct, 0.07; math.Abs(got-want) > 0.0001 {
		t.Fatalf("pooled rolling used pct = %v, want %v", got, want)
	}
	if got, want := snap.Weekly.UsedPct, 0.12; math.Abs(got-want) > 0.0001 {
		t.Fatalf("pooled weekly used pct = %v, want %v", got, want)
	}
	if !snap.Rolling5hObserved || !snap.WeeklyObserved {
		t.Fatalf("pooled windows should be observed: %+v", snap)
	}
}

func TestBuildSnapshotMarksMissingWeeklyUnobserved(t *testing.T) {
	primary := 75.0
	snap, ok := buildSnapshot(usageResponse{
		AccountPoolUsage: &accountPoolUsage{Primary: &primary},
	}, 4, wire.ProducerInfo{}, time.Now())
	if !ok || !snap.Rolling5hObserved || snap.WeeklyObserved {
		t.Fatalf("window presence = rolling %v weekly %v, ok=%v", snap.Rolling5hObserved, snap.WeeklyObserved, ok)
	}
}

func TestBuildSnapshotPoolOverridesAggregateLimits(t *testing.T) {
	primary := 93.0
	secondary := 88.0
	snap, ok := buildSnapshot(usageResponse{
		UpstreamLimits: []upstreamLimit{
			{LimitType: "credits", LimitWindow: "5h", MaxValue: 100, CurrentValue: 100, RemainingValue: 0, Source: "aggregate"},
			{LimitType: "credits", LimitWindow: "1w", MaxValue: 100, CurrentValue: 50, RemainingValue: 50, Source: "aggregate"},
		},
		AccountPoolUsage: &accountPoolUsage{Primary: &primary, Secondary: &secondary},
	}, 4, wire.ProducerInfo{}, time.Now())
	if !ok || math.Abs(snap.Rolling5h.UsedPct-0.07) > 0.0001 || math.Abs(snap.Weekly.UsedPct-0.12) > 0.0001 {
		t.Fatalf("pool should override aggregate windows: %+v, ok=%v", snap, ok)
	}
}

func TestSnapshotterFetchesV1UsageWithBearerKey(t *testing.T) {
	var gotAuth string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/usage" {
			t.Fatalf("path = %s, want /v1/usage", r.URL.Path)
		}
		gotAuth = r.Header.Get("Authorization")
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{
			"upstream_limits": [
				{
					"limit_type": "credits",
					"limit_window": "5h",
					"max_value": 100,
					"current_value": 25,
					"remaining_value": 75,
					"reset_at": "2026-06-10T11:00:00Z",
					"source": "aggregate"
				}
			]
		}`))
	}))
	defer server.Close()

	s := &Snapshotter{
		BaseURL:  server.URL,
		APIKey:   "test-api-key",
		Client:   server.Client(),
		producer: wire.ProducerInfo{ID: "host", TimeZone: "UTC"},
	}
	snap, ok := s.Snapshot(context.Background(), 3, time.Date(2026, 6, 10, 10, 0, 0, 0, time.UTC))
	if !ok {
		t.Fatal("expected snapshot")
	}
	if gotAuth != "Bearer test-api-key" {
		t.Fatalf("authorization = %q", gotAuth)
	}
	if got, want := snap.Rolling5h.UsedPct, 0.25; math.Abs(got-want) > 0.0001 {
		t.Fatalf("rolling used pct = %v, want %v", got, want)
	}
}

func TestSnapshotterCachesAndKeepsStaleAggregateAcrossTransientFailure(t *testing.T) {
	var requests atomic.Int32
	var fail atomic.Bool
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		requests.Add(1)
		if fail.Load() {
			http.Error(w, "temporary", http.StatusServiceUnavailable)
			return
		}
		_, _ = w.Write([]byte(`{"upstream_limits":[{"limit_type":"credits","limit_window":"5h","max_value":100,"current_value":25,"remaining_value":75,"reset_at":"2026-06-10T11:00:00Z","source":"aggregate"}]}`))
	}))
	defer server.Close()
	now := time.Date(2026, 6, 10, 10, 0, 0, 0, time.UTC)
	s := &Snapshotter{
		BaseURL:      server.URL,
		APIKey:       "test",
		Client:       server.Client(),
		producer:     wire.ProducerInfo{ID: "host", TimeZone: "UTC"},
		cacheTTL:     time.Minute,
		stickyTTL:    10 * time.Minute,
		errorBackoff: 30 * time.Second,
	}
	first, ok := s.Snapshot(context.Background(), 1, now)
	if !ok || first.Status.QuotaObservedAt == nil {
		t.Fatalf("first aggregate = %+v, ok=%v", first, ok)
	}
	cached, ok := s.Snapshot(context.Background(), 2, now.Add(10*time.Second))
	if !ok || cached.Seq != 2 || cached.Status.Stale || requests.Load() != 1 {
		t.Fatalf("cached aggregate = %+v requests=%d", cached, requests.Load())
	}

	fail.Store(true)
	stale, ok := s.Snapshot(context.Background(), 3, now.Add(61*time.Second))
	if !ok || !stale.Status.Stale || stale.Status.QuotaObservedAt == nil || *stale.Status.QuotaObservedAt != *first.Status.QuotaObservedAt {
		t.Fatalf("stale aggregate = %+v, ok=%v", stale, ok)
	}
	if status := s.Status(now.Add(61 * time.Second)); status.State != "stale" || status.LastErrorKind != "fetch_error" || status.AgeSeconds == nil {
		t.Fatalf("stale diagnostics = %+v", status)
	}
	backedOff, ok := s.Snapshot(context.Background(), 4, now.Add(62*time.Second))
	if !ok || !backedOff.Status.Stale || requests.Load() != 2 {
		t.Fatalf("backoff aggregate = %+v requests=%d", backedOff, requests.Load())
	}
	expired, ok := s.Snapshot(context.Background(), 5, now.Add(11*time.Minute))
	if !ok || expired.Status.State != wire.StateNetworkError || expired.Status.QuotaSource != wire.QuotaSourceCodexLB || !expired.Status.Stale {
		t.Fatalf("expired configured aggregate should remain visibly unavailable: %+v, ok=%v", expired, ok)
	}
	if requests.Load() != 3 {
		t.Fatalf("requests after sticky expiry = %d, want 3", requests.Load())
	}
	if status := s.Status(now.Add(11 * time.Minute)); status.State != "expired" || !status.Observed {
		t.Fatalf("expired diagnostics = %+v", status)
	}
}

func TestSnapshotterFallsBackWithoutAPIKey(t *testing.T) {
	s := &Snapshotter{BaseURL: "http://127.0.0.1:2455"}
	if _, ok := s.Snapshot(context.Background(), 1, time.Now()); ok {
		t.Fatal("expected no snapshot without API key")
	}
}

func TestConfiguredSnapshotterDoesNotFallBackAfterFailure(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		http.Error(w, "temporary", http.StatusServiceUnavailable)
	}))
	defer server.Close()
	s := &Snapshotter{
		BaseURL:  server.URL,
		APIKey:   "test",
		Client:   server.Client(),
		producer: wire.ProducerInfo{ID: "host", TimeZone: "UTC"},
	}
	snap, ok := s.Snapshot(context.Background(), 3, time.Now())
	if !ok || snap.Status.State != wire.StateNetworkError || snap.Status.QuotaSource != wire.QuotaSourceCodexLB || !snap.Status.Stale {
		t.Fatalf("configured failure should remain codex-lb degraded: %+v, ok=%v", snap, ok)
	}
}

func TestConfiguredSnapshotterPreservesContractFailure(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte(`{"upstream_limits":[],"account_pool_usage":{"primary":null,"secondary":null}}`))
	}))
	defer server.Close()
	s := &Snapshotter{BaseURL: server.URL, APIKey: "test", Client: server.Client()}
	snap, ok := s.Snapshot(context.Background(), 3, time.Now())
	if !ok || snap.Status.State != wire.StateQuotaEndpointChanged || snap.Status.QuotaSource != wire.QuotaSourceCodexLB || !snap.Status.Stale {
		t.Fatalf("configured contract failure should remain codex-lb: %+v, ok=%v", snap, ok)
	}
}

func TestNormalizeBaseURLStripsV1Path(t *testing.T) {
	if got, want := normalizeBaseURL("http://localhost:2455/v1"), "http://localhost:2455"; got != want {
		t.Fatalf("base URL = %q, want %q", got, want)
	}
}

func TestSafeBaseURLRequiresHTTPSOrExactLoopbackHTTP(t *testing.T) {
	tests := map[string]bool{
		"http://127.0.0.1:2455":         true,
		"http://localhost:2455":         true,
		"http://[::1]:2455":             true,
		"https://usage.example.com":     true,
		"http://usage.example.com":      false,
		"http://127.0.0.1.example.com":  false,
		"https://user:pass@example.com": false,
		"file:///tmp/usage.json":        false,
		"https://example.com:bad":       false,
	}
	for raw, want := range tests {
		if got := isSafeBaseURL(raw); got != want {
			t.Errorf("isSafeBaseURL(%q) = %v, want %v", raw, got, want)
		}
	}
}

func TestSnapshotterRejectsPlaintextRemoteBeforeSendingAPIKey(t *testing.T) {
	var requests atomic.Int32
	s := &Snapshotter{
		BaseURL: "http://usage.example.com",
		APIKey:  "must-not-leak",
		Client: &http.Client{Transport: roundTripperFunc(func(*http.Request) (*http.Response, error) {
			requests.Add(1)
			return nil, nil
		})},
	}
	if _, err := s.fetchUsage(context.Background()); err == nil {
		t.Fatal("expected unsafe URL error")
	}
	if got := requests.Load(); got != 0 {
		t.Fatalf("transport received %d requests, want 0", got)
	}
}

func TestSnapshotterDoesNotFollowRedirectWithAPIKey(t *testing.T) {
	var redirectedRequests atomic.Int32
	target := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		redirectedRequests.Add(1)
	}))
	defer target.Close()

	source := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		http.Redirect(w, &http.Request{}, target.URL, http.StatusTemporaryRedirect)
	}))
	defer source.Close()

	s := &Snapshotter{BaseURL: source.URL, APIKey: "must-not-leak", Client: source.Client()}
	if _, err := s.fetchUsage(context.Background()); err == nil {
		t.Fatal("expected redirect response to fail")
	}
	if got := redirectedRequests.Load(); got != 0 {
		t.Fatalf("redirect target received %d requests, want 0", got)
	}
}

func TestSnapshotterCapsResponseBody(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte(strings.Repeat("x", maxUsageResponseBytes+1)))
	}))
	defer server.Close()
	s := &Snapshotter{BaseURL: server.URL, APIKey: "test", Client: server.Client()}
	if _, err := s.fetchUsage(context.Background()); err == nil || !strings.Contains(err.Error(), "exceeds") {
		t.Fatalf("expected body limit error, got %v", err)
	}
}

func TestBuildSnapshotRejectsInvalidAggregateSemantics(t *testing.T) {
	invalidReset := "not-a-time"
	tests := []upstreamLimit{
		{LimitType: "credits", LimitWindow: "5h", MaxValue: 100, CurrentValue: math.NaN(), RemainingValue: 75, Source: "aggregate"},
		{LimitType: "credits", LimitWindow: "5h", MaxValue: 100, CurrentValue: 101, RemainingValue: 0, Source: "aggregate"},
		{LimitType: "credits", LimitWindow: "5h", MaxValue: 100, CurrentValue: 25, RemainingValue: 75, ResetAt: &invalidReset, Source: "aggregate"},
	}
	for _, limit := range tests {
		if _, ok := buildSnapshot(usageResponse{UpstreamLimits: []upstreamLimit{limit}}, 1, wire.ProducerInfo{}, time.Now()); ok {
			t.Fatalf("accepted invalid aggregate limit: %#v", limit)
		}
	}
}

func TestBuildSnapshotRejectsInvalidPoolPercentage(t *testing.T) {
	invalid := 101.0
	if _, ok := buildSnapshot(usageResponse{AccountPoolUsage: &accountPoolUsage{Secondary: &invalid}}, 1, wire.ProducerInfo{}, time.Now()); ok {
		t.Fatal("accepted invalid pooled percentage")
	}
}

type roundTripperFunc func(*http.Request) (*http.Response, error)

func (f roundTripperFunc) RoundTrip(req *http.Request) (*http.Response, error) {
	return f(req)
}

func TestPoolNeverBorrowsMissingWindowOrResetFromKeyAllowance(t *testing.T) {
	primary := 93.0
	reset := "2026-09-12T08:00:00Z"
	response := usageResponse{
		UpstreamLimits: []upstreamLimit{
			{LimitType: "credits", LimitWindow: "5h", MaxValue: 100, CurrentValue: 100, RemainingValue: 0, Source: "aggregate", ResetAt: &reset},
			{LimitType: "credits", LimitWindow: "1w", MaxValue: 100, CurrentValue: 50, RemainingValue: 50, Source: "aggregate", ResetAt: &reset},
		},
		AccountPoolUsage: &accountPoolUsage{Primary: &primary},
	}
	snapshot, ok := buildSnapshot(response, 1, wire.ProducerInfo{}, time.Now())
	if !ok || !snapshot.Rolling5hObserved || snapshot.WeeklyObserved || snapshot.Weekly.ResetsAt != nil || snapshot.Rolling5h.ResetsAt != nil || snapshot.Rolling5h.RemainingSeconds != 0 {
		t.Fatal("partial pool inherited API-key quota or reset time")
	}
	response.AccountPoolUsage = &accountPoolUsage{}
	if _, ok := buildSnapshot(response, 1, wire.ProducerInfo{}, time.Now()); ok {
		t.Fatal("empty observed pool borrowed API-key quotas")
	}
}
