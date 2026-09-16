package claudeswap

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestNativeReaderTracksIdentityAndFreshness(t *testing.T) {
	t.Setenv("CLAUDE_CONFIG_DIR", "")
	home := t.TempDir()
	root := filepath.Join(home, ".claude-swap-backup")
	if err := os.MkdirAll(filepath.Join(root, "cache"), 0700); err != nil {
		t.Fatal(err)
	}
	write := func(path string, v any) {
		t.Helper()
		data, err := json.Marshal(v)
		if err != nil {
			t.Fatal(err)
		}
		if err = os.WriteFile(path, data, 0600); err != nil {
			t.Fatal(err)
		}
	}
	roster := map[string]any{"sequence": []int{1, 2}, "accounts": map[string]any{
		"1": map[string]any{"email": "one@example.test", "organizationUuid": "org"},
		"2": map[string]any{"email": "two@example.test", "organizationUuid": "org"},
	}, "activeAccountNumber": 2} // live identity wins over stale roster flag
	write(filepath.Join(root, "sequence.json"), roster)
	config := func(email string) {
		write(filepath.Join(home, ".claude.json"), map[string]any{"oauthAccount": map[string]any{"emailAddress": email, "organizationUuid": "org"}})
	}
	config("one@example.test")
	reset := time.Now().Add(time.Hour).UTC().Format(time.RFC3339)
	cache := map[string]any{"schemaVersion": 2, "accounts": map[string]any{
		"1": map[string]any{"email": "one@example.test", "organizationUuid": "org", "fetchedAt": float64(time.Now().Unix()), "lastGood": map[string]any{"seven_day": map[string]any{"pct": 70, "resets_at": reset}}},
		"2": map[string]any{"email": "two@example.test", "organizationUuid": "org", "fetchedAt": float64(time.Now().Unix()), "lastGood": map[string]any{"seven_day": map[string]any{"pct": 90, "resets_at": reset}}},
	}}
	write(filepath.Join(root, "cache", "usage.json"), cache)
	reader := NewNativeReader(home, filepath.Join(home, "absent-export"), nil)
	rows, _ := reader.Accounts()
	if len(rows) != 2 || !rows[0].Active || rows[1].Active || rows[0].SevenDay == nil || rows[0].SevenDay.UsedPct != 0.7 {
		t.Fatal("native identity/quota mismatch")
	}
	config("two@example.test")
	reader.lastCheck = time.Time{}
	if reader.ActiveAccountNumber() != 2 {
		t.Fatal("active switch was not observed")
	}
	// Reusing a numbered slot must not reuse the previous owner's cache.
	accounts := cache["accounts"].(map[string]any)
	accounts["2"].(map[string]any)["email"] = "previous@example.test"
	write(filepath.Join(root, "cache", "usage.json"), cache)
	reader.lastCheck = time.Time{}
	rows, _ = reader.Accounts()
	if !rows[1].Active || rows[1].SevenDay != nil {
		t.Fatal("foreign identity quota reused")
	}
	accounts["1"].(map[string]any)["fetchedAt"] = float64(time.Now().Add(-time.Hour).Unix())
	write(filepath.Join(root, "cache", "usage.json"), cache)
	reader.lastCheck = time.Time{}
	rows, _ = reader.Accounts()
	if rows[0].SevenDay != nil {
		t.Fatal("expired cache quota reused")
	}
	config("unknown@example.test")
	reader.lastCheck = time.Time{}
	if reader.ActiveAccountNumber() != 0 {
		t.Fatal("unknown active identity guessed")
	}
	// Every operation is a read: original file contents remain unchanged.
	before, err := os.ReadFile(filepath.Join(root, "cache", "usage.json"))
	if err != nil {
		t.Fatal(err)
	}
	reader.Accounts()
	after, err := os.ReadFile(filepath.Join(root, "cache", "usage.json"))
	if err != nil {
		t.Fatal(err)
	}
	if string(before) != string(after) {
		t.Fatal("native reader wrote usage cache")
	}
}
func TestNativeWindowNeverUsesExpiredReset(t *testing.T) {
	reset := time.Now().Add(-time.Minute).UTC().Format(time.RFC3339)
	if nativeAccountWindow(&nativeWindow{Pct: 80, Reset: &reset}, time.Now()) != nil {
		t.Fatal("past reset quota retained")
	}
	for _, bad := range []string{"bad\nlabel", "bad\u202elabel"} {
		if accountLabel(bad, 1) != "Account 1" {
			t.Fatal("unsafe email label")
		}
	}
}

func TestExportActiveMismatchDoesNotGuess(t *testing.T) {
	data := []byte(`{"schemaVersion":1,"activeAccountNumber":2,"accounts":[{"number":1,"email":"one@example.test","active":true}]}`)
	rows, err := parseAccounts(data)
	if err != nil || len(rows) != 1 || rows[0].Active {
		t.Fatal("mismatched active export accepted")
	}
}

func TestNativeConfigPathMatchesCSwap(t *testing.T) {
	home := t.TempDir()
	if nativeConfigPath(home, "") != filepath.Join(home, ".claude.json") {
		t.Fatal("default config path")
	}
	config := filepath.Join(home, "custom")
	if err := os.MkdirAll(config, 0700); err != nil {
		t.Fatal(err)
	}
	if nativeConfigPath(home, config) != filepath.Join(config, ".claude.json") {
		t.Fatal("custom config path")
	}
	legacy := filepath.Join(config, ".config.json")
	if err := os.WriteFile(legacy, []byte(`{}`), 0600); err != nil {
		t.Fatal(err)
	}
	if nativeConfigPath(home, config) != legacy {
		t.Fatal("legacy config precedence")
	}
	if nativeConfigPath(home, "relative") != "" {
		t.Fatal("relative config accepted")
	}
}
