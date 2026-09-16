package state

import (
	"context"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/jsonl"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

type fakeAccounts struct {
	accts   []wire.AccountUsage
	updated *string
}

func (f fakeAccounts) Accounts() ([]wire.AccountUsage, *string) { return f.accts, f.updated }

func newTestState(provider wire.Provider) *State {
	return New(provider, nil, nil, nil, nil, wire.ProducerInfo{ID: "h", TimeZone: "UTC"}, nil)
}

func TestIngestEventDecoratesClaudeAccounts(t *testing.T) {
	s := newTestState(wire.ProviderClaude)
	up := "2026-07-03T09:00:00.000Z"
	s.SetAccountsProvider(fakeAccounts{
		accts:   []wire.AccountUsage{{Number: 1, Email: "a@b.com", Active: true, Status: "ok"}},
		updated: &up,
	})
	snap := s.IngestEvent(jsonl.TokenEvent{Provider: wire.ProviderClaude, Tokens: 10, Timestamp: time.Now()}, time.Now())
	if len(snap.Accounts) != 1 || snap.Accounts[0].Email != "a@b.com" {
		t.Fatalf("expected decorated accounts, got %+v", snap.Accounts)
	}
	if snap.AccountsUpdated == nil || *snap.AccountsUpdated != up {
		t.Fatalf("expected accounts_updated_at, got %v", snap.AccountsUpdated)
	}
}

func TestNoProviderLeavesAccountsNil(t *testing.T) {
	s := newTestState(wire.ProviderClaude)
	snap := s.IngestEvent(jsonl.TokenEvent{Provider: wire.ProviderClaude, Tokens: 5, Timestamp: time.Now()}, time.Now())
	if snap.Accounts != nil || snap.AccountsUpdated != nil {
		t.Fatalf("no provider → nil accounts, got %+v / %v", snap.Accounts, snap.AccountsUpdated)
	}
}

func TestCodexDecoratedWhenProviderSet(t *testing.T) {
	s := newTestState(wire.ProviderCodex)
	up := "2026-07-03T09:00:00.000Z"
	s.SetAccountsProvider(fakeAccounts{
		accts:   []wire.AccountUsage{{Number: 1, Email: "x@y.com"}},
		updated: &up,
	})
	// Latest is safe without credentials/usage client for this assertion.
	snap := s.Latest(time.Now())
	if len(snap.Accounts) != 1 || snap.Accounts[0].Email != "x@y.com" {
		t.Fatalf("expected codex accounts decorated, got %+v", snap.Accounts)
	}
	if snap.AccountsUpdated == nil || *snap.AccountsUpdated != up {
		t.Fatalf("expected accounts_updated_at, got %v", snap.AccountsUpdated)
	}
}

func TestCodexWithoutProviderStaysNil(t *testing.T) {
	s := newTestState(wire.ProviderCodex)
	snap := s.Latest(time.Now())
	if snap.Accounts != nil {
		t.Fatalf("codex without accounts provider must stay nil, got %+v", snap.Accounts)
	}
}

type localQuotaFixture struct{ snapshot wire.UsageSnapshot }

func (f localQuotaFixture) Snapshot(_ context.Context, seq int, now time.Time) (wire.UsageSnapshot, bool) {
	snapshot := f.snapshot
	snapshot.Seq = seq
	snapshot.GeneratedAtUTC = wire.FormatTime(now)
	return snapshot, true
}

func TestDegradedPoolDoesNotRefreshSuccessfulQuotaTimestamp(t *testing.T) {
	st := newTestState(wire.ProviderCodex)
	now := time.Now()
	st.SetLocalSnapshotter(localQuotaFixture{snapshot: wire.UsageSnapshot{
		Status: wire.SnapshotStatus{State: wire.StateOK, QuotaSource: wire.QuotaSourceCodexLB},
	}})
	_ = st.Refresh(context.Background(), now)
	success := st.Diagnostics().LastQuotaSuccessAt
	st.SetLocalSnapshotter(localQuotaFixture{snapshot: wire.Degraded(wire.ProviderCodex, 1, wire.ProducerInfo{}, now, wire.StateNetworkError)})
	snapshot := st.Refresh(context.Background(), now.Add(time.Minute)).Snapshot
	diagnostics := st.Diagnostics()
	if snapshot.Status.State != wire.StateNetworkError || snapshot.Status.QuotaObservedAt != nil ||
		success == nil || diagnostics.LastQuotaSuccessAt == nil || *success != *diagnostics.LastQuotaSuccessAt || diagnostics.LastQuotaErrorAt == nil {
		t.Fatal("degraded pool was recorded as a fresh quota observation")
	}
}
