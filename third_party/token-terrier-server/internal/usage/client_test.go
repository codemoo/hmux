package usage

import (
	"context"
	"errors"
	"io"
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/auth"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

type usageRoundTripFunc func(*http.Request) (*http.Response, error)

func (f usageRoundTripFunc) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

func TestExecuteCapsResponseBodyAndHandlesReadFailure(t *testing.T) {
	tests := []struct {
		name string
		body io.ReadCloser
		kind Kind
	}{
		{name: "oversized", body: io.NopCloser(strings.NewReader(strings.Repeat("x", maxUsageResponseBytes+1))), kind: KindInvalidResponse},
		{name: "read failure", body: io.NopCloser(errorReader{}), kind: KindNetwork},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			client := testUsageClient(http.StatusOK, tt.body)
			req, err := http.NewRequestWithContext(context.Background(), http.MethodGet, "https://example.invalid", nil)
			if err != nil {
				t.Fatal(err)
			}
			_, err = client.execute(req)
			var apiErr *APIError
			if !errors.As(err, &apiErr) || apiErr.Kind != tt.kind {
				t.Fatalf("error = %v, want API kind %v", err, tt.kind)
			}
		})
	}
}

func TestExecuteDoesNotRetainProviderErrorBody(t *testing.T) {
	const secret = "sensitive-provider-detail"
	client := testUsageClient(http.StatusServiceUnavailable, io.NopCloser(strings.NewReader(secret)))
	req, _ := http.NewRequestWithContext(context.Background(), http.MethodGet, "https://example.invalid", nil)
	_, err := client.execute(req)
	if err == nil || strings.Contains(err.Error(), secret) {
		t.Fatalf("provider error body leaked through error: %v", err)
	}
}

func TestExecuteParsesBoundedRetryAfter(t *testing.T) {
	now := time.Date(2026, 9, 8, 12, 0, 0, 0, time.UTC)
	tests := []struct {
		name   string
		header string
		want   time.Duration
		ok     bool
	}{
		{name: "seconds", header: "2307", want: 2307 * time.Second, ok: true},
		{name: "http date", header: now.Add(17 * time.Minute).Format(http.TimeFormat), want: 17 * time.Minute, ok: true},
		{name: "bounded", header: "999999", want: maximumRetryAfter, ok: true},
		{name: "invalid", header: "later", ok: false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			client := &Client{
				clock: func() time.Time { return now },
				HTTP: &http.Client{Transport: usageRoundTripFunc(func(*http.Request) (*http.Response, error) {
					header := make(http.Header)
					header.Set("Retry-After", tt.header)
					return &http.Response{StatusCode: http.StatusTooManyRequests, Header: header, Body: io.NopCloser(strings.NewReader("private body"))}, nil
				})},
			}
			req, _ := http.NewRequestWithContext(context.Background(), http.MethodGet, "https://example.invalid", nil)
			_, err := client.execute(req)
			got, ok := RetryAfter(err)
			if ok != tt.ok || got != tt.want {
				t.Fatalf("RetryAfter = (%v,%v), want (%v,%v); err=%v", got, ok, tt.want, tt.ok, err)
			}
			if strings.Contains(err.Error(), "private body") {
				t.Fatalf("provider body leaked: %v", err)
			}
		})
	}
}

func TestNormalizeWindowsDistinguishesMissingFromFullyUsed(t *testing.T) {
	fullyUsed := 100.0
	claudeMissing := NormalizeClaude(&claudeUsageResponse{
		FiveHour: &claudeWindow{Utilization: &fullyUsed},
	}, auth.OAuthCredential{}, 1, wire.ProducerInfo{}, time.Now())
	if !claudeMissing.Rolling5hObserved || claudeMissing.WeeklyObserved || claudeMissing.Weekly.UsedPct != 0 {
		t.Fatalf("Claude presence flags incorrect: %+v", claudeMissing)
	}
	claudeFull := NormalizeClaude(&claudeUsageResponse{
		SevenDay: &claudeWindow{Utilization: &fullyUsed},
	}, auth.OAuthCredential{}, 1, wire.ProducerInfo{}, time.Now())
	if !claudeFull.WeeklyObserved || claudeFull.Weekly.UsedPct != 1 {
		t.Fatalf("Claude full weekly quota incorrect: %+v", claudeFull)
	}

	codexMissing := NormalizeCodex(&codexUsageResponse{
		Primary: &codexWindow{UsedPercentCamel: flexFloat{Value: 100, Set: true}},
	}, auth.OAuthCredential{}, 1, wire.ProducerInfo{}, time.Now())
	if !codexMissing.Rolling5hObserved || codexMissing.WeeklyObserved || codexMissing.Weekly.UsedPct != 0 {
		t.Fatalf("Codex presence flags incorrect: %+v", codexMissing)
	}
	codexFull := NormalizeCodex(&codexUsageResponse{
		Secondary: &codexWindow{UsedPercentSnake: flexFloat{Value: 100, Set: true}},
	}, auth.OAuthCredential{}, 1, wire.ProducerInfo{}, time.Now())
	if !codexFull.WeeklyObserved || codexFull.Weekly.UsedPct != 1 {
		t.Fatalf("Codex full weekly quota incorrect: %+v", codexFull)
	}
}

func TestNormalizeCodexPlanTypeUsesOnlyAuthoritativeAllowlist(t *testing.T) {
	window := &codexWindow{UsedPercentCamel: flexFloat{Value: 25, Set: true}}
	for raw, want := range map[string]string{"plus": "plus", "ChatGPT_pro": "pro", "mystery": ""} {
		snapshot := NormalizeCodex(&codexUsageResponse{Primary: window, PlanType: raw}, auth.OAuthCredential{}, 1, wire.ProducerInfo{}, time.Now())
		if snapshot.PlanType != want {
			t.Fatalf("plan_type %q normalized to %q, want %q", raw, snapshot.PlanType, want)
		}
	}
}

func TestProviderResponsesRequireSemanticQuotaWindow(t *testing.T) {
	credential := auth.OAuthCredential{AccessToken: "access"}
	tests := []struct {
		name     string
		provider string
		body     string
	}{
		{name: "claude empty", provider: "claude", body: `{}`},
		{name: "claude missing utilization", provider: "claude", body: `{"five_hour":{"resets_at":"2026-07-24T05:00:00Z"}}`},
		{name: "claude out of range", provider: "claude", body: `{"five_hour":{"utilization":101,"resets_at":"2026-07-24T05:00:00Z"}}`},
		{name: "claude invalid reset", provider: "claude", body: `{"five_hour":{"utilization":25,"resets_at":"not-a-time"}}`},
		{name: "codex empty", provider: "codex", body: `{}`},
		{name: "codex quoted NaN", provider: "codex", body: `{"primary":{"used_percent":"NaN"}}`},
		{name: "codex overflow", provider: "codex", body: `{"primary":{"used_percent":"1e9999"}}`},
		{name: "codex out of range", provider: "codex", body: `{"primary":{"used_percent":101}}`},
		{name: "codex invalid reset", provider: "codex", body: `{"primary":{"used_percent":25,"resetsAt":"not-a-time"}}`},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			client := testUsageClient(http.StatusOK, io.NopCloser(strings.NewReader(tt.body)))
			var err error
			if tt.provider == "claude" {
				_, err = client.fetchClaude(context.Background(), credential)
			} else {
				_, err = client.fetchCodex(context.Background(), credential)
			}
			var apiErr *APIError
			if !errors.As(err, &apiErr) || apiErr.Kind != KindInvalidResponse {
				t.Fatalf("error = %v, want invalid response", err)
			}
		})
	}
}

func TestProviderResponsesAcceptValidMinimumWindow(t *testing.T) {
	credential := auth.OAuthCredential{AccessToken: "access"}
	claude := testUsageClient(http.StatusOK, io.NopCloser(strings.NewReader(
		`{"five_hour":{"utilization":25,"resets_at":"2026-07-24T05:00:00Z"}}`,
	)))
	if _, err := claude.fetchClaude(context.Background(), credential); err != nil {
		t.Fatalf("valid Claude response rejected: %v", err)
	}
	codex := testUsageClient(http.StatusOK, io.NopCloser(strings.NewReader(
		`{"primary":{"used_percent":25,"reset_at":1784869200}}`,
	)))
	if _, err := codex.fetchCodex(context.Background(), credential); err != nil {
		t.Fatalf("valid Codex response rejected: %v", err)
	}
}

func TestClaudeResponseAcceptsActiveWindowWithoutResetTime(t *testing.T) {
	credential := auth.OAuthCredential{AccessToken: "access"}
	client := testUsageClient(http.StatusOK, io.NopCloser(strings.NewReader(
		`{"five_hour":{"utilization":25,"resets_at":null},"seven_day":{"utilization":50,"resets_at":"2026-07-24T05:00:00Z"}}`,
	)))
	response, err := client.fetchClaude(context.Background(), credential)
	if err != nil {
		t.Fatalf("Claude null reset rejected: %v", err)
	}
	snapshot := NormalizeClaude(response, credential, 1, wire.ProducerInfo{}, time.Now())
	if snapshot.Rolling5h.UsedPct != 0.25 || snapshot.Rolling5h.ResetsAt != nil || snapshot.Rolling5h.RemainingSeconds != 0 {
		t.Fatalf("null-reset window normalized incorrectly: %#v", snapshot.Rolling5h)
	}
}

func testUsageClient(status int, body io.ReadCloser) *Client {
	return &Client{HTTP: &http.Client{Transport: usageRoundTripFunc(func(*http.Request) (*http.Response, error) {
		return &http.Response{StatusCode: status, Header: make(http.Header), Body: body}, nil
	})}}
}

type errorReader struct{}

func (errorReader) Read([]byte) (int, error) { return 0, errors.New("read failed") }
