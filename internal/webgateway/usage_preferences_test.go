package webgateway

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestUsagePreferencesPersistAndIsolateAccounts(t *testing.T) {
	dir := t.TempDir()
	store := newUsagePreferenceStore(dir)
	a := loginSession{username: "one", profile: "profile-one"}
	b := loginSession{username: "two", profile: "profile-two"}
	next := defaultUsagePreferences()
	next.Claude.Enabled = false
	next.Codex.Source = "cli"
	saved, err := store.set(a, next)
	if err != nil || saved.Revision != 1 {
		t.Fatal(saved, err)
	}
	again := newUsagePreferenceStore(dir)
	got, err := again.get(a)
	if err != nil || got != saved {
		t.Fatal("preferences not retained", got, err)
	}
	other, err := again.get(b)
	if err != nil || other != defaultUsagePreferences() {
		t.Fatal("account settings crossed", other, err)
	}
	if _, err := again.set(a, next); err != errUsagePreferenceConflict {
		t.Fatal("stale device overwrote settings", err)
	}
	info, err := os.Stat(filepath.Join(dir, usagePreferenceKey(a)+".json"))
	if err != nil || info.Mode().Perm() != 0600 {
		t.Fatal("private mode", err)
	}
}
func TestUsagePreferencesRejectInvalidStorageAndFailedSave(t *testing.T) {
	dir := t.TempDir()
	login := loginSession{username: "one"}
	file := filepath.Join(dir, usagePreferenceKey(login)+".json")
	if err := os.WriteFile(file, []byte(`{}`), 0600); err != nil {
		t.Fatal(err)
	}
	store := newUsagePreferenceStore(dir)
	if _, err := store.get(login); err == nil {
		t.Fatal("malformed settings accepted")
	}
	if _, err := store.set(login, defaultUsagePreferences()); err == nil {
		t.Fatal("malformed original overwritten")
	}
	raw, _ := os.ReadFile(file)
	if string(raw) != "{}" {
		t.Fatal("original replaced")
	}
	blocked := filepath.Join(t.TempDir(), "file")
	_ = os.WriteFile(blocked, []byte("fixture"), 0600)
	store = newUsagePreferenceStore(blocked)
	if _, err := store.set(login, defaultUsagePreferences()); err == nil {
		t.Fatal("unwritable setting reported saved")
	}
}
func TestUsagePreferencesHTTPAuthorizationAndValidation(t *testing.T) {
	s := testServer(t)
	token := loginForTest(t, s)
	csrf, _, _ := s.auth.get(token, false)
	next := defaultUsagePreferences()
	if got := request(s, "GET", "/api/account/usage", nil, "", "", "").Code; got != 401 {
		t.Fatal(got)
	}
	if got := request(s, "POST", "/api/account/usage", next, token, "", s.origin).Code; got != 403 {
		t.Fatal(got)
	}
	if got := request(s, "POST", "/api/account/usage", next, token, csrf, "https://other.example").Code; got != 403 {
		t.Fatal(got)
	}
	next.Codex.Source = "cswap"
	if got := request(s, "POST", "/api/account/usage", next, token, csrf, s.origin).Code; got != 400 {
		t.Fatal(got)
	}
	next = defaultUsagePreferences()
	next.Claude.Enabled = false
	saved := request(s, "POST", "/api/account/usage", next, token, csrf, s.origin)
	if saved.Code != 200 {
		t.Fatal(saved.Code)
	}
	if got := request(s, "POST", "/api/account/usage", next, token, csrf, s.origin).Code; got != 409 {
		t.Fatal(got)
	}
	state := request(s, "GET", "/api/state", nil, token, "", "")
	var body struct {
		Preferences usagePreferences `json:"usage_preferences"`
	}
	if json.Unmarshal(state.Body.Bytes(), &body) != nil || body.Preferences.Claude.Enabled || body.Preferences.Revision != 1 {
		t.Fatal("state did not synchronize saved preferences")
	}
}
