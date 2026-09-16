package state

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/auth"
	"github.com/codemoo/token-terrier/server-go/internal/jsonl"
	"github.com/codemoo/token-terrier/server-go/internal/usage"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

type credentialSourceStub struct {
	body   []byte
	err    error
	writes atomic.Int32
}

func (s *credentialSourceStub) Read(context.Context, wire.Provider) ([]byte, error) {
	if s.err != nil {
		return nil, s.err
	}
	return append([]byte(nil), s.body...), nil
}

func (s *credentialSourceStub) Write(context.Context, wire.Provider, []byte) error {
	s.writes.Add(1)
	return nil
}

type refresherStub struct {
	credential auth.OAuthCredential
	err        error
}

func (s refresherStub) Refresh(context.Context, auth.OAuthCredential) (auth.OAuthCredential, error) {
	return s.credential, s.err
}

type roundTripFunc func(*http.Request) (*http.Response, error)

func (f roundTripFunc) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

func TestUnauthorizedWithNoRefresherNeverWritesCredentials(t *testing.T) {
	fixed := time.Date(2026, 7, 16, 4, 5, 6, 0, time.UTC)
	source := &credentialSourceStub{body: []byte(`{"claudeAiOauth":{"accessToken":"access","refreshToken":"refresh"}}`)}
	client := usage.NewClient(wire.ProducerInfo{ID: "test", TimeZone: "UTC"})
	client.HTTP = &http.Client{Transport: roundTripFunc(func(*http.Request) (*http.Response, error) {
		return &http.Response{
			StatusCode: http.StatusUnauthorized,
			Header:     make(http.Header),
			Body:       io.NopCloser(strings.NewReader(`{}`)),
		}, nil
	})}
	st := New(
		wire.ProviderClaude,
		auth.NewCredentialStore(source),
		client,
		nil,
		nil,
		wire.ProducerInfo{ID: "test", TimeZone: "UTC"},
		slog.New(slog.NewTextHandler(io.Discard, nil)),
	)

	got := st.Refresh(context.Background(), fixed)
	if got.Snapshot.Status.State != wire.StateAuthExpired {
		t.Fatalf("state = %q, want authExpired", got.Snapshot.Status.State)
	}
	if writes := source.writes.Load(); writes != 0 {
		t.Fatalf("credential writes = %d, want 0", writes)
	}
}

func TestAuthCounterOnlyTracksUnresolvedUpstreamRejections(t *testing.T) {
	fixed := time.Date(2026, 7, 16, 4, 5, 6, 0, time.UTC)
	source := &credentialSourceStub{body: []byte(`{"claudeAiOauth":{"accessToken":"access","refreshToken":"refresh"}}`)}
	status := http.StatusUnauthorized
	client := usage.NewClient(wire.ProducerInfo{ID: "test", TimeZone: "UTC"})
	client.HTTP = &http.Client{Transport: roundTripFunc(func(*http.Request) (*http.Response, error) {
		return &http.Response{
			StatusCode: status,
			Header:     make(http.Header),
			Body:       io.NopCloser(strings.NewReader(`{}`)),
		}, nil
	})}
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	st := New(
		wire.ProviderClaude,
		auth.NewCredentialStore(source),
		client,
		refresherStub{err: &auth.RefreshError{Kind: auth.RefreshKindNoRefreshToken}},
		nil,
		wire.ProducerInfo{ID: "test", TimeZone: "UTC"},
		logger,
	)

	first := st.Refresh(context.Background(), fixed)
	if first.Snapshot.Status.State != wire.StateAuthExpired {
		t.Fatalf("first state = %q, want authExpired", first.Snapshot.Status.State)
	}
	if got := st.ConsecutiveAuthExpired(); got != 1 {
		t.Fatalf("counter after first unresolved rejection = %d, want 1", got)
	}

	st.Refresh(context.Background(), fixed.Add(61*time.Second))
	if got := st.ConsecutiveAuthExpired(); got != 2 {
		t.Fatalf("counter after second unresolved rejection = %d, want 2", got)
	}

	// A refresh transport failure after an upstream 401 is not proof that
	// the credential is still rejected, and therefore breaks the run.
	st.refresher = refresherStub{err: &auth.RefreshError{Kind: auth.RefreshKindNetwork, Message: "offline"}}
	network := st.Refresh(context.Background(), fixed.Add(122*time.Second))
	if network.Snapshot.Status.State != wire.StateNetworkError {
		t.Fatalf("network recovery state = %q, want networkError", network.Snapshot.Status.State)
	}
	if got := st.ConsecutiveAuthExpired(); got != 0 {
		t.Fatalf("counter after refresh network error = %d, want 0", got)
	}
	diag := st.Diagnostics()
	if diag.LastQuotaErrorKind != "network" || diag.LastQuotaErrorAt == nil {
		t.Fatalf("diagnostics after network error = %+v", diag)
	}
}

func TestNonAuthResultsNeverAdvanceRestartCounter(t *testing.T) {
	fixed := time.Date(2026, 7, 16, 4, 5, 6, 0, time.UTC)
	tests := []struct {
		name       string
		status     int
		wantState  wire.ProviderState
		wantErrKey string
	}{
		{name: "rate limited", status: http.StatusTooManyRequests, wantState: wire.StateRateLimited, wantErrKey: "rate_limited"},
		{name: "server error", status: http.StatusServiceUnavailable, wantState: wire.StateNetworkError, wantErrKey: "upstream_server"},
		{name: "contract error", status: http.StatusNotFound, wantState: wire.StateQuotaEndpointChanged, wantErrKey: "upstream_contract"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			st := newHTTPStatusState(t, tt.status, refresherStub{})
			// Seed the counter to prove a non-auth result resets rather than
			// merely avoiding an increment.
			st.consecutiveAuthExpired = 3
			got := st.Refresh(context.Background(), fixed)
			if got.Snapshot.Status.State != tt.wantState {
				t.Fatalf("state = %q, want %q", got.Snapshot.Status.State, tt.wantState)
			}
			if count := st.ConsecutiveAuthExpired(); count != 0 {
				t.Fatalf("counter = %d, want 0", count)
			}
			if kind := st.Diagnostics().LastQuotaErrorKind; kind != tt.wantErrKey {
				t.Fatalf("error kind = %q, want %q", kind, tt.wantErrKey)
			}
		})
	}
}

func TestMissingCredentialDoesNotCountAsAuthRejection(t *testing.T) {
	fixed := time.Date(2026, 7, 16, 4, 5, 6, 0, time.UTC)
	source := &credentialSourceStub{err: auth.CredentialFileError{Kind: "not_found", Message: "test"}}
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	st := New(
		wire.ProviderClaude,
		auth.NewCredentialStore(source),
		usage.NewClient(wire.ProducerInfo{ID: "test", TimeZone: "UTC"}),
		refresherStub{},
		nil,
		wire.ProducerInfo{ID: "test", TimeZone: "UTC"},
		logger,
	)
	st.consecutiveAuthExpired = 2
	got := st.Refresh(context.Background(), fixed)
	if got.Snapshot.Status.State != wire.StateAuthExpired {
		t.Fatalf("state = %q, want authExpired display state", got.Snapshot.Status.State)
	}
	if count := st.ConsecutiveAuthExpired(); count != 0 {
		t.Fatalf("counter = %d, want 0", count)
	}
	if kind := st.Diagnostics().LastQuotaErrorKind; kind != "credential_missing" {
		t.Fatalf("error kind = %q, want credential_missing", kind)
	}
}

func TestRefreshRateLimitAfterUnauthorizedDoesNotCount(t *testing.T) {
	fixed := time.Date(2026, 7, 16, 4, 5, 6, 0, time.UTC)
	st := newHTTPStatusState(t, http.StatusUnauthorized, refresherStub{
		err: &auth.RefreshError{Kind: auth.RefreshKindRejected, Status: http.StatusTooManyRequests},
	})
	got := st.Refresh(context.Background(), fixed)
	if got.Snapshot.Status.State != wire.StateRateLimited {
		t.Fatalf("state = %q, want rateLimited", got.Snapshot.Status.State)
	}
	if count := st.ConsecutiveAuthExpired(); count != 0 {
		t.Fatalf("counter = %d, want 0", count)
	}
	if kind := st.Diagnostics().LastQuotaErrorKind; kind != "rate_limited" {
		t.Fatalf("error kind = %q, want rate_limited", kind)
	}
}

func TestRateLimitHonorsRetryAfterWithoutOAuthRecovery(t *testing.T) {
	fixed := time.Date(2026, 9, 8, 4, 5, 6, 0, time.UTC)
	source := &credentialSourceStub{body: []byte(`{"claudeAiOauth":{"accessToken":"access","refreshToken":"refresh"}}`)}
	var requests atomic.Int32
	client := usage.NewClient(wire.ProducerInfo{ID: "test", TimeZone: "UTC"})
	client.HTTP = &http.Client{Transport: roundTripFunc(func(*http.Request) (*http.Response, error) {
		request := requests.Add(1)
		header := make(http.Header)
		if request == 1 {
			header.Set("Retry-After", "2307")
			return &http.Response{StatusCode: http.StatusTooManyRequests, Header: header, Body: io.NopCloser(strings.NewReader(`{"private":"omitted"}`))}, nil
		}
		return &http.Response{StatusCode: http.StatusOK, Header: header, Body: io.NopCloser(strings.NewReader(`{"five_hour":{"utilization":25}}`))}, nil
	})}
	refreshCalls := atomic.Int32{}
	st := New(
		wire.ProviderClaude,
		auth.NewCredentialStore(source),
		client,
		countingRefresher{calls: &refreshCalls},
		nil,
		wire.ProducerInfo{ID: "test", TimeZone: "UTC"},
		slog.New(slog.NewTextHandler(io.Discard, nil)),
	)

	first := st.Refresh(context.Background(), fixed)
	if first.Snapshot.Status.State != wire.StateRateLimited || requests.Load() != 1 || refreshCalls.Load() != 0 {
		t.Fatalf("first rate limit = %+v, requests=%d refresh=%d", first.Snapshot.Status, requests.Load(), refreshCalls.Load())
	}
	wantRetryAt := wire.FormatTime(fixed.Add(2307 * time.Second))
	if first.Snapshot.Status.RetryAt == nil || *first.Snapshot.Status.RetryAt != wantRetryAt {
		t.Fatalf("first retry_at = %v, want %q", first.Snapshot.Status.RetryAt, wantRetryAt)
	}
	before := st.Refresh(context.Background(), fixed.Add(2306*time.Second))
	if before.Snapshot.Status.State != wire.StateRateLimited || requests.Load() != 1 {
		t.Fatalf("request retried before Retry-After: %+v requests=%d", before.Snapshot.Status, requests.Load())
	}
	if before.Snapshot.Status.RetryAt == nil || *before.Snapshot.Status.RetryAt != wantRetryAt {
		t.Fatalf("suspended retry_at = %v, want %q", before.Snapshot.Status.RetryAt, wantRetryAt)
	}
	after := st.Refresh(context.Background(), fixed.Add(2308*time.Second))
	if after.Snapshot.Status.State != wire.StateOK || requests.Load() != 2 || refreshCalls.Load() != 0 {
		t.Fatalf("request did not resume after Retry-After: %+v requests=%d refresh=%d", after.Snapshot.Status, requests.Load(), refreshCalls.Load())
	}
	if after.Snapshot.Status.RetryAt != nil || !st.fetchSuspendedUntil.IsZero() || st.fetchSuspendedAccountKey != "" {
		t.Fatalf("successful post-deadline fetch retained suspension: status=%+v until=%v account=%q", after.Snapshot.Status, st.fetchSuspendedUntil, st.fetchSuspendedAccountKey)
	}
}

func TestExpiredRateLimitClearsDeadlineBeforeLaterFailure(t *testing.T) {
	fixed := time.Date(2026, 9, 8, 4, 5, 6, 0, time.UTC)
	source := &credentialSourceStub{body: []byte(`{"claudeAiOauth":{"accessToken":"access"}}`)}
	var requests atomic.Int32
	client := usage.NewClient(wire.ProducerInfo{})
	client.HTTP = &http.Client{Transport: roundTripFunc(func(*http.Request) (*http.Response, error) {
		if requests.Add(1) == 1 {
			header := make(http.Header)
			header.Set("Retry-After", "1")
			return &http.Response{StatusCode: http.StatusTooManyRequests, Header: header, Body: io.NopCloser(strings.NewReader(`{}`))}, nil
		}
		return &http.Response{StatusCode: http.StatusServiceUnavailable, Header: make(http.Header), Body: io.NopCloser(strings.NewReader(`{}`))}, nil
	})}
	st := New(wire.ProviderClaude, auth.NewCredentialStore(source), client, nil, nil, wire.ProducerInfo{}, slog.New(slog.NewTextHandler(io.Discard, nil)))

	limited := st.Refresh(context.Background(), fixed)
	if limited.Snapshot.Status.RetryAt == nil {
		t.Fatalf("active suspension lacked retry_at: %+v", limited.Snapshot.Status)
	}
	eventTime := fixed.Add(2 * time.Second)
	ingested := st.IngestEvent(jsonl.TokenEvent{
		Provider: wire.ProviderClaude, Tokens: 1, Timestamp: eventTime,
	}, eventTime)
	if ingested.Status.RetryAt != nil || st.fetchSuspendedUntil.IsZero() {
		t.Fatalf("post-deadline event retained retry_at or cleared suspension early: status=%+v until=%v", ingested.Status, st.fetchSuspendedUntil)
	}
	after := st.Refresh(context.Background(), fixed.Add(2*time.Second))
	if after.Snapshot.Status.State != wire.StateNetworkError || after.Snapshot.Status.RetryAt != nil || requests.Load() != 2 {
		t.Fatalf("post-expiry failure retained deadline or cache: status=%+v requests=%d", after.Snapshot.Status, requests.Load())
	}
	if !st.fetchSuspendedUntil.IsZero() || st.fetchSuspendedAccountKey != "" {
		t.Fatalf("expired suspension remained: until=%v account=%q", st.fetchSuspendedUntil, st.fetchSuspendedAccountKey)
	}
}

func TestRetryAtPointerOmitsDeadlineCollapsedByWirePrecision(t *testing.T) {
	now := time.Date(2026, 9, 8, 4, 5, 6, 123456789, time.UTC)
	if got := retryAtPointer(now.Add(100*time.Microsecond), now); got != nil {
		t.Fatalf("collapsed retry_at = %q, want nil", *got)
	}
	if got := retryAtPointer(now.Add(time.Millisecond), now); got == nil {
		t.Fatal("representable retry_at was omitted")
	}
}

func TestRateLimitDeadlineClearsOnAccountSwitchBeforeFailedFetch(t *testing.T) {
	fixed := time.Date(2026, 9, 8, 4, 5, 6, 0, time.UTC)
	source := &credentialSourceStub{body: []byte(`{"claudeAiOauth":{"accessToken":"token-a"}}`)}
	store := auth.NewCredentialStore(source)
	var requests atomic.Int32
	client := usage.NewClient(wire.ProducerInfo{})
	client.HTTP = &http.Client{Transport: roundTripFunc(func(*http.Request) (*http.Response, error) {
		if requests.Add(1) == 1 {
			header := make(http.Header)
			header.Set("Retry-After", "3600")
			return &http.Response{StatusCode: http.StatusTooManyRequests, Header: header, Body: io.NopCloser(strings.NewReader(`{}`))}, nil
		}
		return &http.Response{StatusCode: http.StatusServiceUnavailable, Header: make(http.Header), Body: io.NopCloser(strings.NewReader(`{}`))}, nil
	})}
	st := New(wire.ProviderClaude, store, client, nil, nil, wire.ProducerInfo{}, slog.New(slog.NewTextHandler(io.Discard, nil)))

	limited := st.Refresh(context.Background(), fixed)
	if limited.Snapshot.Status.RetryAt == nil {
		t.Fatalf("account A suspension lacked retry_at: %+v", limited.Snapshot.Status)
	}
	store.Replace(wire.ProviderClaude, auth.OAuthCredential{
		Provider: wire.ProviderClaude, AccessToken: "token-b", AccountID: "account-b",
	})
	switched := st.Refresh(context.Background(), fixed.Add(time.Second))
	if switched.Snapshot.Status.State != wire.StateNetworkError || switched.Snapshot.Status.RetryAt != nil || requests.Load() != 2 {
		t.Fatalf("account B inherited retry deadline: status=%+v requests=%d", switched.Snapshot.Status, requests.Load())
	}
	if !st.fetchSuspendedUntil.IsZero() || st.fetchSuspendedAccountKey != "" {
		t.Fatalf("account A suspension remained after switch: until=%v account=%q", st.fetchSuspendedUntil, st.fetchSuspendedAccountKey)
	}
}

func TestRateLimitKeepsRecentQuotaStaleThenExpiresIt(t *testing.T) {
	fixed := time.Date(2026, 9, 8, 4, 5, 6, 0, time.UTC)
	source := &credentialSourceStub{body: []byte(`{"claudeAiOauth":{"accessToken":"access"}}`)}
	var requests atomic.Int32
	client := usage.NewClient(wire.ProducerInfo{})
	client.HTTP = &http.Client{Transport: roundTripFunc(func(*http.Request) (*http.Response, error) {
		if requests.Add(1) == 1 {
			return &http.Response{StatusCode: http.StatusOK, Header: make(http.Header), Body: io.NopCloser(strings.NewReader(`{"seven_day":{"utilization":40}}`))}, nil
		}
		header := make(http.Header)
		header.Set("Retry-After", "2307")
		return &http.Response{StatusCode: http.StatusTooManyRequests, Header: header, Body: io.NopCloser(strings.NewReader(`{}`))}, nil
	})}
	st := New(wire.ProviderClaude, auth.NewCredentialStore(source), client, nil, nil, wire.ProducerInfo{}, slog.New(slog.NewTextHandler(io.Discard, nil)))
	good := st.Refresh(context.Background(), fixed)
	if good.Snapshot.Status.State != wire.StateOK || !good.Snapshot.WeeklyObserved {
		t.Fatalf("seed quota = %+v", good.Snapshot)
	}
	limited := st.Refresh(context.Background(), fixed.Add(61*time.Second))
	if limited.Snapshot.Status.State != wire.StateOK || !limited.Snapshot.Status.Stale || limited.Snapshot.Weekly.UsedPct != 0.4 {
		t.Fatalf("recent quota was not retained stale: %+v", limited.Snapshot)
	}
	wantRetryAt := wire.FormatTime(fixed.Add(61*time.Second + 2307*time.Second))
	if limited.Snapshot.Status.RetryAt == nil || *limited.Snapshot.Status.RetryAt != wantRetryAt {
		t.Fatalf("stale retry_at = %v, want %q", limited.Snapshot.Status.RetryAt, wantRetryAt)
	}
	expired := st.Refresh(context.Background(), fixed.Add(11*time.Minute))
	if expired.Snapshot.Status.State != wire.StateRateLimited || !expired.Snapshot.Status.Stale || requests.Load() != 2 {
		t.Fatalf("expired sticky quota = %+v requests=%d", expired.Snapshot, requests.Load())
	}
	if expired.Snapshot.Status.RetryAt == nil || *expired.Snapshot.Status.RetryAt != wantRetryAt {
		t.Fatalf("rate-limited retry_at after sticky expiry = %v, want %q", expired.Snapshot.Status.RetryAt, wantRetryAt)
	}
}

func TestRateLimitSuspensionIsScopedToOriginatingAccountAndClearsOnSuccess(t *testing.T) {
	fixed := time.Date(2026, 9, 8, 4, 5, 6, 0, time.UTC)
	source := &credentialSourceStub{body: []byte(`{"claudeAiOauth":{"accessToken":"token-a","accountId":"account-a"}}`)}
	store := auth.NewCredentialStore(source)
	var requests atomic.Int32
	client := usage.NewClient(wire.ProducerInfo{})
	client.HTTP = &http.Client{Transport: roundTripFunc(func(*http.Request) (*http.Response, error) {
		request := requests.Add(1)
		if request == 2 {
			header := make(http.Header)
			header.Set("Retry-After", "3600")
			return &http.Response{StatusCode: http.StatusTooManyRequests, Header: header, Body: io.NopCloser(strings.NewReader(`{}`))}, nil
		}
		return &http.Response{StatusCode: http.StatusOK, Header: make(http.Header), Body: io.NopCloser(strings.NewReader(`{"seven_day":{"utilization":40}}`))}, nil
	})}
	st := New(wire.ProviderClaude, store, client, nil, nil, wire.ProducerInfo{}, slog.New(slog.NewTextHandler(io.Discard, nil)))

	if got := st.Refresh(context.Background(), fixed); got.Snapshot.Status.State != wire.StateOK {
		t.Fatalf("account A seed = %+v", got.Snapshot.Status)
	}
	if got := st.Refresh(context.Background(), fixed.Add(61*time.Second)); got.Snapshot.Status.State != wire.StateOK || !got.Snapshot.Status.Stale || got.Snapshot.Status.RetryAt == nil {
		t.Fatalf("account A 429 should retain stale quota and retry deadline: %+v", got.Snapshot.Status)
	}
	store.Replace(wire.ProviderClaude, auth.OAuthCredential{
		Provider: wire.ProviderClaude, AccessToken: "token-b", AccountID: "account-b",
	})
	if got := st.Refresh(context.Background(), fixed.Add(62*time.Second)); got.Snapshot.Status.State != wire.StateOK || got.Snapshot.Status.RetryAt != nil {
		t.Fatalf("account B success = %+v", got.Snapshot.Status)
	}
	if got := st.Refresh(context.Background(), fixed.Add(123*time.Second)); got.Snapshot.Status.State != wire.StateOK {
		t.Fatalf("account B post-cache fetch = %+v", got.Snapshot.Status)
	}
	if got := requests.Load(); got != 4 {
		t.Fatalf("upstream requests = %d, want A success + A 429 + two B successes", got)
	}
	if !st.fetchSuspendedUntil.IsZero() || st.fetchSuspendedAccountKey != "" {
		t.Fatalf("successful account B fetch did not clear suspension: until=%v account=%q", st.fetchSuspendedUntil, st.fetchSuspendedAccountKey)
	}
}

func TestRefreshRateLimitHonorsRetryAfter(t *testing.T) {
	fixed := time.Date(2026, 9, 8, 4, 5, 6, 0, time.UTC)
	source := &credentialSourceStub{body: []byte(`{"claudeAiOauth":{"accessToken":"access","refreshToken":"refresh"}}`)}
	var requests atomic.Int32
	client := usage.NewClient(wire.ProducerInfo{})
	client.HTTP = &http.Client{Transport: roundTripFunc(func(*http.Request) (*http.Response, error) {
		requests.Add(1)
		return &http.Response{StatusCode: http.StatusUnauthorized, Header: make(http.Header), Body: io.NopCloser(strings.NewReader(`{}`))}, nil
	})}
	refreshCalls := atomic.Int32{}
	st := New(
		wire.ProviderClaude,
		auth.NewCredentialStore(source),
		client,
		refreshRateLimitStub{calls: &refreshCalls, retryAfter: 2307 * time.Second},
		nil,
		wire.ProducerInfo{},
		slog.New(slog.NewTextHandler(io.Discard, nil)),
	)

	first := st.Refresh(context.Background(), fixed)
	if first.Snapshot.Status.State != wire.StateRateLimited || requests.Load() != 1 || refreshCalls.Load() != 1 {
		t.Fatalf("first refresh 429 = %+v requests=%d refresh=%d", first.Snapshot.Status, requests.Load(), refreshCalls.Load())
	}
	before := st.Refresh(context.Background(), fixed.Add(2306*time.Second))
	if before.Snapshot.Status.State != wire.StateRateLimited || requests.Load() != 1 || refreshCalls.Load() != 1 {
		t.Fatalf("retried before refresh Retry-After: %+v requests=%d refresh=%d", before.Snapshot.Status, requests.Load(), refreshCalls.Load())
	}
	after := st.Refresh(context.Background(), fixed.Add(2308*time.Second))
	if after.Snapshot.Status.State != wire.StateRateLimited || requests.Load() != 2 || refreshCalls.Load() != 2 {
		t.Fatalf("did not resume after refresh Retry-After: %+v requests=%d refresh=%d", after.Snapshot.Status, requests.Load(), refreshCalls.Load())
	}
}

func TestRateLimitDelayRejectsInvalidRefreshDurations(t *testing.T) {
	fallback := 300 * time.Second
	for _, retryAfter := range []time.Duration{0, -time.Second} {
		err := &auth.RefreshError{Kind: auth.RefreshKindTransient, Status: http.StatusTooManyRequests, RetryAfter: retryAfter}
		if got := rateLimitDelay(err, fallback); got != fallback {
			t.Fatalf("RetryAfter %v produced %v, want fallback %v", retryAfter, got, fallback)
		}
	}
	valid := &auth.RefreshError{Kind: auth.RefreshKindTransient, Status: http.StatusTooManyRequests, RetryAfter: 2307 * time.Second}
	if got := rateLimitDelay(valid, fallback); got != 2307*time.Second {
		t.Fatalf("valid refresh RetryAfter = %v", got)
	}
	longDelay := &auth.RefreshError{Kind: auth.RefreshKindTransient, Status: http.StatusTooManyRequests, RetryAfter: 48 * time.Hour}
	if got := rateLimitDelay(longDelay, fallback); got != maximumRateLimitDelay {
		t.Fatalf("long refresh RetryAfter = %v, want cap %v", got, maximumRateLimitDelay)
	}
}

type refreshRateLimitStub struct {
	calls      *atomic.Int32
	retryAfter time.Duration
}

func (r refreshRateLimitStub) Refresh(context.Context, auth.OAuthCredential) (auth.OAuthCredential, error) {
	r.calls.Add(1)
	return auth.OAuthCredential{}, &auth.RefreshError{
		Kind: auth.RefreshKindTransient, Status: http.StatusTooManyRequests, RetryAfter: r.retryAfter,
	}
}

type countingRefresher struct {
	calls *atomic.Int32
}

func (r countingRefresher) Refresh(context.Context, auth.OAuthCredential) (auth.OAuthCredential, error) {
	r.calls.Add(1)
	return auth.OAuthCredential{}, errors.New("unexpected refresh")
}

func TestCredentialPersistenceFailureAfterUnauthorizedDoesNotCount(t *testing.T) {
	fixed := time.Date(2026, 7, 16, 4, 5, 6, 0, time.UTC)
	st := newHTTPStatusState(t, http.StatusUnauthorized, refresherStub{
		err: &auth.RefreshError{Kind: auth.RefreshKindPersistence, Message: "credential write failed"},
	})
	// Seed the restart guard to prove persistence failure clears an earlier
	// unresolved rejection instead of merely avoiding another increment.
	st.consecutiveAuthExpired = 2

	got := st.Refresh(context.Background(), fixed)
	if got.Snapshot.Status.State != wire.StateNetworkError {
		t.Fatalf("state = %q, want networkError", got.Snapshot.Status.State)
	}
	if count := st.ConsecutiveAuthExpired(); count != 0 {
		t.Fatalf("counter = %d, want 0", count)
	}
	diag := st.Diagnostics()
	if diag.LastQuotaErrorKind != "credential_persist" || diag.LastQuotaErrorAt == nil {
		t.Fatalf("diagnostics after credential persistence failure = %+v", diag)
	}
}

func TestRefresh400OnlyCountsExplicitCredentialRejection(t *testing.T) {
	contract := &auth.RefreshError{
		Kind:    auth.RefreshKindRejected,
		Status:  http.StatusBadRequest,
		Message: "invalid request format",
	}
	if isDefinitiveAuthRecoveryFailure(contract) {
		t.Fatal("generic refresh HTTP 400 classified as definitive auth rejection")
	}
	invalidGrant := &auth.RefreshError{
		Kind:    auth.RefreshKindRejected,
		Status:  http.StatusBadRequest,
		Message: `{"error":"invalid_grant"}`,
	}
	if !isDefinitiveAuthRecoveryFailure(invalidGrant) {
		t.Fatal("invalid_grant refresh response was not classified as definitive auth rejection")
	}
}

func newHTTPStatusState(t *testing.T, status int, refresher Refresher) *State {
	t.Helper()
	source := &credentialSourceStub{body: []byte(`{"claudeAiOauth":{"accessToken":"access","refreshToken":"refresh"}}`)}
	client := usage.NewClient(wire.ProducerInfo{ID: "test", TimeZone: "UTC"})
	client.HTTP = &http.Client{Transport: roundTripFunc(func(*http.Request) (*http.Response, error) {
		return &http.Response{
			StatusCode: status,
			Header:     make(http.Header),
			Body:       io.NopCloser(strings.NewReader(`{}`)),
		}, nil
	})}
	if refresher == nil {
		refresher = refresherStub{err: errors.New("unused")}
	}
	return New(
		wire.ProviderClaude,
		auth.NewCredentialStore(source),
		client,
		refresher,
		nil,
		wire.ProducerInfo{ID: "test", TimeZone: "UTC"},
		slog.New(slog.NewTextHandler(io.Discard, nil)),
	)
}
