package codexlb

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

// Synthetic Go oracle for the Rust embedded parser. No HTTP or credentials.
func TestHMuxSyntheticOracle(t *testing.T) {
	now := time.Date(2026, 7, 3, 13, 1, 0, 0, time.UTC)
	inputs := map[string]string{
		"per_key":       `{"upstream_limits":[{"source":"aggregate","limit_type":"credits","limit_window":"5hours","max_value":100,"current_value":25,"remaining_value":75,"reset_at":"2026-07-03T15:00:00Z"},{"source":"aggregate","limit_type":"credits","limit_window":"weekly","max_value":200,"current_value":100,"remaining_value":100,"reset_at":"1783400000"},{"source":"per_key","limit_type":"credits","limit_window":"5h","max_value":100,"current_value":100,"remaining_value":0}]}`,
		"pool_partial":  `{"upstream_limits":[{"source":"aggregate","limit_type":"credits","limit_window":"5h","max_value":100,"current_value":100,"remaining_value":0,"reset_at":"2026-07-03T15:00:00Z"},{"source":"aggregate","limit_type":"credits","limit_window":"7d","max_value":100,"current_value":50,"remaining_value":50}],"account_pool_usage":{"primary":93}}`,
		"pool_empty":    `{"upstream_limits":[{"source":"aggregate","limit_type":"credits","limit_window":"5h","max_value":100,"current_value":20,"remaining_value":80}],"account_pool_usage":{}}`,
		"invalid_reset": `{"upstream_limits":[{"source":"aggregate","limit_type":"credits","limit_window":"5h","max_value":100,"current_value":20,"remaining_value":80,"reset_at":"secret-non-time"}]}`,
		"pool_weekly":   `{"account_pool_usage":{"primary":0,"secondary":120}}`,
	}
	type fixture struct {
		Input    string `json:"input"`
		Expected any    `json:"expected"`
		Valid    bool   `json:"valid"`
	}
	out := map[string]fixture{}
	for name, input := range inputs {
		var response usageResponse
		if err := json.Unmarshal([]byte(input), &response); err != nil {
			t.Fatal(err)
		}
		snap, ok := buildSnapshot(response, 7, wire.ProducerInfo{}, now)
		var expected any
		if ok {
			expected = map[string]any{
				"schema": snap.Schema, "seq": snap.Seq, "generated_at_utc": snap.GeneratedAtUTC,
				"provider": snap.Provider, "burn_state": snap.BurnState,
				"rolling_5h": snap.Rolling5h, "weekly": snap.Weekly,
				"rolling_5h_observed": snap.Rolling5hObserved, "weekly_observed": snap.WeeklyObserved,
				"status": map[string]any{"state": snap.Status.State, "data_source": snap.Status.DataSource, "quota_source": snap.Status.QuotaSource, "stale": snap.Status.Stale},
			}
		}
		out[name] = fixture{input, expected, ok}
	}
	path := filepath.Join("..", "..", "..", "..", "tests", "fixtures", "usage-codex-v1", "lb.json")
	data, err := json.MarshalIndent(out, "", "  ")
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
		t.Fatal("Codex usage Go oracle changed")
	}
}
