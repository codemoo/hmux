package claudeswap

import (
	"math"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/jsonl"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

func TestActivityTrackerTracksAccountRateAndTotal(t *testing.T) {
	now := time.Date(2026, 7, 4, 10, 0, 0, 0, time.UTC)
	tracker := NewActivityTracker(time.UTC, now)

	tracker.Ingest(jsonl.TokenEvent{
		Provider:      wire.ProviderClaude,
		AccountNumber: 1,
		Timestamp:     now,
		Tokens:        120,
	}, now)

	snap, ok := tracker.Snapshot(1, now)
	if !ok {
		t.Fatal("expected activity snapshot")
	}
	if math.Abs(snap.TokensPerHour-7200) > 1e-9 {
		t.Fatalf("tokens/hour = %v, want 7200", snap.TokensPerHour)
	}
	if snap.TotalTokens != 120 {
		t.Fatalf("total = %d, want 120", snap.TotalTokens)
	}
}

func TestActivityTrackerIsolatesAccounts(t *testing.T) {
	now := time.Date(2026, 7, 4, 10, 0, 0, 0, time.UTC)
	tracker := NewActivityTracker(time.UTC, now)

	tracker.Ingest(jsonl.TokenEvent{Provider: wire.ProviderClaude, AccountNumber: 1, Timestamp: now, Tokens: 100}, now)
	tracker.Ingest(jsonl.TokenEvent{Provider: wire.ProviderClaude, AccountNumber: 2, Timestamp: now, Tokens: 40}, now)

	one, ok := tracker.Snapshot(1, now)
	if !ok || one.TotalTokens != 100 {
		t.Fatalf("account 1 = %+v/%v, want total 100", one, ok)
	}
	two, ok := tracker.Snapshot(2, now)
	if !ok || two.TotalTokens != 40 {
		t.Fatalf("account 2 = %+v/%v, want total 40", two, ok)
	}
}

func TestActivityTrackerKeepsTodayTotalAfterRateWindowExpires(t *testing.T) {
	now := time.Date(2026, 7, 4, 10, 0, 0, 0, time.UTC)
	tracker := NewActivityTracker(time.UTC, now)

	tracker.Ingest(jsonl.TokenEvent{Provider: wire.ProviderClaude, AccountNumber: 1, Timestamp: now, Tokens: 50}, now)
	later := now.Add(61 * time.Second)
	snap, ok := tracker.Snapshot(1, later)
	if !ok {
		t.Fatal("expected today's total to keep account visible")
	}
	if snap.TokensPerHour != 0 {
		t.Fatalf("tokens/hour after window = %v, want 0", snap.TokensPerHour)
	}
	if snap.TotalTokens != 50 {
		t.Fatalf("total after window = %d, want 50", snap.TotalTokens)
	}
}

func TestActivityTrackerRollsOverAtLocalDay(t *testing.T) {
	loc := time.FixedZone("KST", 9*60*60)
	now := time.Date(2026, 7, 4, 23, 59, 0, 0, loc)
	tracker := NewActivityTracker(loc, now)

	tracker.Ingest(jsonl.TokenEvent{Provider: wire.ProviderClaude, AccountNumber: 1, Timestamp: now, Tokens: 50}, now)
	tomorrow := time.Date(2026, 7, 5, 0, 1, 0, 0, loc)
	if snap, ok := tracker.Snapshot(1, tomorrow); ok {
		t.Fatalf("next local day should clear inactive account, got %+v", snap)
	}
}

func TestActivityTrackerIgnoresUntaggedAndNonClaudeEvents(t *testing.T) {
	now := time.Date(2026, 7, 4, 10, 0, 0, 0, time.UTC)
	tracker := NewActivityTracker(time.UTC, now)

	tracker.Ingest(jsonl.TokenEvent{Provider: wire.ProviderClaude, Timestamp: now, Tokens: 50}, now)
	tracker.Ingest(jsonl.TokenEvent{Provider: wire.ProviderCodex, AccountNumber: 1, Timestamp: now, Tokens: 50}, now)
	if snap, ok := tracker.Snapshot(1, now); ok {
		t.Fatalf("ignored events should not create activity, got %+v", snap)
	}
}
