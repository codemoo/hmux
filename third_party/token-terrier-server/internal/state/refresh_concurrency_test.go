package state

import (
	"context"
	"io"
	"net/http"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/auth"
	"github.com/codemoo/token-terrier/server-go/internal/jsonl"
	"github.com/codemoo/token-terrier/server-go/internal/usage"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

func TestConcurrentRefreshesShareOneUpstreamRequest(t *testing.T) {
	started := make(chan struct{})
	release := make(chan struct{})
	var requests atomic.Int32
	st := newBlockingRefreshState(started, release, &requests)

	const callers = 12
	var ready sync.WaitGroup
	var done sync.WaitGroup
	ready.Add(callers)
	done.Add(callers)
	for range callers {
		go func() {
			defer done.Done()
			ready.Done()
			update := st.Refresh(context.Background(), time.Now())
			if update.Snapshot.Status.State != wire.StateOK {
				t.Errorf("state = %q, want ok", update.Snapshot.Status.State)
			}
		}()
	}
	ready.Wait()
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("upstream request did not start")
	}
	// Give every caller a chance to join the in-flight refresh before it is
	// released. The assertion is still race-safe if a late caller hits cache.
	time.Sleep(10 * time.Millisecond)
	close(release)
	done.Wait()
	if got := requests.Load(); got != 1 {
		t.Fatalf("upstream requests = %d, want 1", got)
	}
}

func TestSlowRefreshCannotOverwriteNewerIngestSequence(t *testing.T) {
	started := make(chan struct{})
	release := make(chan struct{})
	var requests atomic.Int32
	st := newBlockingRefreshState(started, release, &requests)
	now := time.Date(2026, 7, 24, 1, 2, 3, 0, time.UTC)

	result := make(chan UsageUpdate, 1)
	go func() { result <- st.Refresh(context.Background(), now) }()
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("upstream request did not start")
	}

	ingested := st.IngestEvent(jsonl.TokenEvent{
		Provider:   wire.ProviderClaude,
		SessionKey: "newer-session",
		Tokens:     10,
		Timestamp:  now,
	}, now.Add(time.Second))
	close(release)
	update := <-result

	if update.Snapshot.Seq != ingested.Seq {
		t.Fatalf("slow refresh returned seq %d, want newer seq %d", update.Snapshot.Seq, ingested.Seq)
	}
	latest := st.Latest(now.Add(2 * time.Second))
	if latest.Seq != ingested.Seq || latest.TodayTotalTokens != 10 {
		t.Fatalf("slow refresh overwrote newer event: latest=%+v ingested=%+v", latest, ingested)
	}
}

func TestStickyLastGoodPreservesQuotaObservationAndMarksStale(t *testing.T) {
	fixed := time.Date(2026, 7, 24, 1, 2, 3, 0, time.UTC)
	var status atomic.Int32
	status.Store(http.StatusOK)
	source := &credentialSourceStub{body: []byte(`{"claudeAiOauth":{"accessToken":"access","refreshToken":"refresh"}}`)}
	client := usage.NewClient(wire.ProducerInfo{ID: "test", TimeZone: "UTC"})
	client.HTTP = &http.Client{Transport: roundTripFunc(func(*http.Request) (*http.Response, error) {
		code := int(status.Load())
		body := `{"five_hour":{"utilization":25,"resets_at":"2026-07-24T05:00:00Z"}}`
		if code != http.StatusOK {
			body = `{"error":"temporarily unavailable"}`
		}
		return &http.Response{StatusCode: code, Header: make(http.Header), Body: io.NopCloser(strings.NewReader(body))}, nil
	})}
	st := New(wire.ProviderClaude, auth.NewCredentialStore(source), client, refresherStub{}, nil,
		wire.ProducerInfo{ID: "test", TimeZone: "UTC"}, nil)

	first := st.Refresh(context.Background(), fixed).Snapshot
	if first.Status.QuotaObservedAt == nil || *first.Status.QuotaObservedAt != wire.FormatTime(fixed) {
		t.Fatalf("first quota observation = %v", first.Status.QuotaObservedAt)
	}
	status.Store(http.StatusServiceUnavailable)
	later := fixed.Add(61 * time.Second)
	sticky := st.Refresh(context.Background(), later).Snapshot
	if !sticky.Status.Stale {
		t.Fatalf("sticky snapshot is not marked stale: %+v", sticky.Status)
	}
	if sticky.Status.QuotaObservedAt == nil || *sticky.Status.QuotaObservedAt != wire.FormatTime(fixed) {
		t.Fatalf("sticky quota observation = %v, want %s", sticky.Status.QuotaObservedAt, wire.FormatTime(fixed))
	}
	if sticky.GeneratedAtUTC != wire.FormatTime(later) {
		t.Fatalf("sticky emission time = %q, want %q", sticky.GeneratedAtUTC, wire.FormatTime(later))
	}
}

func newBlockingRefreshState(started chan<- struct{}, release <-chan struct{}, requests *atomic.Int32) *State {
	source := &credentialSourceStub{body: []byte(`{"claudeAiOauth":{"accessToken":"access","refreshToken":"refresh"}}`)}
	client := usage.NewClient(wire.ProducerInfo{ID: "test", TimeZone: "UTC"})
	var once sync.Once
	client.HTTP = &http.Client{Transport: roundTripFunc(func(*http.Request) (*http.Response, error) {
		requests.Add(1)
		once.Do(func() { close(started) })
		<-release
		return &http.Response{
			StatusCode: http.StatusOK,
			Header:     make(http.Header),
			Body: io.NopCloser(strings.NewReader(
				`{"five_hour":{"utilization":25,"resets_at":"2026-07-24T05:00:00Z"}}`,
			)),
		}, nil
	})}
	return New(
		wire.ProviderClaude,
		auth.NewCredentialStore(source),
		client,
		refresherStub{},
		nil,
		wire.ProducerInfo{ID: "test", TimeZone: "UTC"},
		nil,
	)
}
