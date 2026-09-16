package webgateway

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func addTestAccount(t *testing.T, s *Server, name string) Credentials {
	t.Helper()
	dir := s.auth.path + ".users"
	if err := os.MkdirAll(dir, 0700); err != nil {
		t.Fatal(err)
	}
	c, err := NewCredentials(name, testPassword)
	if err != nil {
		t.Fatal(err)
	}
	if err := WriteCredentials(filepath.Join(dir, name+".json"), c); err != nil {
		t.Fatal(err)
	}
	return c
}
func TestAccountProfilesAndOTPIsolation(t *testing.T) {
	s := testServer(t)
	first := addTestAccount(t, s, "guest-a")
	second := addTestAccount(t, s, "guest-b")
	auth, err := newAuth(s.auth.path)
	if err != nil {
		t.Fatal(err)
	}
	s.auth = auth
	now := time.Now()
	a, ok := auth.login(first.Username, testPassword, codeAt(first, now), now)
	if !ok {
		t.Fatal("first login")
	}
	b, ok := auth.login(second.Username, testPassword, codeAt(second, now), now)
	if !ok {
		t.Fatal("second login")
	}
	name, profileA, _ := auth.identity(a)
	if name != first.Username || profileA == "" {
		t.Fatal("wrong identity")
	}
	_, profileB, _ := auth.identity(b)
	if profileA == profileB {
		t.Fatal("profiles shared")
	}
	if _, ok := auth.login(first.Username, testPassword, codeAt(first, now), now); ok {
		t.Fatal("OTP replay accepted")
	}
	reloaded, err := newAuth(auth.path)
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := reloaded.login(first.Username, testPassword, codeAt(first, now), now); ok {
		t.Fatal("restart OTP replay accepted")
	}
	s.hub.mu.Lock()
	s.hub.home = &peer{}
	s.hub.updated = now
	s.hub.catalog = json.RawMessage(`{"sessions":[{"id":"$1","created_at":42}]}`)
	s.hub.mu.Unlock()
	csrfA, _, _ := auth.get(a, false)
	csrfB, _, _ := auth.get(b, false)
	body := map[string]any{"operation": "workspace", "payload": map[string]any{"change": map[string]any{"operation_id": "open-tab-00000001", "revision": 0, "base": []any{}, "tabs": []any{map[string]any{"id": "$1", "created_at": 42}}}}}
	if res := request(s, "POST", "/api/action", body, a, csrfA, s.origin); res.Code != 200 {
		t.Fatal(res.Code, res.Body.String())
	}
	read := map[string]any{"operation": "workspace", "payload": map[string]any{"change": nil}}
	res := request(s, "POST", "/api/action", read, b, csrfB, s.origin)
	var workspace struct {
		Tabs []any `json:"tabs"`
	}
	if err := json.Unmarshal(res.Body.Bytes(), &workspace); err != nil {
		t.Fatal(err)
	}
	if len(workspace.Tabs) != 0 {
		t.Fatal("other account tabs leaked")
	}
	// Browser cannot choose another account's namespace.
	read["payload"] = map[string]any{"change": nil, "profile": profileA}
	if got := request(s, "POST", "/api/action", read, b, csrfB, s.origin).Code; got != 400 {
		t.Fatal("profile injection accepted", got)
	}
	// Both accounts see the same live tmux catalog.
	for _, token := range []string{a, b} {
		if res := request(s, "GET", "/api/state", nil, token, "", ""); res.Code != 200 {
			t.Fatal("shared catalog unavailable")
		}
	}
	auth.logout(a)
	if _, _, ok := auth.get(b, false); !ok {
		t.Fatal("logout revoked another account")
	}
	s.hub.mu.Lock()
	s.hub.home = nil
	s.hub.mu.Unlock()
}

func TestSameAccountDevicesRemainIndependent(t *testing.T) {
	s := testServer(t)
	c := s.auth.credentials
	now := time.Now()
	first, ok := s.auth.login(c.Username, testPassword, codeAt(c, now), now)
	if !ok {
		t.Fatal("first device login")
	}
	later := now.Add(31 * time.Second)
	second, ok := s.auth.login(c.Username, testPassword, codeAt(c, later), later)
	if !ok {
		t.Fatal("second device login")
	}
	if first == second {
		t.Fatal("devices share token")
	}
	for _, token := range []string{first, second} {
		if _, _, ok := s.auth.get(token, false); !ok {
			t.Fatal("login displaced existing device")
		}
	}
	s.auth.logout(second)
	if _, _, ok := s.auth.get(first, false); !ok {
		t.Fatal("logout displaced other device")
	}
}
