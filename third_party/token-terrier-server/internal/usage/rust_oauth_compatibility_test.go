package usage

import (
	"encoding/json"
	"github.com/codemoo/token-terrier/server-go/internal/auth"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
	"os"
	"path/filepath"
	"reflect"
	"testing"
	"time"
)

type rustOAuthCase struct {
	Name     string         `json:"name"`
	Provider wire.Provider  `json:"provider"`
	Raw      string         `json:"raw"`
	Want     map[string]any `json:"want"`
}

func TestRustOAuthOracle(t *testing.T) {
	cases := []rustOAuthCase{
		{Name: "claude-both", Provider: wire.ProviderClaude, Raw: `{"five_hour":{"utilization":25,"resets_at":"2026-09-24T05:00:00Z"},"seven_day":{"utilization":60}}`},
		{Name: "claude-weekly-only", Provider: wire.ProviderClaude, Raw: `{"seven_day":{"utilization":0},"extra_rate_windows":[{"private":"synthetic"}]}`},
		{Name: "claude-empty", Provider: wire.ProviderClaude, Raw: `{}`},
		{Name: "claude-missing-pct", Provider: wire.ProviderClaude, Raw: `{"five_hour":{}}`},
		{Name: "claude-string-pct", Provider: wire.ProviderClaude, Raw: `{"five_hour":{"utilization":"25"}}`},
		{Name: "claude-negative", Provider: wire.ProviderClaude, Raw: `{"five_hour":{"utilization":-1}}`},
		{Name: "claude-over", Provider: wire.ProviderClaude, Raw: `{"five_hour":{"utilization":101}}`},
		{Name: "claude-bad-sonnet", Provider: wire.ProviderClaude, Raw: `{"five_hour":{"utilization":10},"seven_day_sonnet":{"utilization":101}}`},
		{Name: "claude-empty-reset", Provider: wire.ProviderClaude, Raw: `{"five_hour":{"utilization":10,"resets_at":""}}`},
		{Name: "claude-trim-reset", Provider: wire.ProviderClaude, Raw: `{"five_hour":{"utilization":100,"resets_at":" 2026-09-24T00:00:00Z "}}`},
		{Name: "claude-extra-type", Provider: wire.ProviderClaude, Raw: `{"five_hour":{"utilization":10},"extra_rate_windows":{}}`},
		{Name: "claude-zero-time", Provider: wire.ProviderClaude, Raw: `{"five_hour":{"utilization":10,"resets_at":"0001-01-01T00:00:00Z"}}`},
		{Name: "codex-nested", Provider: wire.ProviderCodex, Raw: `{"plan_type":"ChatGPT_Pro","rate_limit":{"primary_window":{"used_percent":"12.5","reset_at":1790229600},"secondary_window":{"usedPercent":100}}}`},
		{Name: "codex-primary-wins", Provider: wire.ProviderCodex, Raw: `{"primary":{"usedPercent":0,"used_percent":80},"rate_limit":{"primary_window":{"used_percent":99}},"credits":{"remaining":5},"accountEmail":"synthetic@example.invalid"}`},
		{Name: "codex-weekly-only", Provider: wire.ProviderCodex, Raw: `{"plan_type":"plus","secondary":{"usedPercent":44.25,"resetsAt":"2026-09-25T09:00:00+09:00"}}`},
		{Name: "codex-empty-reset", Provider: wire.ProviderCodex, Raw: `{"primary":{"usedPercent":1,"resetsAt":""}}`},
		{Name: "codex-number-string-epoch", Provider: wire.ProviderCodex, Raw: `{"primary":{"usedPercent":1,"reset_at":"1790229600"}}`},
		{Name: "codex-reset-precedence", Provider: wire.ProviderCodex, Raw: `{"primary":{"usedPercent":1,"reset_at":1790229600,"resetsAt":"2026-09-23T00:00:00Z"}}`},
		{Name: "codex-malformed-flex-fallback", Provider: wire.ProviderCodex, Raw: `{"primary":{"usedPercent":{},"used_percent":35,"windowMinutes":[],"limit_window_seconds":true}}`},
		{Name: "codex-empty", Provider: wire.ProviderCodex, Raw: `{}`},
		{Name: "codex-missing-pct", Provider: wire.ProviderCodex, Raw: `{"primary":{}}`},
		{Name: "codex-negative", Provider: wire.ProviderCodex, Raw: `{"primary":{"used_percent":-1}}`},
		{Name: "codex-nonfinite", Provider: wire.ProviderCodex, Raw: `{"primary":{"used_percent":"NaN"}}`},
		{Name: "codex-overflow", Provider: wire.ProviderCodex, Raw: `{"primary":{"used_percent":"1e999"}}`},
		{Name: "codex-bad-tertiary", Provider: wire.ProviderCodex, Raw: `{"primary":{"used_percent":1},"tertiary":{"used_percent":101}}`},
		{Name: "codex-bad-ignored-alias", Provider: wire.ProviderCodex, Raw: `{"primary":{"usedPercent":0,"used_percent":"NaN"}}`},
		{Name: "codex-bad-credits", Provider: wire.ProviderCodex, Raw: `{"primary":{"usedPercent":0},"credits":{"balance":"NaN"}}`},
		{Name: "codex-bad-unused-window-int", Provider: wire.ProviderCodex, Raw: `{"primary":{"usedPercent":0},"rate_limit":{"primary_window":{"windowMinutes":"NaN"}}}`},
		{Name: "codex-fraction-reset", Provider: wire.ProviderCodex, Raw: `{"primary":{"usedPercent":0,"reset_at":123.4}}`},
		{Name: "codex-negative-reset", Provider: wire.ProviderCodex, Raw: `{"primary":{"usedPercent":0,"reset_at":-1}}`},
		{Name: "codex-zero-reset", Provider: wire.ProviderCodex, Raw: `{"primary":{"usedPercent":0,"reset_at":0}}`},
		{Name: "codex-blank-epoch", Provider: wire.ProviderCodex, Raw: `{"primary":{"usedPercent":0,"reset_at":""}}`},
		{Name: "codex-bad-unselected-reset-type", Provider: wire.ProviderCodex, Raw: `{"primary":{"usedPercent":0},"rate_limit":{"primary_window":{"reset_at":{}}}}`},
		{Name: "codex-trailing", Provider: wire.ProviderCodex, Raw: `{"primary":{"used_percent":0}} {}`},
	}
	now, _ := time.Parse(time.RFC3339, "2026-09-24T00:00:00Z")
	for i := range cases {
		c := &cases[i]
		var s wire.UsageSnapshot
		if c.Provider == wire.ProviderClaude {
			var r claudeUsageResponse
			if err := json.Unmarshal([]byte(c.Raw), &r); err != nil {
				continue
			}
			if err := validateClaudeUsage(&r); err != nil {
				continue
			}
			s = NormalizeClaude(&r, auth.OAuthCredential{}, 9, wire.ProducerInfo{}, now)
		} else {
			var r codexUsageResponse
			if err := json.Unmarshal([]byte(c.Raw), &r); err != nil {
				continue
			}
			if err := validateCodexUsage(&r); err != nil {
				continue
			}
			s = NormalizeCodex(&r, auth.OAuthCredential{}, 9, wire.ProducerInfo{}, now)
		}
		raw, _ := json.Marshal(s)
		var all map[string]any
		_ = json.Unmarshal(raw, &all)
		c.Want = map[string]any{}
		for _, k := range []string{"schema", "seq", "generated_at_utc", "provider", "plan_type", "burn_rate_per_min", "burn_state", "today_total_tokens", "today_sessions", "rolling_5h", "weekly", "rolling_5h_observed", "weekly_observed"} {
			if v, ok := all[k]; ok {
				c.Want[k] = v
			}
		}
		c.Want["status"] = map[string]any{"state": "ok", "data_source": "api_only", "quota_source": "oauth_api", "stale": false, "quota_observed_at": "2026-09-24T00:00:00.000Z"}
	}
	path := filepath.Join("..", "..", "..", "..", "tests", "fixtures", "usage-oauth-v1", "go-oracle.json")
	raw, _ := json.MarshalIndent(cases, "", "  ")
	raw = append(raw, '\n')
	if os.Getenv("HMUX_UPDATE_USAGE_ORACLES") == "1" {
		if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, raw, 0644); err != nil {
			t.Fatal(err)
		}
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var stored []rustOAuthCase
	if err = json.Unmarshal(data, &stored); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(cases, stored) {
		t.Fatal("OAuth oracle changed")
	}
}
