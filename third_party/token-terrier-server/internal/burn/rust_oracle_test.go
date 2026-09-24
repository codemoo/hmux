package burn

import (
	"bytes"
	"encoding/json"
	"github.com/codemoo/token-terrier/server-go/internal/jsonl"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestRustBurnOracle(t *testing.T) {
	type step struct {
		Op      string `json:"op"`
		Now     string `json:"now"`
		At      string `json:"at,omitempty"`
		Tokens  int    `json:"tokens,omitempty"`
		Session string `json:"session,omitempty"`
		Source  string `json:"source,omitempty"`
	}
	type result struct {
		Rate     float64  `json:"rate"`
		State    string   `json:"state"`
		Total    int      `json:"total"`
		Sessions int      `json:"sessions"`
		Observed bool     `json:"observed"`
		Sources  []string `json:"sources"`
	}
	steps := []step{
		{Op: "snapshot", Now: "2026-09-23T23:59:50Z"},
		{Op: "ingest", Now: "2026-09-23T23:59:51Z", At: "2026-09-23T23:59:51Z", Tokens: 600, Session: "a", Source: "jsonl"},
		{Op: "ingest", Now: "2026-09-23T23:59:52Z", At: "2026-09-23T23:59:12Z", Tokens: 200, Session: "b", Source: "hermes"},
		{Op: "snapshot", Now: "2026-09-23T23:59:55Z"},
		{Op: "ingest", Now: "2026-09-24T00:00:01Z", At: "2026-09-23T23:59:52Z", Tokens: 100, Session: "old"},
		{Op: "ingest", Now: "2026-09-24T00:00:02Z", At: "2026-09-24T00:00:02Z", Tokens: 3100, Session: "c", Source: "jsonl"},
		{Op: "snapshot", Now: "2026-09-24T00:01:05Z"},
		{Op: "snapshot", Now: "2026-09-24T00:02:00Z"},
	}
	parse := func(s string) time.Time {
		v, e := time.Parse(time.RFC3339, s)
		if e != nil {
			t.Fatal(e)
		}
		return v
	}
	tracker := New(time.UTC, parse(steps[0].Now))
	outputs := make([]result, 0, len(steps))
	for _, s := range steps {
		now := parse(s.Now)
		var snap Snapshot
		if s.Op == "ingest" {
			snap = tracker.Ingest(jsonl.TokenEvent{Provider: wire.ProviderClaude, Timestamp: parse(s.At), Tokens: s.Tokens, SessionKey: s.Session, Source: s.Source}, now)
		} else {
			snap = tracker.Snapshot(now)
		}
		outputs = append(outputs, result{snap.RatePerMinute, string(snap.State), snap.TodayTotalTokens, snap.TodaySessionsCount, snap.HasObserved, snap.ActivitySources})
	}
	root := filepath.Join("..", "..", "..", "..", "tests", "fixtures", "usage-activity-v1")
	verifyBurnJSON(t, filepath.Join(root, "burn-steps.json"), steps)
	verifyBurnJSON(t, filepath.Join(root, "burn-go-oracle.json"), outputs)
}
func verifyBurnJSON(t *testing.T, path string, value any) {
	t.Helper()
	data, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	data = append(data, '\n')
	if os.Getenv("HMUX_UPDATE_USAGE_ORACLES") == "1" {
		if err := os.WriteFile(path, data, 0644); err != nil {
			t.Fatal(err)
		}
	}
	stored, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(data, stored) {
		t.Fatalf("oracle changed: %s", path)
	}
}
