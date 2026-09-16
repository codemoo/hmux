// Package usage holds provider usage-API clients (Anthropic, Codex/OpenAI)
// and the normalizer that turns raw responses into UsageSnapshot.
package usage

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"strings"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/auth"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

const (
	maxUsageResponseBytes = 64 << 10
	maximumRetryAfter     = 24 * time.Hour
)

// APIError mirrors Swift UsageAPIError variants. The daemon uses these to
// decide whether a failure is transient (sticky cache OK), unauthorized
// (re-login prompt), or contractually broken (quota_endpoint_changed).
type APIError struct {
	Kind    Kind // unauthorized | invalidResponse | server | network
	Status  int  // populated for `server`
	Message string
	// RetryAfter is populated only for HTTP 429 with a valid Retry-After
	// header. It contains no response body or credential data.
	RetryAfter *time.Duration
}

// Kind names the API error class.
type Kind int

const (
	KindUnknown Kind = iota
	KindUnauthorized
	KindInvalidResponse
	KindServer
	KindNetwork
)

func (e *APIError) Error() string {
	switch e.Kind {
	case KindUnauthorized:
		return "provider usage API rejected the access token"
	case KindInvalidResponse:
		return "provider usage API returned an invalid response: " + e.Message
	case KindServer:
		return fmt.Sprintf("provider usage API returned HTTP %d: %s", e.Status, e.Message)
	case KindNetwork:
		return "provider usage API network error: " + e.Message
	}
	return "provider usage API error"
}

// IsUnauthorized typed predicate for callers that want to retry under refresh.
func IsUnauthorized(err error) bool {
	var ae *APIError
	return errors.As(err, &ae) && ae.Kind == KindUnauthorized
}

// IsServer typed predicate (transient: 408/425/429/5xx).
func IsServer(err error) bool {
	var ae *APIError
	return errors.As(err, &ae) && ae.Kind == KindServer
}

// ServerStatus returns the HTTP status if err is a server-class APIError.
// Returns 0 otherwise.
func ServerStatus(err error) int {
	var ae *APIError
	if errors.As(err, &ae) && ae.Kind == KindServer {
		return ae.Status
	}
	return 0
}

// RetryAfter returns a bounded provider-requested delay for HTTP 429.
func RetryAfter(err error) (time.Duration, bool) {
	var ae *APIError
	if !errors.As(err, &ae) || ae.Kind != KindServer || ae.Status != http.StatusTooManyRequests || ae.RetryAfter == nil {
		return 0, false
	}
	return *ae.RetryAfter, true
}

// Client fetches normalized usage snapshots from the upstream providers.
type Client struct {
	HTTP  *http.Client
	clock func() time.Time

	// Producer info to embed in normalized snapshots.
	Producer wire.ProducerInfo
}

// NewClient builds a Client with sensible HTTP timeouts.
func NewClient(producer wire.ProducerInfo) *Client {
	return &Client{
		HTTP: &http.Client{
			Timeout: 30 * time.Second,
		},
		Producer: producer,
	}
}

// Snapshot fetches the upstream usage data and normalizes it to the wire
// format. Caller supplies seq + now so the same response normalized at
// different times produces different generated_at_utc.
func (c *Client) Snapshot(ctx context.Context, provider wire.Provider, credential auth.OAuthCredential, seq int, now time.Time) (wire.UsageSnapshot, error) {
	switch provider {
	case wire.ProviderClaude:
		raw, err := c.fetchClaude(ctx, credential)
		if err != nil {
			return wire.UsageSnapshot{}, err
		}
		return NormalizeClaude(raw, credential, seq, c.Producer, now), nil
	case wire.ProviderCodex:
		raw, err := c.fetchCodex(ctx, credential)
		if err != nil {
			return wire.UsageSnapshot{}, err
		}
		return NormalizeCodex(raw, credential, seq, c.Producer, now), nil
	}
	return wire.UsageSnapshot{}, &APIError{Kind: KindInvalidResponse, Message: "unsupported provider"}
}

// execute runs the HTTP request and maps the response into the typed APIError
// space. Mirrors UsageAPIClient.execute(_:).
func (c *Client) execute(req *http.Request) ([]byte, error) {
	resp, err := c.HTTP.Do(req)
	if err != nil {
		return nil, &APIError{Kind: KindNetwork, Message: err.Error()}
	}
	defer resp.Body.Close()
	if resp.StatusCode == http.StatusUnauthorized || resp.StatusCode == http.StatusForbidden {
		return nil, &APIError{Kind: KindUnauthorized}
	}
	body, err := io.ReadAll(io.LimitReader(resp.Body, maxUsageResponseBytes+1))
	if err != nil {
		return nil, &APIError{Kind: KindNetwork, Message: "response body read failed"}
	}
	if len(body) > maxUsageResponseBytes {
		return nil, &APIError{Kind: KindInvalidResponse, Message: "response body exceeds 64 KiB limit"}
	}
	switch {
	case resp.StatusCode >= 200 && resp.StatusCode < 300:
		return body, nil
	case resp.StatusCode == 408 || resp.StatusCode == 425 || resp.StatusCode == 429 ||
		(resp.StatusCode >= 500 && resp.StatusCode < 600):
		// Transient — sticky cache will mask up to stickyTTL.
		apiError := &APIError{Kind: KindServer, Status: resp.StatusCode, Message: "upstream error body omitted"}
		if resp.StatusCode == http.StatusTooManyRequests {
			if delay, ok := parseRetryAfter(resp.Header.Get("Retry-After"), c.now()); ok {
				apiError.RetryAfter = &delay
			}
		}
		return nil, apiError
	default:
		// Other 4xx (400/404/410/...) typically mean the endpoint
		// contract changed, not transient outage. Surface as
		// invalidResponse → state.quotaEndpointChanged.
		return nil, &APIError{
			Kind:    KindInvalidResponse,
			Message: fmt.Sprintf("HTTP %d", resp.StatusCode),
		}
	}
}

func (c *Client) now() time.Time {
	if c != nil && c.clock != nil {
		return c.clock()
	}
	return time.Now()
}

func parseRetryAfter(raw string, now time.Time) (time.Duration, bool) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return 0, false
	}
	if seconds, err := strconv.ParseInt(raw, 10, 64); err == nil {
		if seconds < 0 {
			return 0, false
		}
		if seconds > int64(maximumRetryAfter/time.Second) {
			return maximumRetryAfter, true
		}
		return boundedRetryAfter(time.Duration(seconds) * time.Second), true
	}
	retryAt, err := http.ParseTime(raw)
	if err != nil {
		return 0, false
	}
	delay := retryAt.Sub(now)
	if delay < 0 {
		delay = 0
	}
	return boundedRetryAfter(delay), true
}

func boundedRetryAfter(delay time.Duration) time.Duration {
	if delay > maximumRetryAfter {
		return maximumRetryAfter
	}
	return delay
}
