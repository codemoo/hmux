package webgateway

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestRustUsagePreferenceOracle(t *testing.T) {
	login := loginSession{username: "one", profile: "profile-one"}
	store := newUsagePreferenceStore(t.TempDir())
	next := defaultUsagePreferences()
	next.Claude.Enabled = false
	next.Codex.Source = "cli"
	saved, err := store.set(login, next)
	if err != nil {
		t.Fatal(err)
	}
	savedJSON, err := os.ReadFile(filepath.Join(store.dir, usagePreferenceKey(login)+".json"))
	if err != nil {
		t.Fatal(err)
	}
	type decodeCase struct {
		JSON  string `json:"json"`
		Valid bool   `json:"valid"`
	}
	inputs := []string{
		string(savedJSON), `{}`, `[]`, `null`,
		`{"version":1,"claude":{"source":"cswap"},"codex":{"source":"cli"}}`,
		`{"version":1,"revision":null,"claude":{"enabled":null,"source":"cli"},"codex":{"source":"codex-lb"}}`,
		`{"version":1,"revision":9007199254740991,"claude":{"source":"cswap"},"codex":{"source":"cli"}}`,
		`{"version":1,"revision":9007199254740992,"claude":{"source":"cswap"},"codex":{"source":"cli"}}`,
		`{"version":1,"revision":-1,"claude":{"source":"cswap"},"codex":{"source":"cli"}}`,
		`{"version":1,"claude":{"source":"codex-lb"},"codex":{"source":"cli"}}`,
		`{"version":1,"claude":[true,"cswap"],"codex":{"source":"cli"}}`,
		`{"version":1,"unknown":true,"claude":{"source":"cswap"},"codex":{"source":"cli"}}`,
	}
	cases := make([]decodeCase, 0, len(inputs))
	for _, input := range inputs {
		var p usagePreferences
		err := strictPayload(json.RawMessage(input), &p)
		cases = append(cases, decodeCase{input, err == nil && p.valid()})
	}
	raw, err := json.MarshalIndent(map[string]any{"username": login.username, "profile": login.profile, "key": usagePreferenceKey(login), "default": defaultUsagePreferences(), "saved": saved, "saved_json": string(savedJSON), "cases": cases}, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	raw = append(raw, '\n')
	path := filepath.Join("..", "..", "tests", "fixtures", "usage-preferences-v1", "go-oracle.json")
	if os.Getenv("UPDATE_HMUX_RUST_PREFERENCES_FIXTURE") == "1" {
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, raw, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(raw, want) {
		t.Fatal("Go usage preference oracle changed")
	}
}

// Current-state handoff: Rust writes revision 1, Go reads and advances it, then
// Rust reloads the exact current file. No production store is accessed.
func TestRustUsagePreferenceHandoff(t *testing.T) {
	dir := os.Getenv("HMUX_RUST_PREFERENCES_HANDOFF")
	if dir == "" {
		t.Skip("isolated handoff directory supplied by make rust-compat")
	}
	store := newUsagePreferenceStore(dir)
	login := loginSession{username: "one", profile: "profile-one"}
	got, err := store.get(login)
	if err != nil || got.Revision != 1 || got.Claude.Enabled || got.Codex.Source != "cli" {
		t.Fatal("Rust state not readable", got, err)
	}
	got.Codex.Enabled = false
	saved, err := store.set(login, got)
	if err != nil || saved.Revision != 2 {
		t.Fatal("Go handoff write failed", saved, err)
	}
}
