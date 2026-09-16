package burn

import (
	"math"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/jsonl"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

func TestLatePreviousDayEventDoesNotRollbackToday(t *testing.T) {
	loc := time.FixedZone("KST", 9*60*60)
	today := time.Date(2026, 7, 16, 9, 0, 0, 0, loc)
	yesterday := time.Date(2026, 7, 15, 23, 0, 0, 0, loc)
	tracker := New(loc, today)

	tracker.Ingest(eventAt(today, 100, "today"), today)
	snapshot := tracker.Ingest(
		eventAt(yesterday, 900, "yesterday"),
		today.Add(time.Second))

	if snapshot.TodayTotalTokens != 100 || snapshot.TodaySessionsCount != 1 {
		t.Fatalf("snapshot = %+v", snapshot)
	}
}

func TestOutOfOrderWindowSortedBeforeEviction(t *testing.T) {
	now := time.Unix(1_700_000_000, 0)
	tracker := New(time.UTC, now)
	tracker.ewmaTimeConstant = 0.001

	tracker.Ingest(eventAt(now.Add(-10*time.Second), 100, "newer"), now)
	tracker.Ingest(eventAt(now.Add(-40*time.Second), 200, "older"), now)
	snapshot := tracker.Snapshot(now.Add(30 * time.Second))

	if math.Abs(snapshot.RatePerMinute-100) >= 0.001 {
		t.Fatalf("rate = %f, want 100", snapshot.RatePerMinute)
	}
	if snapshot.TodayTotalTokens != 300 {
		t.Fatalf("today total = %d, want 300", snapshot.TodayTotalTokens)
	}
}

func TestFarFutureEventIgnored(t *testing.T) {
	now := time.Unix(1_700_000_000, 0)
	tracker := New(time.UTC, now)

	snapshot := tracker.Ingest(
		eventAt(now.Add(time.Hour), 10_000, "future"),
		now)

	if snapshot.RatePerMinute != 0 || snapshot.TodayTotalTokens != 0 || snapshot.HasObserved {
		t.Fatalf("snapshot = %+v", snapshot)
	}
}

func TestForwardDayStillResetsTotal(t *testing.T) {
	dayOne := time.Date(2026, 7, 15, 9, 0, 0, 0, time.UTC)
	dayTwo := dayOne.Add(24 * time.Hour)
	tracker := New(time.UTC, dayOne)

	tracker.Ingest(eventAt(dayOne, 100, "first"), dayOne)
	snapshot := tracker.Ingest(eventAt(dayTwo, 25, "second"), dayTwo)

	if snapshot.TodayTotalTokens != 25 || snapshot.TodaySessionsCount != 1 {
		t.Fatalf("snapshot = %+v", snapshot)
	}
}

func TestActivitySourcesAreDistinctSortedAndResetDaily(t *testing.T) {
	dayOne := time.Date(2026, 7, 15, 9, 0, 0, 0, time.UTC)
	dayTwo := dayOne.Add(24 * time.Hour)
	tracker := New(time.UTC, dayOne)
	hermes := eventAt(dayOne, 10, "hermes-session")
	hermes.Source = "hermes"
	jsonlEvent := eventAt(dayOne.Add(time.Second), 20, "jsonl-session")
	jsonlEvent.Source = "jsonl"
	tracker.Ingest(hermes, dayOne)
	snapshot := tracker.Ingest(jsonlEvent, dayOne.Add(time.Second))
	if got := strings.Join(snapshot.ActivitySources, ","); got != "hermes,jsonl" {
		t.Fatalf("activity sources = %q", got)
	}

	next := eventAt(dayTwo, 5, "next-day")
	next.Source = "jsonl"
	snapshot = tracker.Ingest(next, dayTwo)
	if got := strings.Join(snapshot.ActivitySources, ","); got != "jsonl" {
		t.Fatalf("next-day activity sources = %q", got)
	}
}

func eventAt(timestamp time.Time, tokens int, session string) jsonl.TokenEvent {
	return jsonl.TokenEvent{
		Provider:   wire.ProviderClaude,
		Timestamp:  timestamp,
		Tokens:     tokens,
		SessionKey: session,
	}
}
