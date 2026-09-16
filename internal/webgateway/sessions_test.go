package webgateway

import (
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"strings"
	"testing"
	"time"
)

func readPersistedSessions(t *testing.T, auth *authStore) persistedSessionFile {
	t.Helper()
	raw, err := os.ReadFile(auth.sessionPath)
	if err != nil {
		t.Fatal(err)
	}
	var value persistedSessionFile
	if err := json.Unmarshal(raw, &value); err != nil {
		t.Fatal(err)
	}
	return value
}

func TestPersistentLoginUsesHashAndSurvivesRestart(t *testing.T) {
	s := testServer(t)
	now := time.Now().UTC()
	token, ok := s.auth.login("owner", testPassword, codeAt(s.auth.credentials, now), now,
		"2001:db8:1:2::", "2001:db8:1:2::9", "Safari on macOS")
	if !ok {
		t.Fatal("login failed")
	}
	stored := readPersistedSessions(t, s.auth)
	if stored.Version != sessionFileVersion || len(stored.Sessions) != 1 {
		t.Fatalf("unexpected stored sessions: %#v", stored)
	}
	record := stored.Sessions[0]
	if record.TokenHash == token || strings.Contains(string(mustJSON(t, stored)), token) {
		t.Fatal("bearer token persisted")
	}
	hash, err := base64.RawURLEncoding.DecodeString(record.TokenHash)
	if err != nil || string(hash) != sessionKey(token) {
		t.Fatal("persisted token hash does not authenticate cookie")
	}
	if record.ID == token || record.IP != "2001:db8:1:2::9" || record.Browser != "Safari on macOS" {
		t.Fatal("public identity or login metadata incorrect")
	}
	info, err := os.Stat(s.auth.sessionPath)
	if err != nil || info.Mode().Perm() != 0600 {
		t.Fatal("session file is not private")
	}
	restarted, err := newAuth(s.auth.path)
	if err != nil {
		t.Fatal(err)
	}
	originalCSRF, _, _ := s.auth.get(token, false)
	restartedCSRF, _, ok := restarted.get(token, false)
	if !ok {
		t.Fatal("persistent login did not survive restart")
	}
	if restartedCSRF != originalCSRF || restartedCSRF == "" {
		t.Fatal("CSRF token changed across restart")
	}
}

func mustJSON(t *testing.T, value any) []byte {
	t.Helper()
	raw, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	return raw
}

func TestCredentialFingerprintIgnoresReplayStepAndInvalidatesSecretChange(t *testing.T) {
	s := testServer(t)
	now := time.Now().UTC()
	token, ok := s.auth.login("owner", testPassword, codeAt(s.auth.credentials, now), now)
	if !ok {
		t.Fatal("login failed")
	}
	credentials, err := LoadCredentials(s.auth.path)
	if err != nil {
		t.Fatal(err)
	}
	credentials.LastStep++
	if err := WriteCredentials(s.auth.path, credentials); err != nil {
		t.Fatal(err)
	}
	restarted, err := newAuth(s.auth.path)
	if err != nil {
		t.Fatal(err)
	}
	if _, _, ok := restarted.get(token, false); !ok {
		t.Fatal("LastStep invalidated persistent login")
	}
	credentials.Hash[0] ^= 0xff
	if err := WriteCredentials(s.auth.path, credentials); err != nil {
		t.Fatal(err)
	}
	restarted, err = newAuth(s.auth.path)
	if err != nil {
		t.Fatal(err)
	}
	if _, _, ok := restarted.get(token, false); ok {
		t.Fatal("password credential change retained login")
	}
	if len(readPersistedSessions(t, restarted).Sessions) != 0 {
		t.Fatal("invalidated login remained on disk")
	}
}

func TestSessionListAndRevokeAreAccountScopedAndPersistent(t *testing.T) {
	s := testServer(t)
	guest := addTestAccount(t, s, "guest-session")
	auth, err := newAuth(s.auth.path)
	if err != nil {
		t.Fatal(err)
	}
	s.auth = auth
	now := time.Now().UTC()
	ownerToken, ok := auth.login("owner", testPassword, codeAt(auth.credentials, now), now)
	if !ok {
		t.Fatal("owner login failed")
	}
	guestToken, ok := auth.login(guest.Username, testPassword, codeAt(guest, now), now)
	if !ok {
		t.Fatal("guest login failed")
	}
	ownerSessions, ok := auth.listSessions(ownerToken)
	if !ok || len(ownerSessions) != 1 || !ownerSessions[0].Current {
		t.Fatalf("owner list leaked or omitted sessions: %#v", ownerSessions)
	}
	guestSessions, ok := auth.listSessions(guestToken)
	if !ok || len(guestSessions) != 1 || !guestSessions[0].Current {
		t.Fatalf("guest list leaked or omitted sessions: %#v", guestSessions)
	}
	if _, err := auth.revoke(ownerToken, guestSessions[0].ID); !errors.Is(err, errSessionNotFound) {
		t.Fatalf("cross-account revoke returned %v", err)
	}
	_, guestDone, _ := auth.get(guestToken, false)
	current, err := auth.revoke(guestToken, guestSessions[0].ID)
	if err != nil || !current {
		t.Fatalf("current revoke failed: current=%v err=%v", current, err)
	}
	select {
	case <-guestDone:
	default:
		t.Fatal("revoked live connections were not cancelled")
	}
	restarted, err := newAuth(auth.path)
	if err != nil {
		t.Fatal(err)
	}
	if _, _, ok := restarted.get(guestToken, false); ok {
		t.Fatal("revoked login resurrected after restart")
	}
	if _, _, ok := restarted.get(ownerToken, false); !ok {
		t.Fatal("revoke affected another account")
	}
}

func TestSessionsAPIAndLogoutStorageFailure(t *testing.T) {
	s := testServer(t)
	now := time.Now().UTC()
	token, ok := s.auth.login("owner", testPassword, codeAt(s.auth.credentials, now), now,
		"198.51.100.8", "198.51.100.8", "Chrome on Android")
	if !ok {
		t.Fatal("login failed")
	}
	csrf, done, _ := s.auth.get(token, false)
	response := request(s, http.MethodGet, "/api/sessions", nil, token, "", "")
	if response.Code != http.StatusOK {
		t.Fatal(response.Code, response.Body.String())
	}
	var list struct {
		Sessions []sessionInfo `json:"sessions"`
	}
	if err := json.Unmarshal(response.Body.Bytes(), &list); err != nil || len(list.Sessions) != 1 {
		t.Fatalf("invalid response: err=%v body=%s", err, response.Body.String())
	}
	item := list.Sessions[0]
	if item.ID == "" || item.Browser != "Chrome on Android" || item.IP != "198.51.100.8" || !item.Current || item.CreatedAt.IsZero() || item.LastSeenAt.IsZero() || item.ExpiresAt.IsZero() {
		t.Fatalf("incomplete session API item: %#v", item)
	}
	if got := request(s, http.MethodPost, "/api/sessions/revoke", map[string]string{"id": item.ID}, token, "bad", s.origin).Code; got != http.StatusForbidden {
		t.Fatal("revoke accepted invalid CSRF", got)
	}
	if err := os.Chmod(s.auth.sessionPath, 0644); err != nil {
		t.Fatal(err)
	}
	response = request(s, http.MethodPost, "/api/logout", struct{}{}, token, csrf, s.origin)
	if response.Code != http.StatusServiceUnavailable || len(response.Result().Cookies()) != 0 {
		t.Fatalf("logout persistence failure was reported as success: %d", response.Code)
	}
	select {
	case <-done:
	default:
		t.Fatal("storage failure did not fail closed")
	}
	if _, err := newAuth(s.auth.path); err == nil {
		t.Fatal("unsafe session file accepted on restart")
	}
}

func TestCurrentSessionAPIRevokePersistsBeforeSuccess(t *testing.T) {
	s := testServer(t)
	token := loginForTest(t, s)
	csrf, done, _ := s.auth.get(token, false)
	sessions, ok := s.auth.listSessions(token)
	if !ok || len(sessions) != 1 {
		t.Fatal("session unavailable")
	}
	response := request(s, http.MethodPost, "/api/sessions/revoke", map[string]string{"id": sessions[0].ID}, token, csrf, s.origin)
	if response.Code != http.StatusOK || !strings.Contains(response.Body.String(), `"ok":true`) || len(response.Result().Cookies()) != 1 {
		t.Fatalf("current revoke response: %d %s", response.Code, response.Body.String())
	}
	select {
	case <-done:
	default:
		t.Fatal("current revoke did not cancel live connections")
	}
	restarted, err := newAuth(s.auth.path)
	if err != nil {
		t.Fatal(err)
	}
	if _, _, ok := restarted.get(token, false); ok {
		t.Fatal("current session revoke resurrected")
	}
}

func TestServerCloseKeepsPersistentSessions(t *testing.T) {
	s := testServer(t)
	token := loginForTest(t, s)
	_, done, _ := s.auth.get(token, false)
	s.Close()
	select {
	case <-done:
	default:
		t.Fatal("close did not cancel active connection")
	}
	restarted, err := newAuth(s.auth.path)
	if err != nil {
		t.Fatal(err)
	}
	if _, _, ok := restarted.get(token, false); !ok {
		t.Fatal("graceful close revoked persistent login")
	}
}

func TestSeenPersistenceIsThrottled(t *testing.T) {
	s := testServer(t)
	token := loginForTest(t, s)
	before := readPersistedSessions(t, s.auth).Sessions[0].LastSeenAt
	if _, _, ok := s.auth.get(token, true); !ok {
		t.Fatal("touch failed")
	}
	if after := readPersistedSessions(t, s.auth).Sessions[0].LastSeenAt; !after.Equal(before) {
		t.Fatal("ordinary touch wrote session file")
	}
	s.auth.mu.Lock()
	s.auth.sessions[sessionKey(token)].persistedSeen = time.Now().Add(-seenPersistInterval - time.Second)
	s.auth.mu.Unlock()
	if _, _, ok := s.auth.get(token, true); !ok {
		t.Fatal("persisted touch failed")
	}
	if after := readPersistedSessions(t, s.auth).Sessions[0].LastSeenAt; !after.After(before) {
		t.Fatal("throttled touch was never persisted")
	}
}

func TestNinthLoginEvictsLeastRecentlySeenSessionPersistently(t *testing.T) {
	s := testServer(t)
	credentials := s.auth.credentials
	start := time.Now().UTC()
	var firstToken, lastToken string
	var firstDone <-chan struct{}
	for i := 0; i < maxSessionsPerAccount+1; i++ {
		at := start.Add(time.Duration(i) * 31 * time.Second)
		token, ok := s.auth.login(credentials.Username, testPassword, codeAt(credentials, at), at)
		if !ok {
			t.Fatalf("login %d failed", i)
		}
		if i == 0 {
			firstToken = token
			_, firstDone, _ = s.auth.get(token, false)
		}
		lastToken = token
	}
	select {
	case <-firstDone:
	default:
		t.Fatal("evicted session connections remain open")
	}
	if _, _, ok := s.auth.get(firstToken, false); ok {
		t.Fatal("least recently seen session was not evicted")
	}
	if sessions, ok := s.auth.listSessions(lastToken); !ok || len(sessions) != maxSessionsPerAccount {
		t.Fatalf("session limit not enforced: ok=%v count=%d", ok, len(sessions))
	}
	restarted, err := newAuth(s.auth.path)
	if err != nil {
		t.Fatal(err)
	}
	if _, _, ok := restarted.get(firstToken, false); ok {
		t.Fatal("evicted session resurrected")
	}
	if _, _, ok := restarted.get(lastToken, false); !ok {
		t.Fatal("newest session was not persisted")
	}
}

func TestExpiredSessionRemovalPersists(t *testing.T) {
	s := testServer(t)
	token := loginForTest(t, s)
	s.auth.mu.Lock()
	s.auth.sessions[sessionKey(token)].expires = time.Now().Add(-time.Second)
	s.auth.mu.Unlock()
	if _, _, ok := s.auth.get(token, false); ok {
		t.Fatal("expired login accepted")
	}
	if len(readPersistedSessions(t, s.auth).Sessions) != 0 {
		t.Fatal("expired login remained persisted")
	}
	restarted, err := newAuth(s.auth.path)
	if err != nil {
		t.Fatal(err)
	}
	if _, _, ok := restarted.get(token, false); ok {
		t.Fatal("expired login resurrected")
	}
}

func TestFreshAttemptCapacityCannotEvictRateLimits(t *testing.T) {
	s := testServer(t)
	now := time.Now()
	s.auth.mu.Lock()
	for i := 0; i < 1024; i++ {
		s.auth.attempts[fmt.Sprintf("source-%d", i)] = []time.Time{now, now, now, now, now}
	}
	s.auth.mu.Unlock()
	if _, ok := s.auth.login("owner", testPassword, codeAt(s.auth.credentials, now), now, "new-source"); ok {
		t.Fatal("untracked new source bypassed capacity")
	}
	s.auth.mu.Lock()
	defer s.auth.mu.Unlock()
	if len(s.auth.attempts) != 1024 {
		t.Fatal("fresh limiter evicted")
	}
	for _, attempts := range s.auth.attempts {
		if len(attempts) != 5 {
			t.Fatal("throttle state changed")
		}
	}
}

func TestActionStopsWhenActivityPersistenceFails(t *testing.T) {
	s := testServer(t)
	token := loginForTest(t, s)
	csrf, _, _ := s.auth.get(token, false)
	s.auth.mu.Lock()
	s.auth.sessions[sessionKey(token)].persistedSeen = time.Now().Add(-seenPersistInterval - time.Minute)
	s.auth.mu.Unlock()
	if err := os.Chmod(s.auth.sessionPath, 0644); err != nil {
		t.Fatal(err)
	}
	response := request(s, http.MethodPost, "/api/action", map[string]string{"operation": "profiles"}, token, csrf, s.origin)
	if response.Code != http.StatusUnauthorized {
		t.Fatalf("action continued after auth failure: %d", response.Code)
	}
}
