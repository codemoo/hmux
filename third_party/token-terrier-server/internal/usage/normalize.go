package usage

import (
	"encoding/json"
	"math"
	"strconv"
	"strings"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/auth"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

// NormalizeClaude turns ClaudeUsageResponse into the wire UsageSnapshot.
// Mirrors UsageNormalizer.normalizeClaude.
func NormalizeClaude(resp *claudeUsageResponse, credential auth.OAuthCredential, seq int, producer wire.ProducerInfo, now time.Time) wire.UsageSnapshot {
	generatedAt := wire.FormatTime(now)
	fiveHour := claudeRolling(resp.FiveHour, now)
	weekly := claudeRolling(resp.SevenDay, now)
	var quotaWindows []wire.QuotaWindow
	if resp.SevenDaySonnet != nil {
		quotaWindows = append(quotaWindows, claudeQuotaWindow("sonnet", resp.SevenDaySonnet))
	}
	if resp.SevenDayOpus != nil {
		quotaWindows = append(quotaWindows, claudeQuotaWindow("opus", resp.SevenDayOpus))
	}
	if quotaWindows == nil {
		quotaWindows = []wire.QuotaWindow{}
	}
	tier := stringPtrOrNil(credential.RateLimitTier)
	extraWindows := resp.ExtraRateWindows
	if extraWindows == nil {
		// Wire format requires [] not null for this field. Swift's
		// JSONEncoder serializes empty arrays explicitly; we have to
		// be explicit too.
		extraWindows = []json.RawMessage{}
	}
	return wire.UsageSnapshot{
		Schema:            1,
		Seq:               seq,
		GeneratedAtUTC:    generatedAt,
		ProducerID:        producer.ID,
		ProducerTimeZone:  producer.TimeZone,
		Provider:          wire.ProviderClaude,
		BurnState:         "idle",
		Rolling5h:         fiveHour,
		Weekly:            weekly,
		Rolling5hObserved: resp.FiveHour != nil,
		WeeklyObserved:    resp.SevenDay != nil,
		QuotaWindows:      quotaWindows,
		Credits:           nil,
		Extras: wire.SnapshotExtras{
			LoginMethod:      nil,
			AccountEmail:     nil,
			RateLimitTier:    tier,
			ExtraRateWindows: extraWindows,
		},
		Status: wire.SnapshotStatus{
			State:           wire.StateOK,
			DataSource:      wire.DataSourceAPIOnly,
			QuotaSource:     wire.QuotaSourceOAuth,
			ActivitySources: []string{},
			Stale:           false,
			QuotaObservedAt: &generatedAt,
		},
	}
}

// NormalizeCodex turns CodexUsageResponse into the wire UsageSnapshot.
// Mirrors UsageNormalizer.normalizeCodex.
func NormalizeCodex(resp *codexUsageResponse, credential auth.OAuthCredential, seq int, producer wire.ProducerInfo, now time.Time) wire.UsageSnapshot {
	generatedAt := wire.FormatTime(now)
	primary := resp.primaryWindow()
	secondary := resp.secondaryWindow()
	rolling := codexRolling(primary, now)
	weekly := codexRolling(secondary, now)
	var windows []wire.QuotaWindow
	if t := resp.tertiaryWindow(); t != nil {
		scope := "rolling"
		if seconds := codexWindowSeconds(t); seconds >= 7*24*60*60 {
			scope = "weekly"
		}
		windows = append(windows, codexQuotaWindow("tertiary", scope, t))
	}
	if windows == nil {
		windows = []wire.QuotaWindow{}
	}
	var credits *wire.Credits
	if r := resp.Credits.effectiveRemaining(); r != nil {
		updatedAt := strings.TrimSpace(resp.Credits.UpdatedAt)
		if updatedAt == "" {
			updatedAt = generatedAt
		}
		credits = &wire.Credits{Remaining: *r, UpdatedAt: &updatedAt}
	}
	loginMethod := resp.effectiveLoginMethod()
	return wire.UsageSnapshot{
		Schema:            1,
		Seq:               seq,
		GeneratedAtUTC:    generatedAt,
		ProducerID:        producer.ID,
		ProducerTimeZone:  producer.TimeZone,
		Provider:          wire.ProviderCodex,
		BurnState:         "idle",
		Rolling5h:         rolling,
		Weekly:            weekly,
		Rolling5hObserved: primary != nil,
		WeeklyObserved:    secondary != nil,
		QuotaWindows:      windows,
		Credits:           credits,
		Extras: wire.SnapshotExtras{
			LoginMethod:      stringPtrOrNil(loginMethod),
			AccountEmail:     stringPtrOrNil(coalesce(resp.AccountEmail, credential.AccountEmail)),
			RateLimitTier:    nil,
			ExtraRateWindows: []json.RawMessage{},
		},
		Status: wire.SnapshotStatus{
			State:           wire.StateOK,
			DataSource:      wire.DataSourceAPIOnly,
			QuotaSource:     wire.QuotaSourceOAuth,
			ActivitySources: []string{},
			Stale:           false,
			QuotaObservedAt: &generatedAt,
		},
	}
}

func claudeRolling(w *claudeWindow, now time.Time) wire.RollingWindow {
	if w == nil {
		return wire.EmptyRollingWindow()
	}
	resetText := claudeResetText(w)
	resetT := parseISO(resetText)
	resets := stringPtrOrNil(resetText)
	return wire.RollingWindow{
		UsedPct:          percentToRatio(*w.Utilization),
		RemainingSeconds: remainingSeconds(resetT, now),
		ResetsAt:         resets,
	}
}

func claudeQuotaWindow(label string, w *claudeWindow) wire.QuotaWindow {
	resetText := claudeResetText(w)
	return wire.QuotaWindow{
		Label:    label,
		Scope:    "weekly",
		UsedPct:  percentToRatio(*w.Utilization),
		ResetsAt: stringPtrOrNil(resetText),
	}
}

func claudeResetText(w *claudeWindow) string {
	if w == nil || w.ResetsAt == nil {
		return ""
	}
	return strings.TrimSpace(*w.ResetsAt)
}

func codexRolling(w *codexWindow, now time.Time) wire.RollingWindow {
	if w == nil {
		return wire.EmptyRollingWindow()
	}
	pct := codexUsedPct(w)
	resetT := codexResetTime(w)
	var resets *string
	if !resetT.IsZero() {
		s := wire.FormatTime(resetT)
		resets = &s
	}
	return wire.RollingWindow{
		UsedPct:          percentToRatio(pct),
		RemainingSeconds: remainingSeconds(resetT, now),
		ResetsAt:         resets,
	}
}

func codexQuotaWindow(label, scope string, w *codexWindow) wire.QuotaWindow {
	pct := codexUsedPct(w)
	resetT := codexResetTime(w)
	var resets *string
	if !resetT.IsZero() {
		s := wire.FormatTime(resetT)
		resets = &s
	}
	return wire.QuotaWindow{
		Label:    label,
		Scope:    scope,
		UsedPct:  percentToRatio(pct),
		ResetsAt: resets,
	}
}

func codexUsedPct(w *codexWindow) float64 {
	if v := w.UsedPercentCamel.Ptr(); v != nil {
		return *v
	}
	if v := w.UsedPercentSnake.Ptr(); v != nil {
		return *v
	}
	return 0
}

func codexResetTime(w *codexWindow) time.Time {
	if w.ResetsAtRaw != nil {
		s := strings.TrimSpace(*w.ResetsAtRaw)
		if s != "" {
			if t := parseISO(s); !t.IsZero() {
				return t
			}
		}
	}
	if s := w.ResetAtRaw.String(); s != "" {
		if i, err := strconv.ParseInt(s, 10, 64); err == nil {
			return time.Unix(i, 0)
		}
		if f, err := strconv.ParseFloat(s, 64); err == nil {
			sec, frac := math.Modf(f)
			return time.Unix(int64(sec), int64(frac*1e9))
		}
	}
	return time.Time{}
}

func codexWindowSeconds(w *codexWindow) int {
	if v := w.LimitWindowSeconds.Ptr(); v != nil {
		return *v
	}
	if v := w.WindowMinutes.Ptr(); v != nil {
		return *v * 60
	}
	return 0
}

func percentToRatio(p float64) float64 {
	r := p / 100.0
	if r < 0 {
		return 0
	}
	if r > 1 {
		return 1
	}
	return r
}

func remainingSeconds(reset, now time.Time) int {
	if reset.IsZero() {
		return 0
	}
	d := reset.Sub(now)
	if d < 0 {
		return 0
	}
	return int(d.Truncate(time.Second).Seconds())
}

// parseISO matches SnapshotDateFormatter.date(from:): tries fractional ISO
// first, then plain.
func parseISO(s string) time.Time {
	if s == "" {
		return time.Time{}
	}
	for _, layout := range []string{
		"2006-01-02T15:04:05.000Z",
		"2006-01-02T15:04:05Z",
		time.RFC3339Nano,
		time.RFC3339,
	} {
		if t, err := time.Parse(layout, s); err == nil {
			return t
		}
	}
	return time.Time{}
}

func stringPtrOrNil(s string) *string {
	if strings.TrimSpace(s) == "" {
		return nil
	}
	return &s
}

func coalesce(values ...string) string {
	for _, v := range values {
		if strings.TrimSpace(v) != "" {
			return v
		}
	}
	return ""
}
