package codexaccounts

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// Synthetic Go oracle for normalization before transport's Codex privacy rule.
func TestHMuxSyntheticOracle(t *testing.T) {
	now := time.Date(2026, 7, 3, 13, 1, 0, 0, time.UTC)
	inputs := map[string]string{
		"normal":        `{"schemaVersion":1,"accountsUpdatedAt":"2026-07-03T13:00:00Z","accounts":[{"number":1,"accountId":"synthetic-id","email":"private@example.test","alias":" Work ","displayName":"Ignore me","status":" active ","plan_type":"chatgpt_pro","fiveHourPct":1.5,"sevenDayPct":-0.5,"resetAtPrimary":"2026-07-03T15:00:00Z","totalTokens":123,"tokensPerHour":45.6,"lastRefreshAt":"2026-07-03T12:55:00Z"},{"number":2,"email":"hidden@example.test","displayName":"Ignore","status":"token-expired","planType":"unknown","sevenDayPct":0.92}]}`,
		"missing_time":  `{"schemaVersion":1,"accounts":[{"number":4,"alias":"","email":"hidden@example.test","status":"rateLimited","fiveHourPct":0.25}]}`,
		"zero":          `{"schemaVersion":1,"accounts":[]}`,
		"duplicate":     `{"schemaVersion":1,"accounts":[{"number":1},{"number":1}]}`,
		"missing_array": `{"schemaVersion":1}`,
	}
	type fixture struct {
		Input    string `json:"input"`
		Valid    bool   `json:"valid"`
		Expected any    `json:"expected"`
	}
	out := map[string]fixture{}
	for name, input := range inputs {
		accounts, updatedRaw, err := parseAccounts([]byte(input))
		var expected any
		if err == nil {
			source := now
			if normalized := normalizeTimestampString(updatedRaw); normalized != nil {
				parsed, parseErr := time.Parse("2006-01-02T15:04:05.000Z", *normalized)
				if parseErr != nil {
					t.Fatal(parseErr)
				}
				source = boundedSourceTime(parsed, now)
			}
			rows := make([]any, 0, len(accounts))
			for _, a := range accounts {
				rows = append(rows, map[string]any{
					"number": a.Number, "email": "", "display_name": a.DisplayName,
					"active": a.Active, "status": a.Status, "five_hour": a.FiveHour,
					"seven_day": a.SevenDay, "tokens_per_hour": a.TokensPerHour,
					"total_tokens": a.TotalTokens, "last_refresh_at": a.LastRefreshAt,
					"plan_type": a.PlanType,
				})
			}
			var updated any
			if len(accounts) > 0 {
				updated = source.Format("2006-01-02T15:04:05.000Z")
			}
			expected = map[string]any{"accounts": rows, "updated_at": updated}
		}
		out[name] = fixture{input, err == nil, expected}
	}
	path := filepath.Join("..", "..", "..", "..", "tests", "fixtures", "usage-codex-v1", "accounts.json")
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
