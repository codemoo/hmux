package stream

import (
	"bytes"
	"encoding/json"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestRustSourcesOracle(t *testing.T) {
	now, _ := time.Parse(time.RFC3339, "2026-09-24T00:00:00Z")
	cli := wire.Degraded(wire.ProviderClaude, 2, wire.ProducerInfo{}, now, wire.StateAuthExpired)
	cli.TodayTotalTokens = 500
	cli.Status.DataSource = wire.DataSourceJSONLOnly
	observed := "2026-09-23T23:50:00Z"
	reset := "2026-09-25T00:00:00Z"
	cases := []transportSnapshot{}
	for _, status := range []string{"ok", "stale", "keychain_unavailable", "token_expired"} {
		accounts := []wire.AccountUsage{{Number: 1, Email: "test@example.invalid", Active: true, Status: status, LastRefreshAt: &observed, SevenDay: &wire.AccountWindow{UsedPct: 0.4, ResetsAt: &reset}}}
		secondary := claudeSwapSourceSnapshot(cli, accounts, &observed, now)
		cases = append(cases, newTransportSnapshotWithSources(cli, map[string]wire.UsageSnapshot{"cli": cli, "cswap": secondary}))
	}
	raw, err := json.MarshalIndent(cases, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	raw = append(raw, '\n')
	path := filepath.Join("..", "..", "..", "tests", "fixtures", "usage-transport-v1", "sources-go-oracle.json")
	if os.Getenv("HMUX_UPDATE_USAGE_ORACLES") == "1" {
		if err := os.WriteFile(path, raw, 0644); err != nil {
			t.Fatal(err)
		}
	}
	stored, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(stored, raw) {
		t.Fatal("usage sources Go oracle changed")
	}
}
