// Package wire holds the on-the-wire JSON types served by the daemon.
//
// These mirror the Swift types in Sources/TokenUsageCore/State/Snapshot.swift
// and SSE/SSEEvent.swift. The Go daemon must produce byte-equivalent JSON
// (modulo key ordering, which menubar Codable consumers ignore) so the
// existing menu bar client speaks to it without changes.
package wire

import (
	"encoding/json"
	"strings"
	"time"
)

// Provider identifies a quota provider exposed by the daemon.
type Provider string

const (
	ProviderClaude Provider = "claude"
	ProviderCodex  Provider = "codex"
)

// ProviderState describes the current fetch/auth state.
type ProviderState string

const (
	StateOK                   ProviderState = "ok"
	StateRefreshing           ProviderState = "refreshing"
	StateAuthExpired          ProviderState = "authExpired"
	StateNetworkError         ProviderState = "networkError"
	StateRateLimited          ProviderState = "rateLimited"
	StateCodexLoggedOut       ProviderState = "codexLoggedOut"
	StateQuotaEndpointChanged ProviderState = "quotaEndpointChanged"
)

// SnapshotDataSource identifies how the snapshot was produced.
type SnapshotDataSource string

const (
	DataSourceAPIOnly     SnapshotDataSource = "api_only"
	DataSourceAPIAndJSONL SnapshotDataSource = "api+jsonl"
	DataSourceJSONLOnly   SnapshotDataSource = "jsonl_only"
)

// QuotaSource and activity_sources separate the two independent provenance
// dimensions that the legacy data_source string conflates.
type QuotaSource string

const (
	QuotaSourceNone       QuotaSource = "none"
	QuotaSourceOAuth      QuotaSource = "oauth_api"
	QuotaSourceClaudeSwap QuotaSource = "claude_swap"
	QuotaSourceCodexLB    QuotaSource = "codex_lb"
)

// QuotaWindow is a single named quota window in normalized form.
type QuotaWindow struct {
	Label    string  `json:"label"`
	Scope    string  `json:"scope"`
	UsedPct  float64 `json:"used_pct"`
	ResetsAt *string `json:"resets_at"`
}

// RollingWindow is a normalized rolling quota window.
type RollingWindow struct {
	UsedPct          float64 `json:"used_pct"`
	RemainingSeconds int     `json:"remaining_seconds"`
	ResetsAt         *string `json:"resets_at"`
}

// EmptyRollingWindow matches Swift's RollingWindow.empty.
func EmptyRollingWindow() RollingWindow {
	return RollingWindow{UsedPct: 0, RemainingSeconds: 0, ResetsAt: nil}
}

// AccountWindow is one quota window for an optional provider account row.
type AccountWindow struct {
	UsedPct  float64 `json:"used_pct"`
	ResetsAt *string `json:"resets_at"`
}

// AccountUsage is one optional provider account's usage. Claude-swap and
// codex-lb readers normalize their source-specific statuses into this shared
// contract before attaching rows to a snapshot.
type AccountUsage struct {
	Number        int            `json:"number"`
	Email         string         `json:"email"`
	DisplayName   string         `json:"display_name,omitempty"`
	Active        bool           `json:"active"`
	Status        string         `json:"status"`
	FiveHour      *AccountWindow `json:"five_hour"`
	SevenDay      *AccountWindow `json:"seven_day"`
	TokensPerHour *float64       `json:"tokens_per_hour,omitempty"`
	TotalTokens   *int64         `json:"total_tokens,omitempty"`
	LastRefreshAt *string        `json:"last_refresh_at,omitempty"`
	PlanType      string         `json:"plan_type,omitempty"`
}

// Credits captures credit balance information when a provider exposes it.
type Credits struct {
	Remaining float64 `json:"remaining"`
	UpdatedAt *string `json:"updated_at"`
}

// SnapshotExtras carries provider-specific metadata.
type SnapshotExtras struct {
	LoginMethod      *string           `json:"login_method"`
	AccountEmail     *string           `json:"account_email"`
	RateLimitTier    *string           `json:"rate_limit_tier"`
	ExtraRateWindows []json.RawMessage `json:"extra_rate_windows"`
}

// EmptySnapshotExtras matches Swift's SnapshotExtras.empty.
func EmptySnapshotExtras() SnapshotExtras {
	return SnapshotExtras{
		LoginMethod:      nil,
		AccountEmail:     nil,
		RateLimitTier:    nil,
		ExtraRateWindows: []json.RawMessage{},
	}
}

// SnapshotStatus captures the fetch status embedded in every snapshot.
type SnapshotStatus struct {
	State           ProviderState      `json:"state"`
	DataSource      SnapshotDataSource `json:"data_source"`
	QuotaSource     QuotaSource        `json:"quota_source"`
	ActivitySources []string           `json:"activity_sources"`
	Stale           bool               `json:"stale"`
	QuotaObservedAt *string            `json:"quota_observed_at,omitempty"`
	RetryAt         *string            `json:"retry_at,omitempty"`
}

// UsageSnapshot is the final SSE / /snapshot schema served to clients.
type UsageSnapshot struct {
	Schema            int            `json:"schema"`
	Seq               int            `json:"seq"`
	GeneratedAtUTC    string         `json:"generated_at_utc"`
	ProducerID        string         `json:"producer_id"`
	ProducerTimeZone  string         `json:"producer_tz"`
	Provider          Provider       `json:"provider"`
	PlanType          string         `json:"plan_type,omitempty"`
	BurnRatePerMinute float64        `json:"burn_rate_per_min"`
	BurnState         string         `json:"burn_state"`
	TodayTotalTokens  int            `json:"today_total_tokens"`
	TodaySessions     int            `json:"today_sessions"`
	Rolling5h         RollingWindow  `json:"rolling_5h"`
	Weekly            RollingWindow  `json:"weekly"`
	Rolling5hObserved bool           `json:"rolling_5h_observed"`
	WeeklyObserved    bool           `json:"weekly_observed"`
	QuotaWindows      []QuotaWindow  `json:"quota_windows"`
	Credits           *Credits       `json:"credits"`
	Extras            SnapshotExtras `json:"extras"`
	Status            SnapshotStatus `json:"status"`
	// Accounts is optional and provider-agnostic; omitempty preserves the
	// compact wire shape when no account-pool integration is configured.
	Accounts        []AccountUsage `json:"accounts,omitempty"`
	AccountsUpdated *string        `json:"accounts_updated_at,omitempty"`
}

// NormalizePlanType exposes only plan categories the UI knows how to label. Raw
// provider strings are never transported, and quota percentages are never
// used to infer a plan.
func NormalizePlanType(value string) string {
	normalized := strings.ToLower(strings.TrimSpace(value))
	normalized = strings.ReplaceAll(normalized, "chatgpt_", "")
	normalized = strings.ReplaceAll(normalized, "chatgpt-", "")
	switch normalized {
	case "free", "plus", "pro", "team", "business", "enterprise", "edu", "go":
		return normalized
	default:
		return ""
	}
}

// ProducerInfo is stable producer metadata picked up from env at boot.
type ProducerInfo struct {
	ID       string
	TimeZone string
}

// Degraded builds a snapshot that still satisfies the schema when the
// upstream provider is unreachable / unauthorized / contractually broken.
// Mirrors UsageSnapshot.degraded(...) in Snapshot.swift.
func Degraded(provider Provider, seq int, producer ProducerInfo, now time.Time, state ProviderState) UsageSnapshot {
	return UsageSnapshot{
		Schema:           1,
		Seq:              seq,
		GeneratedAtUTC:   FormatTime(now),
		ProducerID:       producer.ID,
		ProducerTimeZone: producer.TimeZone,
		Provider:         provider,
		BurnState:        "idle",
		Rolling5h:        EmptyRollingWindow(),
		Weekly:           EmptyRollingWindow(),
		QuotaWindows:     []QuotaWindow{},
		Credits:          nil,
		Extras:           EmptySnapshotExtras(),
		Status: SnapshotStatus{
			State:           state,
			DataSource:      DataSourceAPIOnly,
			QuotaSource:     QuotaSourceNone,
			ActivitySources: []string{},
			Stale:           true,
		},
	}
}

// FormatTime emits ISO8601 with millisecond precision in UTC ("Z"), matching
// Swift's SnapshotDateFormatter.string(from:) so consumer parsers don't see
// a different shape on the same date.
//
//	2026-05-05T00:30:00.123Z
func FormatTime(t time.Time) string {
	// time.RFC3339Nano emits up to 9 fractional digits and trims trailing
	// zeros — both of which would diverge from Swift's fixed-3-digit
	// fractional seconds. Use an explicit layout instead.
	return t.UTC().Format("2006-01-02T15:04:05.000Z")
}
