package claudeswap

import (
	"sync"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/jsonl"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

// ActivitySnapshot is the live JSONL-derived activity attached to one
// claude-swap account row.
type ActivitySnapshot struct {
	TokensPerHour float64
	TotalTokens   int64
}

// ActivityProvider supplies live per-account activity for Reader overlays.
type ActivityProvider interface {
	Snapshot(accountNumber int, now time.Time) (ActivitySnapshot, bool)
}

// ActivityTracker tracks live Claude usage by claude-swap account number.
// It intentionally mirrors the daemon's burn tracker shape, but reports
// tokens/hour plus today's total for account detail rows.
type ActivityTracker struct {
	mu sync.Mutex

	timeZone *time.Location
	window   time.Duration
	accounts map[int]*accountActivity
}

type accountActivity struct {
	day        time.Time
	todayTotal int64
	window     []activityPoint
}

type activityPoint struct {
	t      time.Time
	tokens int
}

// NewActivityTracker builds a per-account activity tracker. Timezone is used
// for daily rollover; pass nil for time.Local.
func NewActivityTracker(timeZone *time.Location, now time.Time) *ActivityTracker {
	if timeZone == nil {
		timeZone = time.Local
	}
	return &ActivityTracker{
		timeZone: timeZone,
		window:   time.Minute,
		accounts: map[int]*accountActivity{},
	}
}

// Ingest records an account-tagged Claude JSONL token event.
func (t *ActivityTracker) Ingest(ev jsonl.TokenEvent, now time.Time) {
	if t == nil || ev.Provider != wire.ProviderClaude || ev.AccountNumber <= 0 || ev.Tokens <= 0 {
		return
	}
	when := ev.Timestamp
	if when.IsZero() {
		when = now
	}

	t.mu.Lock()
	defer t.mu.Unlock()

	acct := t.accounts[ev.AccountNumber]
	if acct == nil {
		acct = &accountActivity{}
		t.accounts[ev.AccountNumber] = acct
	}
	t.rolloverIfNeededLocked(acct, when)
	acct.todayTotal += int64(ev.Tokens)
	acct.window = append(acct.window, activityPoint{t: when, tokens: ev.Tokens})
	t.evictExpiredLocked(acct, now)
}

// Snapshot returns the current tokens/hour and today's total for one account.
func (t *ActivityTracker) Snapshot(accountNumber int, now time.Time) (ActivitySnapshot, bool) {
	if t == nil || accountNumber <= 0 {
		return ActivitySnapshot{}, false
	}

	t.mu.Lock()
	defer t.mu.Unlock()

	acct := t.accounts[accountNumber]
	if acct == nil {
		return ActivitySnapshot{}, false
	}
	t.rolloverIfNeededLocked(acct, now)
	t.evictExpiredLocked(acct, now)

	sliding := 0
	for _, point := range acct.window {
		sliding += point.tokens
	}
	if acct.todayTotal <= 0 && sliding <= 0 {
		delete(t.accounts, accountNumber)
		return ActivitySnapshot{}, false
	}
	perHour := float64(sliding) * float64(time.Hour) / float64(t.window)
	return ActivitySnapshot{
		TokensPerHour: perHour,
		TotalTokens:   acct.todayTotal,
	}, true
}

func (t *ActivityTracker) evictExpiredLocked(acct *accountActivity, now time.Time) {
	cutoff := now.Add(-t.window)
	idx := 0
	for ; idx < len(acct.window); idx++ {
		if !acct.window[idx].t.Before(cutoff) {
			break
		}
	}
	if idx > 0 {
		acct.window = append(acct.window[:0], acct.window[idx:]...)
	}
}

func (t *ActivityTracker) rolloverIfNeededLocked(acct *accountActivity, now time.Time) {
	day := startOfDayIn(t.timeZone, now)
	if !acct.day.Equal(day) {
		acct.day = day
		acct.todayTotal = 0
	}
}

func startOfDayIn(loc *time.Location, t time.Time) time.Time {
	if loc == nil {
		loc = time.Local
	}
	lt := t.In(loc)
	y, m, d := lt.Date()
	return time.Date(y, m, d, 0, 0, 0, 0, loc)
}
