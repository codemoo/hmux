// Package codexlb asks a local codex-lb instance for aggregate upstream quota
// usage and normalizes it into Token Terrier's Codex snapshot shape.
package codexlb

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"math"
	"net/http"
	"net/url"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

const (
	disableEnv            = "TOKEN_USAGE_DISABLE_CODEX_LB"
	defaultURL            = "http://127.0.0.1:2455"
	maxUsageResponseBytes = 1 << 20
)

var errUsageContract = errors.New("codex-lb usage contract error")

// Snapshotter reads codex-lb's self-service usage endpoint.
type Snapshotter struct {
	BaseURL string
	APIKey  string
	Client  *http.Client

	producer wire.ProducerInfo
	logger   *slog.Logger

	mu            sync.Mutex
	cacheTTL      time.Duration
	stickyTTL     time.Duration
	errorBackoff  time.Duration
	lastAttempt   time.Time
	nextAttempt   time.Time
	lastGood      *wire.UsageSnapshot
	lastGoodAt    time.Time
	lastErrorAt   time.Time
	lastErrorKind string
}

// Status is a privacy-safe aggregate-source freshness summary.
type Status struct {
	Enabled               bool
	Observed              bool
	State                 string
	LastScanAt            *string
	LastSuccessAt         *string
	LastErrorAt           *string
	LastErrorKind         string
	SourceUpdatedAt       *string
	AgeSeconds            *int64
	MaxLastGoodAgeSeconds int64
}

// NewSnapshotter builds a Snapshotter. The URL defaults to the local codex-lb
// server and can be overridden with TOKEN_USAGE_CODEX_LB_URL or
// CODEX_LB_BASE_URL. API keys are read from TOKEN_USAGE_CODEX_LB_API_KEY first,
// then CODEX_LB_API_KEY.
func NewSnapshotter(producer wire.ProducerInfo, logger *slog.Logger) *Snapshotter {
	return NewSnapshotterWithConfiguration(producer, logger, "", "")
}

// NewSnapshotterWithConfiguration builds a Snapshotter with optional
// app-injected values. Non-empty explicit values take precedence over the
// environment; the normal standalone daemon keeps its existing behavior.
func NewSnapshotterWithConfiguration(producer wire.ProducerInfo, logger *slog.Logger, explicitBaseURL, explicitAPIKey string) *Snapshotter {
	if logger == nil {
		logger = slog.Default()
	}
	baseURL := firstNonEmpty(
		explicitBaseURL,
		os.Getenv("TOKEN_USAGE_CODEX_LB_URL"),
		os.Getenv("CODEX_LB_BASE_URL"),
		defaultURL,
	)
	apiKey := firstNonEmpty(
		explicitAPIKey,
		os.Getenv("TOKEN_USAGE_CODEX_LB_API_KEY"),
		os.Getenv("CODEX_LB_API_KEY"),
	)
	return &Snapshotter{
		BaseURL:      normalizeBaseURL(baseURL),
		APIKey:       strings.TrimSpace(apiKey),
		Client:       &http.Client{Timeout: 5 * time.Second},
		producer:     producer,
		logger:       logger,
		cacheTTL:     60 * time.Second,
		stickyTTL:    10 * time.Minute,
		errorBackoff: 30 * time.Second,
	}
}

// Snapshot returns a normalized Codex snapshot from codex-lb if available.
// ok=false means "no codex-lb data, fall back to the normal Codex API path".
func (s *Snapshotter) Snapshot(ctx context.Context, seq int, now time.Time) (snap wire.UsageSnapshot, ok bool) {
	logger := s.logger
	if logger == nil {
		logger = slog.Default()
	}
	if os.Getenv(disableEnv) == "1" {
		return wire.UsageSnapshot{}, false
	}
	if strings.TrimSpace(s.APIKey) == "" {
		return wire.UsageSnapshot{}, false
	}
	s.mu.Lock()
	if s.lastGood != nil && now.Before(s.nextAttempt) && now.Sub(s.lastGoodAt) < durationOr(s.stickyTTL, 10*time.Minute) {
		snap := reemitSnapshot(*s.lastGood, seq, now, true)
		s.mu.Unlock()
		return snap, true
	}
	if s.lastGood != nil && s.nextAttempt.IsZero() && !s.lastAttempt.IsZero() && now.Sub(s.lastAttempt) < durationOr(s.cacheTTL, 60*time.Second) {
		snap := reemitSnapshot(*s.lastGood, seq, now, false)
		s.mu.Unlock()
		return snap, true
	}
	s.lastAttempt = now
	s.mu.Unlock()
	resp, err := s.fetchUsage(ctx)
	if err != nil {
		providerState := wire.StateNetworkError
		errorKind := "fetch_error"
		if errors.Is(err, errUsageContract) {
			providerState = wire.StateQuotaEndpointChanged
			errorKind = "contract_error"
		}
		s.mu.Lock()
		s.nextAttempt = now.Add(durationOr(s.errorBackoff, 30*time.Second))
		s.lastErrorAt = now
		s.lastErrorKind = errorKind
		if s.lastGood != nil && now.Sub(s.lastGoodAt) < durationOr(s.stickyTTL, 10*time.Minute) {
			snap := reemitSnapshot(*s.lastGood, seq, now, true)
			s.mu.Unlock()
			logger.Warn("codex-lb usage fetch failed; serving stale aggregate", "err", err)
			return snap, true
		}
		s.mu.Unlock()
		logger.Warn("codex-lb usage fetch failed; aggregate unavailable", "err", err)
		return unavailableSnapshot(seq, s.producer, now, providerState), true
	}
	snap, ok = buildSnapshot(resp, seq, s.producer, now)
	if !ok {
		s.mu.Lock()
		s.nextAttempt = now.Add(durationOr(s.errorBackoff, 30*time.Second))
		s.lastErrorAt = now
		s.lastErrorKind = "contract_error"
		s.mu.Unlock()
		logger.Warn("codex-lb usage response had no usable quota")
		return unavailableSnapshot(seq, s.producer, now, wire.StateQuotaEndpointChanged), true
	}
	observed := wire.FormatTime(now)
	snap.Status.QuotaObservedAt = &observed
	s.mu.Lock()
	s.lastGood = &snap
	s.lastGoodAt = now
	s.nextAttempt = time.Time{}
	s.lastErrorKind = ""
	s.mu.Unlock()
	return snap, true
}

func unavailableSnapshot(seq int, producer wire.ProducerInfo, now time.Time, providerState wire.ProviderState) wire.UsageSnapshot {
	snapshot := wire.Degraded(wire.ProviderCodex, seq, producer, now, providerState)
	snapshot.Status.QuotaSource = wire.QuotaSourceCodexLB
	return snapshot
}

// Status reports whether the aggregate is fresh, sticky-stale, expired, or
// unavailable. It never performs network I/O.
func (s *Snapshotter) Status(now time.Time) Status {
	enabled := os.Getenv(disableEnv) != "1" && strings.TrimSpace(s.APIKey) != ""
	if !enabled {
		return Status{Enabled: false, State: "disabled"}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	status := Status{
		Enabled:               true,
		Observed:              !s.lastAttempt.IsZero(),
		State:                 "unobserved",
		LastScanAt:            formatTimePtr(s.lastAttempt),
		LastSuccessAt:         formatTimePtr(s.lastGoodAt),
		LastErrorAt:           formatTimePtr(s.lastErrorAt),
		LastErrorKind:         s.lastErrorKind,
		SourceUpdatedAt:       formatTimePtr(s.lastGoodAt),
		MaxLastGoodAgeSeconds: int64(durationOr(s.stickyTTL, 10*time.Minute).Seconds()),
	}
	if s.lastGoodAt.IsZero() {
		if !s.lastErrorAt.IsZero() {
			status.State = "error"
		}
		return status
	}
	age := now.Sub(s.lastGoodAt)
	if age < 0 {
		age = 0
	}
	ageSeconds := int64(age.Seconds())
	status.AgeSeconds = &ageSeconds
	switch {
	case age < durationOr(s.cacheTTL, time.Minute) && s.lastErrorKind == "":
		status.State = "fresh"
	case age < durationOr(s.stickyTTL, 10*time.Minute):
		status.State = "stale"
	default:
		status.State = "expired"
	}
	return status
}

func formatTimePtr(value time.Time) *string {
	if value.IsZero() {
		return nil
	}
	formatted := wire.FormatTime(value)
	return &formatted
}

func reemitSnapshot(snapshot wire.UsageSnapshot, seq int, now time.Time, stale bool) wire.UsageSnapshot {
	snapshot.Seq = seq
	snapshot.GeneratedAtUTC = wire.FormatTime(now)
	snapshot.Status.Stale = stale
	return snapshot
}

func durationOr(value, fallback time.Duration) time.Duration {
	if value <= 0 {
		return fallback
	}
	return value
}

func (s *Snapshotter) fetchUsage(ctx context.Context) (usageResponse, error) {
	if strings.TrimSpace(s.BaseURL) == "" {
		return usageResponse{}, errors.New("empty codex-lb URL")
	}
	if !isSafeBaseURL(s.BaseURL) {
		return usageResponse{}, errors.New("unsafe codex-lb URL: require HTTPS or exact loopback HTTP")
	}
	endpoint := strings.TrimRight(s.BaseURL, "/") + "/v1/usage"
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, endpoint, nil)
	if err != nil {
		return usageResponse{}, err
	}
	req.Header.Set("Authorization", "Bearer "+s.APIKey)
	req.Header.Set("Accept", "application/json")

	client := s.Client
	if client == nil {
		client = &http.Client{Timeout: 5 * time.Second}
	}
	secureClient := *client
	secureClient.CheckRedirect = func(_ *http.Request, _ []*http.Request) error {
		return http.ErrUseLastResponse
	}
	httpResp, err := secureClient.Do(req)
	if err != nil {
		return usageResponse{}, err
	}
	defer httpResp.Body.Close()

	if httpResp.StatusCode < 200 || httpResp.StatusCode >= 300 {
		return usageResponse{}, fmt.Errorf("codex-lb usage status %d", httpResp.StatusCode)
	}
	limited := io.LimitReader(httpResp.Body, maxUsageResponseBytes+1)
	raw, err := io.ReadAll(limited)
	if err != nil {
		return usageResponse{}, fmt.Errorf("read codex-lb usage: %w", err)
	}
	if len(raw) > maxUsageResponseBytes {
		return usageResponse{}, fmt.Errorf("codex-lb usage body exceeds %d bytes", maxUsageResponseBytes)
	}
	var decoded usageResponse
	if err := json.Unmarshal(bytes.TrimSpace(raw), &decoded); err != nil {
		return usageResponse{}, fmt.Errorf("%w: invalid JSON", errUsageContract)
	}
	if err := validateUsageResponse(decoded); err != nil {
		return usageResponse{}, fmt.Errorf("%w: %v", errUsageContract, err)
	}
	return decoded, nil
}

type usageResponse struct {
	UpstreamLimits   []upstreamLimit   `json:"upstream_limits"`
	AccountPoolUsage *accountPoolUsage `json:"account_pool_usage"`
}

type accountPoolUsage struct {
	Primary   *float64 `json:"primary"`
	Secondary *float64 `json:"secondary"`
}

type upstreamLimit struct {
	LimitType      string  `json:"limit_type"`
	LimitWindow    string  `json:"limit_window"`
	MaxValue       float64 `json:"max_value"`
	CurrentValue   float64 `json:"current_value"`
	RemainingValue float64 `json:"remaining_value"`
	ModelFilter    *string `json:"model_filter"`
	ResetAt        *string `json:"reset_at"`
	Source         string  `json:"source"`
}

func buildSnapshot(resp usageResponse, seq int, producer wire.ProducerInfo, now time.Time) (wire.UsageSnapshot, bool) {
	if err := validateUsageResponse(resp); err != nil {
		return wire.UsageSnapshot{}, false
	}

	rolling := wire.EmptyRollingWindow()
	weekly := wire.EmptyRollingWindow()
	quotaWindows := make([]wire.QuotaWindow, 0)
	seen := false
	rollingObserved := false
	weeklyObserved := false

	for _, limit := range resp.UpstreamLimits {
		if !isAggregateCreditLimit(limit) || limit.MaxValue <= 0 {
			continue
		}
		usedPct := clamp(limit.CurrentValue/limit.MaxValue, 0, 1)
		resetAt := parseResetAt(limit.ResetAt)
		window := normalizedWindow(limit.LimitWindow)
		switch window {
		case "5h":
			rolling = rollingWindow(usedPct, resetAt, now)
			rollingObserved = true
			seen = true
		case "7d":
			weekly = rollingWindow(usedPct, resetAt, now)
			weeklyObserved = true
			seen = true
		default:
			quotaWindows = append(quotaWindows, quotaWindow(window, usedPct, resetAt))
			seen = true
		}
	}
	if pool := resp.AccountPoolUsage; pool != nil {
		// Pool quota and per-key aggregate limits are different scopes. Never
		// borrow a missing pool window or reset time from an API-key allowance.
		rolling = wire.EmptyRollingWindow()
		weekly = wire.EmptyRollingWindow()
		rollingObserved, weeklyObserved, seen = false, false, false
		if pool.Primary != nil {
			rolling.UsedPct = clamp(1-(*pool.Primary/100), 0, 1)
			rollingObserved = true
			seen = true
		}
		if pool.Secondary != nil {
			weekly.UsedPct = clamp(1-(*pool.Secondary/100), 0, 1)
			weeklyObserved = true
			seen = true
		}
	}
	if !seen {
		return wire.UsageSnapshot{}, false
	}

	loginMethod := "codex-lb"
	return wire.UsageSnapshot{
		Schema:            1,
		Seq:               seq,
		GeneratedAtUTC:    wire.FormatTime(now),
		ProducerID:        producer.ID,
		ProducerTimeZone:  producer.TimeZone,
		Provider:          wire.ProviderCodex,
		BurnState:         "idle",
		Rolling5h:         rolling,
		Weekly:            weekly,
		Rolling5hObserved: rollingObserved,
		WeeklyObserved:    weeklyObserved,
		QuotaWindows:      quotaWindows,
		Credits:           nil,
		Extras: wire.SnapshotExtras{
			LoginMethod:      &loginMethod,
			AccountEmail:     nil,
			RateLimitTier:    nil,
			ExtraRateWindows: []json.RawMessage{},
		},
		Status: wire.SnapshotStatus{
			State:           wire.StateOK,
			DataSource:      wire.DataSourceAPIOnly,
			QuotaSource:     wire.QuotaSourceCodexLB,
			ActivitySources: []string{},
			Stale:           false,
		},
	}, true
}

func validateUsageResponse(resp usageResponse) error {
	seen := false
	for _, limit := range resp.UpstreamLimits {
		if !isAggregateCreditLimit(limit) {
			continue
		}
		seen = true
		if strings.TrimSpace(limit.LimitWindow) == "" {
			return errors.New("codex-lb aggregate limit has empty window")
		}
		if !finite(limit.MaxValue) || !finite(limit.CurrentValue) || !finite(limit.RemainingValue) {
			return errors.New("codex-lb aggregate limit has non-finite values")
		}
		if limit.MaxValue <= 0 || limit.CurrentValue < 0 || limit.CurrentValue > limit.MaxValue {
			return errors.New("codex-lb aggregate limit has out-of-range current value")
		}
		if limit.RemainingValue < 0 || limit.RemainingValue > limit.MaxValue {
			return errors.New("codex-lb aggregate limit has out-of-range remaining value")
		}
		if limit.ResetAt != nil && strings.TrimSpace(*limit.ResetAt) != "" && parseResetAt(limit.ResetAt).IsZero() {
			return errors.New("codex-lb aggregate limit has invalid reset_at")
		}
	}
	if pool := resp.AccountPoolUsage; pool != nil {
		for _, remaining := range []*float64{pool.Primary, pool.Secondary} {
			if remaining == nil {
				continue
			}
			seen = true
			if !finite(*remaining) || *remaining < 0 || *remaining > 100 {
				return errors.New("codex-lb account pool has invalid remaining percentage")
			}
		}
	}
	if !seen {
		return errors.New("codex-lb response has no usable quota")
	}
	return nil
}

func finite(value float64) bool {
	return !math.IsNaN(value) && !math.IsInf(value, 0)
}

func rollingWindow(usedPct float64, resetAt time.Time, now time.Time) wire.RollingWindow {
	var resets *string
	if !resetAt.IsZero() {
		s := wire.FormatTime(resetAt)
		resets = &s
	}
	return wire.RollingWindow{
		UsedPct:          usedPct,
		RemainingSeconds: remainingSeconds(resetAt, now),
		ResetsAt:         resets,
	}
}

func quotaWindow(label string, usedPct float64, resetAt time.Time) wire.QuotaWindow {
	var resets *string
	if !resetAt.IsZero() {
		s := wire.FormatTime(resetAt)
		resets = &s
	}
	return wire.QuotaWindow{
		Label:    label,
		Scope:    label,
		UsedPct:  usedPct,
		ResetsAt: resets,
	}
}

func isAggregateCreditLimit(limit upstreamLimit) bool {
	return strings.EqualFold(limit.Source, "aggregate") && strings.EqualFold(limit.LimitType, "credits")
}

func normalizedWindow(window string) string {
	w := strings.ToLower(strings.TrimSpace(window))
	switch w {
	case "5hr", "5hrs", "5hour", "5hours", "primary":
		return "5h"
	case "7day", "7days", "1w", "1week", "1weeks", "week", "weekly", "secondary":
		return "7d"
	case "":
		return "unknown"
	default:
		return w
	}
}

func parseResetAt(value *string) time.Time {
	if value == nil {
		return time.Time{}
	}
	raw := strings.TrimSpace(*value)
	if raw == "" {
		return time.Time{}
	}
	for _, layout := range []string{time.RFC3339Nano, time.RFC3339} {
		if t, err := time.Parse(layout, raw); err == nil {
			return t
		}
	}
	if seconds, err := strconv.ParseInt(raw, 10, 64); err == nil && seconds > 0 {
		return time.Unix(seconds, 0).UTC()
	}
	return time.Time{}
}

func remainingSeconds(resetAt time.Time, now time.Time) int {
	if resetAt.IsZero() || !resetAt.After(now) {
		return 0
	}
	return int(math.Round(resetAt.Sub(now).Seconds()))
}

func clamp(value, minValue, maxValue float64) float64 {
	if value < minValue {
		return minValue
	}
	if value > maxValue {
		return maxValue
	}
	return value
}

func normalizeBaseURL(raw string) string {
	base := strings.TrimSpace(raw)
	if base == "" {
		return defaultURL
	}
	if parsed, err := url.Parse(base); err == nil {
		parsed.Path = strings.TrimSuffix(parsed.Path, "/v1")
		parsed.RawQuery = ""
		parsed.Fragment = ""
		return strings.TrimRight(parsed.String(), "/")
	}
	return strings.TrimRight(strings.TrimSuffix(base, "/v1"), "/")
}

func isSafeBaseURL(raw string) bool {
	parsed, err := url.Parse(strings.TrimSpace(raw))
	if err != nil || parsed.Hostname() == "" || parsed.User != nil || parsed.RawQuery != "" || parsed.Fragment != "" {
		return false
	}
	switch strings.ToLower(parsed.Scheme) {
	case "https":
		return true
	case "http":
		host := strings.TrimSuffix(strings.ToLower(parsed.Hostname()), ".")
		return host == "localhost" || host == "127.0.0.1" || host == "::1"
	default:
		return false
	}
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if strings.TrimSpace(value) != "" {
			return value
		}
	}
	return ""
}
