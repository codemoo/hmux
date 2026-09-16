// Package claudeswap reads a claude-swap `--list --json` snapshot file and
// exposes per-account Claude usage. It never executes cswap or does network
// I/O. HMux normally reads the native cache; an existing export is optional.
package claudeswap

import (
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"math"
	"os"
	"strings"
	"sync"
	"time"
	"unicode"
	"unicode/utf8"

	"github.com/codemoo/token-terrier/server-go/internal/safefile"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

// cswap `--list --json` shape (schemaVersion 1). Only fields we use.
type cswapList struct {
	SchemaVersion       int             `json:"schemaVersion"`
	ActiveAccountNumber *int            `json:"activeAccountNumber"`
	Accounts            json.RawMessage `json:"accounts"`
}

type cswapAccount struct {
	Number      int         `json:"number"`
	Email       string      `json:"email"`
	Active      bool        `json:"active"`
	UsageStatus string      `json:"usageStatus"`
	Usage       *cswapUsage `json:"usage"`
}

type cswapUsage struct {
	FiveHour *cswapWindow `json:"fiveHour"`
	SevenDay *cswapWindow `json:"sevenDay"`
}

type cswapWindow struct {
	Pct      float64 `json:"pct"`
	ResetsAt *string `json:"resetsAt"`
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
// claude-swap account snapshot. Missing is separate from the error states
// because an absent file is a valid configuration when claude-swap is unused.
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
// measured from the source snapshot timestamp, not the time this process read
// it, so repeatedly reading an old file cannot make stale data look fresh.
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

// parseAccounts converts a cswap --list --json payload to wire.AccountUsage.
// Returns (nil, nil) when there are zero accounts. The whole replacement is
// rejected for malformed schema, invalid/duplicate account numbers, or
// non-finite percentages so a partially corrupt pool cannot replace last-good.
func parseAccounts(data []byte) ([]wire.AccountUsage, error) {
	var list cswapList
	if err := json.Unmarshal(data, &list); err != nil {
		return nil, err
	}
	if list.SchemaVersion != 1 {
		return nil, fmt.Errorf("unsupported claude-swap schemaVersion %d", list.SchemaVersion)
	}
	if len(list.Accounts) == 0 || strings.TrimSpace(string(list.Accounts)) == "null" {
		return nil, errors.New("claude-swap accounts must be an array")
	}
	if err := safefile.ValidateJSONArrayLimit(list.Accounts, maximumAccounts); err != nil {
		return nil, fmt.Errorf("validate claude-swap accounts: %w", err)
	}
	var accounts []cswapAccount
	if err := json.Unmarshal(list.Accounts, &accounts); err != nil {
		return nil, fmt.Errorf("decode claude-swap accounts: %w", err)
	}
	if len(accounts) == 0 {
		return nil, nil
	}
	out := make([]wire.AccountUsage, 0, len(accounts))
	seenNumbers := make(map[int]struct{}, len(accounts))
	for _, a := range accounts {
		if a.Number <= 0 {
			return nil, fmt.Errorf("claude-swap account number must be positive: %d", a.Number)
		}
		if _, duplicate := seenNumbers[a.Number]; duplicate {
			return nil, fmt.Errorf("duplicate claude-swap account number %d", a.Number)
		}
		seenNumbers[a.Number] = struct{}{}

		acc := wire.AccountUsage{
			Number: a.Number,
			Email:  accountLabel(a.Email, a.Number),
			Active: a.Active,
			Status: strings.TrimSpace(a.UsageStatus),
		}
		if acc.Status == "" {
			acc.Status = "unavailable"
		}
		if a.Usage != nil {
			var err error
			if acc.FiveHour, err = toWindow(a.Usage.FiveHour); err != nil {
				return nil, fmt.Errorf("claude-swap account %d fiveHour: %w", a.Number, err)
			}
			if acc.SevenDay, err = toWindow(a.Usage.SevenDay); err != nil {
				return nil, fmt.Errorf("claude-swap account %d sevenDay: %w", a.Number, err)
			}
		}
		out = append(out, acc)
	}
	activeCount, activeNumber := 0, 0
	for _, account := range out {
		if account.Active {
			activeCount++
			activeNumber = account.Number
		}
	}
	if activeCount > 1 || (list.ActiveAccountNumber != nil && *list.ActiveAccountNumber != activeNumber) {
		for i := range out {
			out[i].Active = false
		}
	}
	return out, nil
}

func toWindow(w *cswapWindow) (*wire.AccountWindow, error) {
	if w == nil {
		return nil, nil
	}
	if math.IsNaN(w.Pct) || math.IsInf(w.Pct, 0) {
		return nil, errors.New("percentage must be finite")
	}
	return &wire.AccountWindow{
		UsedPct:  clampUnit(w.Pct / 100.0),
		ResetsAt: normalizeReset(w.ResetsAt),
	}, nil
}

func accountLabel(raw string, number int) string {
	if label := strings.TrimSpace(raw); label != "" && len(label) <= 256 && utf8.ValidString(label) {
		safe := true
		for _, r := range label {
			if unicode.IsControl(r) || (r >= 0x202a && r <= 0x202e) || (r >= 0x2066 && r <= 0x2069) {
				safe = false
				break
			}
		}
		if safe {
			return label
		}
	}
	return fmt.Sprintf("Account %d", number)
}

// normalizeReset reformats a cswap RFC3339(+offset, fractional) timestamp to
// Token Terrier's canonical millisecond-Z form so the Swift date parser accepts
// it. Unparseable / empty → nil (tolerated).
func normalizeReset(raw *string) *string {
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
	return nil
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

// Reader loads the claude-swap accounts file, caching the parsed result and
// re-reading only when the file's mtime changes. Concurrency-safe. All checks
// are throttled to at most once per checkEvery to keep hot burn-event paths
// cheap.
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
	activity      ActivityProvider
}

// NewReader builds a Reader for the given accounts file path.
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

// SetActivityProvider attaches live JSONL-derived per-account activity.
func (r *Reader) SetActivityProvider(p ActivityProvider) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.activity = p
}

// ActiveAccountNumber returns the current claude-swap active account number,
// or 0 when no active account row is known.
func (r *Reader) ActiveAccountNumber() int {
	accts, _ := r.Accounts()
	active := 0
	for _, acct := range accts {
		if acct.Active && acct.Number > 0 {
			if active != 0 {
				return 0
			}
			active = acct.Number
		}
	}
	return active
}

// Accounts returns current accounts plus their source mtime. A malformed,
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
			// Genuine absence (e.g. claude-swap uninstalled) is not an error.
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

	accts, perr := parseAccounts(snapshot.Data)
	if perr != nil {
		r.markErrorLocked(ReaderStateParseError, "parse", now)
		r.logger.Debug("claude-swap accounts parse failed", "err", perr)
		return r.resultLocked(now)
	}

	sourceTime := boundedSourceTime(info.ModTime(), now)
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
	r.logger.Debug("claude-swap accounts loaded", "count", len(accts))
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
	return r.accountsWithActivityLocked(now), copyString(r.cachedUpdated)
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

func (r *Reader) accountsWithActivityLocked(now time.Time) []wire.AccountUsage {
	if r.cachedAccts == nil {
		return nil
	}
	out := append([]wire.AccountUsage(nil), r.cachedAccts...)
	if r.activity == nil {
		return out
	}
	for i := range out {
		stats, ok := r.activity.Snapshot(out[i].Number, now)
		if !ok {
			continue
		}
		tokensPerHour := stats.TokensPerHour
		totalTokens := stats.TotalTokens
		out[i].TokensPerHour = &tokensPerHour
		out[i].TotalTokens = &totalTokens
	}
	return out
}
