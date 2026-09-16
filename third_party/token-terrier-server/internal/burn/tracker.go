// Package burn computes a smoothed tokens-per-minute rate from JSONL
// TokenEvents and tracks today's totals + active session count.
//
// Mirrors Sources/TokenUsageCore/Burn/BurnRate.swift.
package burn

import (
	"math"
	"sort"
	"sync"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/jsonl"
)

// State names the six-step character animation state derived from rate.
type State string

const (
	StateIdle   State = "idle"
	StateWalk   State = "walk"
	StateJog    State = "jog"
	StateRun    State = "run"
	StateFly    State = "fly"
	StateRocket State = "rocket"
)

var stateLadder = []State{StateIdle, StateWalk, StateJog, StateRun, StateFly, StateRocket}

// Re-baselined for fresh-tokens-per-minute (cache creation and fresh input included)
// so all six tiers stay reachable on real Claude/Codex sessions. See Swift
// BurnState comments for the calibration story.
var (
	upperEnter = []float64{500, 3000, 12000, 40000, 100000}
	lowerExit  = []float64{400, 2400, 9600, 32000, 80000}
)

func indexOf(s State) int {
	for i, x := range stateLadder {
		if s == x {
			return i
		}
	}
	return 0
}

func atIdx(i int) State {
	if i < 0 {
		return stateLadder[0]
	}
	if i >= len(stateLadder) {
		return stateLadder[len(stateLadder)-1]
	}
	return stateLadder[i]
}

// nextState applies ±20% hysteresis and a 5s minimum dwell.
func nextState(value float64, current State, lastChange, now time.Time, minDwell time.Duration) State {
	elapsed := now.Sub(lastChange)
	if elapsed >= 0 && elapsed < minDwell {
		return current
	}
	idx := indexOf(current)
	for idx < len(upperEnter) && value >= upperEnter[idx] {
		idx++
	}
	if idx > indexOf(current) {
		return atIdx(idx)
	}
	for idx > 0 && value < lowerExit[idx-1] {
		idx--
	}
	return atIdx(idx)
}

// Snapshot is the burn-rate readout served alongside every UsageSnapshot.
type Snapshot struct {
	RatePerMinute      float64
	State              State
	TodayTotalTokens   int
	TodaySessionsCount int
	HasObserved        bool
	ActivitySources    []string
}

// Tracker computes the rate from a stream of TokenEvents. Thread-safe.
type Tracker struct {
	mu sync.Mutex

	timeZone         *time.Location
	windowSeconds    float64
	ewmaTimeConstant float64
	minDwell         time.Duration
	maxFutureSkew    time.Duration

	window         []timedTokens // tokens within the last windowSeconds
	ewma           float64
	lastEwmaUpdate time.Time

	todayDay      time.Time
	todayTotal    int
	todaySessions map[string]struct{}
	todaySources  map[string]struct{}
	hasObserved   bool

	currentState    State
	lastStateChange time.Time
}

type timedTokens struct {
	t time.Time
	n int
}

// New builds a Tracker rooted at `clock`. Timezone is used for daily rollover;
// pass nil for time.Local.
func New(timeZone *time.Location, clock time.Time) *Tracker {
	if timeZone == nil {
		timeZone = time.Local
	}
	return &Tracker{
		timeZone:         timeZone,
		windowSeconds:    60,
		ewmaTimeConstant: 20,
		minDwell:         5 * time.Second,
		maxFutureSkew:    5 * time.Minute,
		todaySessions:    map[string]struct{}{},
		todaySources:     map[string]struct{}{},
		currentState:     StateIdle,
		// Anchor dwell origin in the past so the first observed event
		// can transition out of idle immediately.
		lastStateChange: clock.Add(-time.Hour),
	}
}

// Ingest records a token event and returns the resulting snapshot.
func (t *Tracker) Ingest(ev jsonl.TokenEvent, now time.Time) Snapshot {
	t.mu.Lock()
	defer t.mu.Unlock()
	// The wall clock owns the active day bucket. Delayed events from a prior
	// day must not roll today's total backward.
	t.rolloverIfNeeded(now)
	if ev.Tokens < 0 || ev.Timestamp.Sub(now) > t.maxFutureSkew {
		return t.snapshotLocked(now)
	}
	eventTime := ev.Timestamp
	if eventTime.After(now) {
		eventTime = now
	}

	t.hasObserved = true
	if startOfDayIn(t.timeZone, eventTime).Equal(t.todayDay) {
		t.todayTotal += ev.Tokens
		t.todaySessions[ev.SessionKey] = struct{}{}
		if ev.Source != "" {
			t.todaySources[ev.Source] = struct{}{}
		}
	}

	cutoff := now.Add(-time.Duration(t.windowSeconds) * time.Second)
	contributesToRate := !eventTime.Before(cutoff)
	if contributesToRate {
		t.window = append(t.window, timedTokens{t: eventTime, n: ev.Tokens})
		sort.Slice(t.window, func(i, j int) bool {
			return t.window[i].t.Before(t.window[j].t)
		})
	}
	t.evictExpiredLocked(now)
	if contributesToRate {
		t.updateEwmaForEventLocked(ev, now)
	} else {
		t.decayEwmaLocked(now)
	}

	value := t.currentValueLocked()
	newState := nextState(value, t.currentState, t.lastStateChange, now, t.minDwell)
	if newState != t.currentState {
		t.currentState = newState
		t.lastStateChange = now
	}
	return t.makeSnapshotLocked(value)
}

// Snapshot returns the current burn snapshot without ingesting an event.
// Decays the EWMA in place so a slow stream doesn't keep a stale rate.
func (t *Tracker) Snapshot(now time.Time) Snapshot {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.snapshotLocked(now)
}

func (t *Tracker) snapshotLocked(now time.Time) Snapshot {
	t.rolloverIfNeeded(now)
	t.evictExpiredLocked(now)
	t.decayEwmaLocked(now)
	value := t.currentValueLocked()
	newState := nextState(value, t.currentState, t.lastStateChange, now, t.minDwell)
	if newState != t.currentState {
		t.currentState = newState
		t.lastStateChange = now
	}
	return t.makeSnapshotLocked(value)
}

func (t *Tracker) makeSnapshotLocked(value float64) Snapshot {
	sources := make([]string, 0, len(t.todaySources))
	for source := range t.todaySources {
		sources = append(sources, source)
	}
	sort.Strings(sources)
	return Snapshot{
		RatePerMinute:      value,
		State:              t.currentState,
		TodayTotalTokens:   t.todayTotal,
		TodaySessionsCount: len(t.todaySessions),
		HasObserved:        t.hasObserved,
		ActivitySources:    sources,
	}
}

func (t *Tracker) currentValueLocked() float64 {
	var sliding int
	for _, w := range t.window {
		sliding += w.n
	}
	scale := 60.0 / t.windowSeconds
	slidingValue := float64(sliding) * scale
	if t.ewma > slidingValue {
		return t.ewma
	}
	return slidingValue
}

func (t *Tracker) evictExpiredLocked(now time.Time) {
	cutoff := now.Add(-time.Duration(t.windowSeconds) * time.Second)
	kept := t.window[:0]
	for _, event := range t.window {
		if !event.t.Before(cutoff) && !event.t.After(now) {
			kept = append(kept, event)
		}
	}
	t.window = kept
}

func (t *Tracker) updateEwmaForEventLocked(ev jsonl.TokenEvent, now time.Time) {
	instant := float64(ev.Tokens) * 60.0 / t.windowSeconds
	if !t.lastEwmaUpdate.IsZero() {
		dt := now.Sub(t.lastEwmaUpdate).Seconds()
		if dt < 0.001 {
			dt = 0.001
		}
		alpha := 1 - math.Exp(-dt/t.ewmaTimeConstant)
		decayed := t.ewma * math.Exp(-dt/t.ewmaTimeConstant)
		t.ewma = decayed + alpha*(instant-decayed)
	} else {
		t.ewma = instant
	}
	t.lastEwmaUpdate = now
}

func (t *Tracker) decayEwmaLocked(now time.Time) {
	if t.lastEwmaUpdate.IsZero() {
		return
	}
	dt := now.Sub(t.lastEwmaUpdate).Seconds()
	if dt < 0 {
		dt = 0
	}
	t.ewma = t.ewma * math.Exp(-dt/t.ewmaTimeConstant)
	t.lastEwmaUpdate = now
}

func (t *Tracker) rolloverIfNeeded(now time.Time) {
	day := startOfDayIn(t.timeZone, now)
	if t.todayDay.IsZero() {
		t.todayDay = day
		return
	}
	if !day.After(t.todayDay) {
		return
	}
	t.todayDay = day
	t.todayTotal = 0
	t.todaySessions = map[string]struct{}{}
	t.todaySources = map[string]struct{}{}
}

func startOfDayIn(loc *time.Location, t time.Time) time.Time {
	tt := t.In(loc)
	return time.Date(tt.Year(), tt.Month(), tt.Day(), 0, 0, 0, 0, loc)
}
