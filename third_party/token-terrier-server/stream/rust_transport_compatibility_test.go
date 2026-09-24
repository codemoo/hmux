package stream

import (
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

type rustTransportCase struct {
	Name  string `json:"name"`
	Raw   string `json:"raw"`
	Valid bool   `json:"valid"`
}

func TestRustTransportOracle(t *testing.T) {
	base := `{"schema":1,"seq":7,"generated_at_utc":"2026-09-24T00:00:00.000Z","provider":"codex","status":{"state":"ok","quota_source":"oauth_api"}}`
	cases := []rustTransportCase{}
	add := func(name, raw string) {
		_, err := decodeTransportSnapshot([]byte(raw))
		cases = append(cases, rustTransportCase{name, raw, err == nil})
	}
	mutate := func(name, key string, value any) {
		var m map[string]any
		_ = json.Unmarshal([]byte(base), &m)
		m[key] = value
		raw, _ := json.Marshal(m)
		add(name, string(raw))
	}
	add("minimal", base)
	add("trailing", base+` {}`)
	mutate("schema", "schema", 2)
	mutate("negative-seq", "seq", -1)
	mutate("unknown", "producer_id", "synthetic-private")
	mutate("plan-pro", "plan_type", "pro")
	mutate("plan-unormalized", "plan_type", "ChatGPT_Pro")
	mutate("burn-negative", "burn_rate_per_min", -1)
	mutate("total-negative", "today_total_tokens", -1)
	mutate("missing-time", "generated_at_utc", "")
	mutate("long-time", "generated_at_utc", strings.Repeat("x", 129))
	mutate("used-ratio", "rolling_5h", map[string]any{"used_pct": 0.4, "remaining_seconds": 5, "resets_at": nil})
	mutate("used-percent", "rolling_5h", map[string]any{"used_pct": 40})
	mutate("negative-time", "weekly", map[string]any{"remaining_seconds": -1})
	mutate("window-extra", "weekly", map[string]any{"extra": "private"})
	for _, c := range []struct {
		name, state, time string
		stale             bool
	}{
		{"retry-ok", "ok", "2026-09-24T01:00:00Z", true}, {"retry-limited", "rateLimited", "2026-09-25T00:00:00Z", false},
		{"retry-fresh", "ok", "2026-09-24T01:00:00Z", false}, {"retry-past", "rateLimited", "2026-09-23T23:59:59Z", false},
		{"retry-long", "rateLimited", "2026-09-25T00:00:01Z", false}, {"retry-invalid", "rateLimited", "x", false},
	} {
		mutate(c.name, "status", map[string]any{"state": c.state, "stale": c.stale, "retry_at": c.time})
	}
	account := func(n int) map[string]any {
		return map[string]any{"number": n, "email": "", "display_name": "Alias", "status": "ok", "five_hour": nil, "seven_day": nil}
	}
	mutate("accounts-one", "accounts", []any{account(1)})
	mutate("accounts-duplicate", "accounts", []any{account(1), account(1)})
	mutate("accounts-zero", "accounts", []any{account(0)})
	a := account(1)
	a["email"] = "synthetic@example.invalid"
	mutate("codex-email", "accounts", []any{a})
	var claude map[string]any
	_ = json.Unmarshal([]byte(base), &claude)
	claude["provider"] = "claude"
	claude["accounts"] = []any{a}
	raw, _ := json.Marshal(claude)
	add("claude-email", string(raw))
	a = account(1)
	a["display_name"] = "bad\u202ename"
	mutate("bidi-alias", "accounts", []any{a})
	a = account(1)
	a["total_tokens"] = -1
	mutate("account-negative", "accounts", []any{a})
	a = account(1)
	a["credential"] = "synthetic-private"
	mutate("account-extra", "accounts", []any{a})
	rows := []any{}
	for i := 1; i <= 129; i++ {
		rows = append(rows, account(i))
		if i == 128 {
			mutate("accounts-limit", "accounts", rows)
		}
	}
	mutate("accounts-over", "accounts", rows)
	var child map[string]any
	_ = json.Unmarshal([]byte(base), &child)
	mutate("source-cli", "sources", map[string]any{"cli": child})
	mutate("source-wrong-name", "sources", map[string]any{"cswap": child})
	child["status"] = map[string]any{"state": "ok", "quota_source": "codex_lb"}
	mutate("source-lb", "sources", map[string]any{"codex-lb": child})
	mutate("source-provenance", "sources", map[string]any{"cli": child})
	child["provider"] = "claude"
	mutate("source-provider", "sources", map[string]any{"codex-lb": child})
	child["provider"] = "codex"
	child["sources"] = map[string]any{"cli": json.RawMessage(base)}
	mutate("source-recursive", "sources", map[string]any{"codex-lb": child})
	path := filepath.Join("..", "..", "..", "tests", "fixtures", "usage-transport-v1", "go-oracle.json")
	encoded, _ := json.MarshalIndent(cases, "", "  ")
	encoded = append(encoded, '\n')
	if os.Getenv("HMUX_UPDATE_USAGE_ORACLES") == "1" {
		if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, encoded, 0644); err != nil {
			t.Fatal(err)
		}
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var stored []rustTransportCase
	if err = json.Unmarshal(data, &stored); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(cases, stored) {
		t.Fatal("usage transport Go oracle changed")
	}
}
