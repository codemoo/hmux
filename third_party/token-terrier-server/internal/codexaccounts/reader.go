// Package codexaccounts reads an existing codex-lb derived JSON export and
// exposes per-account Codex usage. HMux treats the export as read-only and
// does not manage its producer, log into codex-lb, or create background jobs.
package codexaccounts

import (
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"math"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/safefile"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

// codex-lb-accounts derived JSON shape (schemaVersion 1). Only fields we use.
type derivedList struct {
	SchemaVersion   int             `json:"schemaVersion"`
	AccountsUpdated string          `json:"accountsUpdatedAt"`
	Accounts        json.RawMessage `json:"accounts"`
}

type derivedAccount struct {
	Number           int      `json:"number"`
	AccountID        string   `json:"accountId"`
	Email            string   `json:"email"`
	Alias            string   `json:"alias"`
	DisplayName      string   `json:"displayName"`
	Status           string   `json:"status"`
	FiveHourPct      *float64 `json:"fiveHourPct"`
	SevenDayPct      *float64 `json:"sevenDayPct"`
	ResetAtPrimary   *string  `json:"resetAtPrimary"`
	ResetAtSecondary *string  `json:"resetAtSecondary"`
	TotalTokens      *int64   `json:"totalTokens"`
	TokensPerHour    *float64 `json:"tokensPerHour"`
	LastRefreshAt    *string  `json:"lastRefreshAt"`
	Plan             string   `json:"plan"`
	PlanType         string   `json:"planType"`
	PlanTypeSnake    string   `json:"plan_type"`
}

const (
	// Refresh jobs normally run every five minutes and the menu app warns at
	// ten. Thirty minutes leaves an explicit warning window while preventing
	// an optional source failure from serving account data indefinitely.
	defaultMaxLastGoodAge = 30 * time.Minute
	maxSourceFutureSkew   = 5 * time.Minute
	maximumSourceBytes    = 8 * 1024 * 1024
	maximumAccounts       = 128
)

// ReaderState describes the most recent attempt to observe the optional
// codex-lb account snapshot. Missing is separate from the error states because
// an absent file is a valid configuration when codex-lb is unused.
type ReaderState string

const (
	ReaderStateUnobserved ReaderState = "unobserved"
	ReaderStateFresh      ReaderState = "fresh"
	ReaderStateMissing    ReaderState = "missing"
	ReaderStateStatError  ReaderState = "stat_error"
	ReaderStateReadError  ReaderState = "read_error"
	ReaderStateParseError ReaderState = "parse_error"
)

// ReaderStatus is a privacy-safe account-source diagnostic. AgeSeconds is
// measured from the payload's accountsUpdatedAt (or file mtime fallback), not
// the time this process read it, so old data cannot appear freshly observed.
type ReaderStatus struct {
	State             ReaderState `json:"state"`
	UsingLastGood     bool        `json:"using_last_good"`
	LastGoodExpired   bool        `json:"last_good_expired"`
	LastCheckedAt     *string     `json:"last_checked_at,omitempty"`
	LastSuccessAt     *string     `json:"last_success_at,omitempty"`
	LastErrorAt       *string     `json:"last_error_at,omitempty"`
	LastErrorKind     string      `json:"last_error_kind,omitempty"`
	SourceUpdatedAt   *string     `json:"source_updated_at,omitempty"`
	AgeSeconds        *int64      `json:"age_seconds,omitempty"`
	MaxLastGoodAgeSec int64       `json:"max_last_good_age_seconds"`
}

// parseAccounts converts a codex-lb-accounts derived payload to
// wire.AccountUsage. Returns (nil, nil) when there are zero accounts. The
// whole replacement is rejected for malformed schema, invalid/duplicate
// account numbers, or non-finite percentages so last-good stays coherent.
func parseAccounts(data []byte) ([]wire.AccountUsage, string, error) {
	var list derivedList
	if err := json.Unmarshal(data, &list); err != nil {
		return nil, "", err
	}
	if list.SchemaVersion != 1 {
		return nil, "", fmt.Errorf("unsupported codex-lb-accounts schemaVersion %d", list.SchemaVersion)
	}
	if len(list.Accounts) == 0 || strings.TrimSpace(string(list.Accounts)) == "null" {
		return nil, "", errors.New("codex-lb accounts must be an array")
	}
	if err := safefile.ValidateJSONArrayLimit(list.Accounts, maximumAccounts); err != nil {
		return nil, "", fmt.Errorf("validate codex-lb accounts: %w", err)
	}
	var accounts []derivedAccount
	if err := json.Unmarshal(list.Accounts, &accounts); err != nil {
		return nil, "", fmt.Errorf("decode codex-lb accounts: %w", err)
	}
	if len(accounts) == 0 {
		return nil, "", nil
	}
	out := make([]wire.AccountUsage, 0, len(accounts))
	seenNumbers := make(map[int]struct{}, len(accounts))
	for _, a := range accounts {
		if a.Number <= 0 {
			return nil, "", fmt.Errorf("codex-lb account number must be positive: %d", a.Number)
		}
		if _, duplicate := seenNumbers[a.Number]; duplicate {
			return nil, "", fmt.Errorf("duplicate codex-lb account number %d", a.Number)
		}
		seenNumbers[a.Number] = struct{}{}

		fiveHour, err := toWindow(a.FiveHourPct, a.ResetAtPrimary)
		if err != nil {
			return nil, "", fmt.Errorf("codex-lb account %d fiveHourPct: %w", a.Number, err)
		}
		sevenDay, err := toWindow(a.SevenDayPct, a.ResetAtSecondary)
		if err != nil {
			return nil, "", fmt.Errorf("codex-lb account %d sevenDayPct: %w", a.Number, err)
		}

		acc := wire.AccountUsage{
			Number: a.Number,
			Email:  accountLabel(a.Number, a.Alias, a.DisplayName, a.Email),
			// HMux displays the owner's codex-lb alias, never email/displayName.
			DisplayName:   accountLabel(a.Number, a.Alias),
			Active:        isActiveStatus(a.Status),
			Status:        normalizeStatus(a.Status),
			FiveHour:      fiveHour,
			SevenDay:      sevenDay,
			TokensPerHour: a.TokensPerHour,
			TotalTokens:   a.TotalTokens,
			LastRefreshAt: normalizeTimestamp(a.LastRefreshAt),
			PlanType:      wire.NormalizePlanType(firstNonEmpty(a.Plan, a.PlanType, a.PlanTypeSnake)),
		}
		out = append(out, acc)
	}
	return out, list.AccountsUpdated, nil
}

// toWindow builds an AccountWindow from a nullable used-pct and reset
// timestamp. The refresher converts codex-lb's remaining-pct into used-pct;
// this final boundary rejects non-finite values and clamps finite drift.
func toWindow(pct *float64, resetsAt *string) (*wire.AccountWindow, error) {
	if pct == nil {
		return nil, nil
	}
	if math.IsNaN(*pct) || math.IsInf(*pct, 0) {
		return nil, errors.New("percentage must be finite")
	}
	return &wire.AccountWindow{
		UsedPct:  clampUnit(*pct),
		ResetsAt: normalizeTimestamp(resetsAt),
	}, nil
}

// normalizeStatus maps codex-lb account status to the same vocabulary used
// by claude-swap accounts ("ok" for a healthy/active account). Known
// codex-lb aliases are folded so older refresher outputs and newer
// dashboard shapes render consistently.
func normalizeStatus(status string) string {
	trimmed := normalizeStatusToken(status)
	switch trimmed {
	case "active", "enabled", "healthy", "logged_in", "ok":
		return "ok"
	case "disabled", "inactive", "suspended":
		return "paused"
	case "auth_required", "auth_expired", "login_required", "reauth", "reauthrequired", "token_expired", "unauthorized":
		return "reauth_required"
	case "ratelimited":
		return "rate_limited"
	case "":
		return "unavailable"
	default:
		return trimmed
	}
}

func normalizeStatusToken(status string) string {
	trimmed := strings.ToLower(strings.TrimSpace(status))
	trimmed = strings.ReplaceAll(trimmed, "-", "_")
	trimmed = strings.ReplaceAll(trimmed, " ", "_")
	return trimmed
}

func isActiveStatus(status string) bool {
	return normalizeStatus(status) == "ok"
}

func normalizeTimestamp(raw *string) *string {
	if raw == nil {
		return nil
	}
	s := strings.TrimSpace(*raw)
	if s == "" {
		return nil
	}
	for _, layout := range []string{time.RFC3339Nano, time.RFC3339} {
		if t, err := time.Parse(layout, s); err == nil {
			out := wire.FormatTime(t.UTC())
			return &out
		}
	}
	if seconds, err := strconv.ParseInt(s, 10, 64); err == nil && seconds > 0 {
		out := wire.FormatTime(time.Unix(seconds, 0).UTC())
		return &out
	}
	return nil
}

func normalizeTimestampString(raw string) *string {
	return normalizeTimestamp(&raw)
}

func clampUnit(v float64) float64 {
	if v < 0 {
		return 0
	}
	if v > 1 {
		return 1
	}
	return v
}

func firstNonEmpty(values ...string) string {
	for _, v := range values {
		if trimmed := strings.TrimSpace(v); trimmed != "" {
			return trimmed
		}
	}
	return ""
}

func accountLabel(number int, values ...string) string {
	if label := firstNonEmpty(values...); label != "" {
		return label
	}
	return fmt.Sprintf("Account %d", number)
}

// Reader loads the codex-lb-accounts derived file, caching the parsed result
// and re-reading only when the file's mtime changes. Concurrency-safe. All
// checks are throttled to at most once per checkEvery to keep hot burn-event
// paths cheap.
type Reader struct {
	path           string
	logger         *slog.Logger
	checkEvery     time.Duration
	maxLastGoodAge time.Duration
	now            func() time.Time

	mu            sync.Mutex
	cachedAccts   []wire.AccountUsage
	cachedUpdated *string
	cachedSource  time.Time
	hasLastGood   bool
	lastMod       time.Time
	lastCheck     time.Time
	lastSuccess   time.Time
	lastError     time.Time
	lastErrorKind string
	readerState   ReaderState
	checked       bool
}

// NewReader builds a Reader for the given codex-lb-accounts derived file
// path.
func NewReader(path string, logger *slog.Logger) *Reader {
	if logger == nil {
		logger = slog.Default()
	}
	return &Reader{
		path:           path,
		logger:         logger,
		checkEvery:     2 * time.Second,
		maxLastGoodAge: defaultMaxLastGoodAge,
		now:            time.Now,
		readerState:    ReaderStateUnobserved,
	}
}

// Accounts returns current accounts plus accountsUpdatedAt. A malformed,
// unreadable, or temporarily missing replacement may use last-good until its
// source timestamp reaches maxLastGoodAge. Expired data is omitted. Never logs
// emails or labels — count only.
func (r *Reader) Accounts() ([]wire.AccountUsage, *string) {
	r.mu.Lock()
	defer r.mu.Unlock()

	now := r.now()
	if r.checked && now.Sub(r.lastCheck) < r.checkEvery {
		return r.resultLocked(now)
	}
	r.checked = true
	r.lastCheck = now

	snapshot, err := safefile.Read(r.path, maximumSourceBytes)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			// Genuine absence (e.g. refresher not installed) is not an error.
			// Retain a bounded last-good only to bridge transient replacement.
			r.readerState = ReaderStateMissing
			return r.resultLocked(now)
		}
		r.markErrorLocked(ReaderStateStatError, "stat", now)
		return r.resultLocked(now)
	}
	info := snapshot.Info
	if !r.lastMod.IsZero() && info.ModTime().Equal(r.lastMod) {
		r.readerState = ReaderStateFresh
		return r.resultLocked(now)
	}

	accts, updatedAt, perr := parseAccounts(snapshot.Data)
	if perr != nil {
		r.markErrorLocked(ReaderStateParseError, "parse", now)
		r.logger.Debug("codex-lb accounts parse failed", "err", perr)
		return r.resultLocked(now)
	}

	sourceTime := boundedSourceTime(info.ModTime(), now)
	if normalized := normalizeTimestampString(updatedAt); normalized != nil {
		if parsed, err := time.Parse("2006-01-02T15:04:05.000Z", *normalized); err == nil {
			sourceTime = boundedSourceTime(parsed, now)
		}
	}
	r.lastMod = info.ModTime()
	r.cachedAccts = accts
	r.cachedSource = sourceTime
	r.hasLastGood = true
	if accts == nil {
		r.cachedUpdated = nil
	} else {
		u := wire.FormatTime(sourceTime)
		r.cachedUpdated = &u
	}
	r.readerState = ReaderStateFresh
	r.lastSuccess = now
	r.logger.Debug("codex-lb accounts loaded", "count", len(accts))
	return r.resultLocked(now)
}

// Status returns lock-safe, JSON-ready account-source diagnostics. It does not
// trigger filesystem I/O; Accounts performs observations on its normal cadence.
func (r *Reader) Status() ReaderStatus {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.statusLocked(r.now())
}

func (r *Reader) markErrorLocked(state ReaderState, kind string, now time.Time) {
	r.readerState = state
	r.lastError = now
	r.lastErrorKind = kind
}

func (r *Reader) resultLocked(now time.Time) ([]wire.AccountUsage, *string) {
	if !r.lastGoodUsableLocked(now) {
		return nil, nil
	}
	return append([]wire.AccountUsage(nil), r.cachedAccts...), copyString(r.cachedUpdated)
}

func (r *Reader) lastGoodUsableLocked(now time.Time) bool {
	if !r.hasLastGood || r.cachedSource.IsZero() {
		return false
	}
	return sourceAge(now, r.cachedSource) <= r.maxLastGoodAge
}

func (r *Reader) statusLocked(now time.Time) ReaderStatus {
	status := ReaderStatus{
		State:             r.readerState,
		LastCheckedAt:     formatOptionalTime(r.lastCheck),
		LastSuccessAt:     formatOptionalTime(r.lastSuccess),
		LastErrorAt:       formatOptionalTime(r.lastError),
		LastErrorKind:     r.lastErrorKind,
		MaxLastGoodAgeSec: int64(r.maxLastGoodAge / time.Second),
	}
	if !r.hasLastGood || r.cachedSource.IsZero() {
		return status
	}
	age := sourceAge(now, r.cachedSource)
	ageSeconds := int64(age / time.Second)
	status.SourceUpdatedAt = formatOptionalTime(r.cachedSource)
	status.AgeSeconds = &ageSeconds
	status.LastGoodExpired = age > r.maxLastGoodAge
	status.UsingLastGood = r.readerState != ReaderStateFresh && !status.LastGoodExpired
	return status
}

func sourceAge(now, source time.Time) time.Duration {
	age := now.Sub(source)
	if age < 0 {
		return 0
	}
	return age
}

func boundedSourceTime(source, now time.Time) time.Time {
	source = source.UTC()
	if source.After(now.Add(maxSourceFutureSkew)) {
		return now.UTC()
	}
	return source
}

func formatOptionalTime(value time.Time) *string {
	if value.IsZero() {
		return nil
	}
	formatted := wire.FormatTime(value.UTC())
	return &formatted
}

func copyString(value *string) *string {
	if value == nil {
		return nil
	}
	copy := *value
	return &copy
}
