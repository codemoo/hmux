package state

import (
	"bytes"
	"encoding/json"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/auth"
	"github.com/codemoo/token-terrier/server-go/internal/burn"
	"github.com/codemoo/token-terrier/server-go/internal/usage"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

// Frozen synthetic evidence for the Rust pure decision state. This exercises
// Go's active status mapping, sticky fallback and retry timestamp helpers.
func TestRustQuotaStateOracle(t *testing.T) {
	now := time.Date(2026, 9, 8, 4, 5, 6, 123456789, time.UTC)
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	type visible struct {
		State           string  `json:"state"`
		Stale           bool    `json:"stale"`
		RetryAt         *string `json:"retry_at"`
		QuotaObservedAt *string `json:"quota_observed_at"`
	}
	snapshot := func(s wire.UsageSnapshot) visible {
		return visible{string(s.Status.State), s.Status.Stale, s.Status.RetryAt, s.Status.QuotaObservedAt}
	}
	result := map[string]any{}
	for _, provider := range []wire.Provider{wire.ProviderClaude, wire.ProviderCodex} {
		prefix := string(provider) + "_"
		for name, err := range map[string]error{
			"missing":   auth.CredentialFileError{Kind: "missing_token"},
			"malformed": auth.CredentialFileError{Kind: "invalid_json"},
		} {
			result[prefix+name] = string(mapCredentialError(provider, err))
		}
		result[prefix+"unauthorized"] = string(mapRecoveryError(provider, &usage.APIError{Kind: usage.KindUnauthorized}))
	}
	fresh := wire.Degraded(wire.ProviderClaude, 1, wire.ProducerInfo{}, now, wire.StateOK)
	fresh.Status.Stale = false
	fresh.Status.QuotaSource = wire.QuotaSourceOAuth
	observed := wire.FormatTime(now)
	fresh.Status.QuotaObservedAt = &observed
	network := &usage.APIError{Kind: usage.KindNetwork}
	sticky := New(wire.ProviderClaude, nil, nil, nil, nil, wire.ProducerInfo{}, logger)
	sticky.seq = 2
	sticky.lastOkSnapshot = &fresh
	sticky.lastOkAt = now
	sticky.lastOkAccountKey = "synthetic-account"
	result["sticky_network"] = snapshot(sticky.applyError(2, now.Add(61*time.Second), "synthetic-account", wire.StateNetworkError, network, "network", false, burn.Snapshot{}).Snapshot)
	expired := New(wire.ProviderClaude, nil, nil, nil, nil, wire.ProducerInfo{}, logger)
	expired.seq = 2
	expired.lastOkSnapshot = &fresh
	expired.lastOkAt = now
	expired.lastOkAccountKey = "synthetic-account"
	result["expired_network"] = snapshot(expired.applyError(2, now.Add(600*time.Second), "synthetic-account", wire.StateNetworkError, network, "network", false, burn.Snapshot{}).Snapshot)
	delay := 2307 * time.Second
	limited := New(wire.ProviderClaude, nil, nil, nil, nil, wire.ProducerInfo{}, logger)
	limited.seq = 2
	rateError := &usage.APIError{Kind: usage.KindServer, Status: 429, RetryAfter: &delay}
	result["rate_limited"] = snapshot(limited.applyError(2, now, "synthetic-account", wire.StateRateLimited, rateError, "rate_limited", false, burn.Snapshot{}).Snapshot)
	result["delay_seconds"] = []int64{
		int64(rateLimitDelay(rateError, 300*time.Second) / time.Second),
		int64(rateLimitDelay(&usage.APIError{Kind: usage.KindServer, Status: 429}, 300*time.Second) / time.Second),
		int64(rateLimitDelay(&auth.RefreshError{Kind: auth.RefreshKindTransient, Status: 429, RetryAfter: 48 * time.Hour}, 300*time.Second) / time.Second),
	}
	result["collapsed_retry"] = retryAtPointer(now.Add(100*time.Microsecond), now)
	result["visible_retry"] = retryAtPointer(now.Add(time.Millisecond), now)

	path := filepath.Join("..", "..", "..", "..", "tests", "fixtures", "usage-state-v1", "go.json")
	data, err := json.MarshalIndent(result, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	data = append(data, '\n')
	if os.Getenv("HMUX_UPDATE_USAGE_ORACLES") == "1" {
		if err := os.WriteFile(path, data, 0644); err != nil {
			t.Fatal(err)
		}
	}
	stored, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(data, stored) {
		t.Fatal("quota state Go oracle changed")
	}
}
