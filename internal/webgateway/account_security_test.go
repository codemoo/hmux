package webgateway

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

func TestLegacyCredentialsAndFingerprintRemainCompatible(t *testing.T) {
	c, err := NewCredentials("legacy", testPassword)
	if err != nil {
		t.Fatal(err)
	}
	stable := struct {
		Username   string `json:"username"`
		Salt       []byte `json:"salt"`
		Hash       []byte `json:"hash"`
		TOTPSecret string `json:"totp_secret"`
	}{c.Username, c.Salt, c.Hash, c.TOTPSecret}
	raw, err := json.Marshal(stable)
	if err != nil {
		t.Fatal(err)
	}
	digest := sha256.Sum256(raw)
	legacyFingerprint := base64.RawURLEncoding.EncodeToString(digest[:])
	if got := credentialFingerprint(c); got != legacyFingerprint {
		t.Fatalf("enabled credential fingerprint changed: got %q want %q", got, legacyFingerprint)
	}
	credentialJSON, err := json.Marshal(c)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(credentialJSON), "totp_disabled") {
		t.Fatal("default credential emitted the compatibility field")
	}
}

func TestLoginChallengeDoesNotConsumeOTPOrMintSession(t *testing.T) {
	s := testServer(t)
	now := time.Now().UTC()
	before := s.auth.credentials.LastStep
	response := request(s, http.MethodPost, "/api/login", map[string]string{
		"username": "owner", "password": testPassword, "code": "",
	}, "", "", s.origin)
	if response.Code != http.StatusOK || !strings.Contains(response.Body.String(), `"totp_required":true`) {
		t.Fatalf("missing-code challenge: %d %s", response.Code, response.Body.String())
	}
	if len(response.Result().Cookies()) != 0 || len(s.auth.sessions) != 0 || s.auth.credentials.LastStep != before {
		t.Fatal("challenge minted a login or consumed an OTP counter")
	}
	for _, body := range []map[string]string{
		{"username": "owner", "password": "wrong-password", "code": ""},
		{"username": "missing", "password": testPassword, "code": ""},
	} {
		response = request(s, http.MethodPost, "/api/login", body, "", "", s.origin)
		if response.Code != http.StatusUnauthorized || strings.Contains(response.Body.String(), "totp_required") {
			t.Fatalf("invalid credentials disclosed challenge: %d %s", response.Code, response.Body.String())
		}
	}
	response = request(s, http.MethodPost, "/api/login", map[string]string{
		"username": "owner", "password": testPassword, "code": codeAt(s.auth.credentials, now),
	}, "", "", s.origin)
	if response.Code != http.StatusOK || len(response.Result().Cookies()) != 1 || len(s.auth.sessions) != 1 {
		t.Fatalf("challenged OTP was not usable: %d %s", response.Code, response.Body.String())
	}
}

func TestAccountSecurityAPIValidationAndSameValue(t *testing.T) {
	s := testServer(t)
	token := loginForTest(t, s)
	csrf, _, _ := s.auth.get(token, false)
	if response := request(s, http.MethodGet, "/api/account/security", nil, token, "", ""); response.Code != http.StatusOK || response.Body.String() != "{\"totp_enabled\":true}\n" {
		t.Fatalf("security GET: %d %s", response.Code, response.Body.String())
	}
	now := time.Now().Add(30 * time.Second)
	valid := map[string]any{"totp_enabled": true, "password": testPassword, "code": codeAt(s.auth.credentials, now)}
	if got := request(s, http.MethodPost, "/api/account/security", valid, token, "bad", s.origin).Code; got != http.StatusForbidden {
		t.Fatal("invalid CSRF accepted", got)
	}
	if got := request(s, http.MethodPost, "/api/account/security", valid, token, csrf, "https://evil.example").Code; got != http.StatusForbidden {
		t.Fatal("invalid Origin accepted", got)
	}
	if got := request(s, http.MethodPost, "/api/account/security", map[string]string{"password": testPassword, "code": "123456"}, token, csrf, s.origin).Code; got != http.StatusBadRequest {
		t.Fatal("missing boolean accepted", got)
	}
	injected := map[string]any{"totp_enabled": true, "password": testPassword, "code": "123456", "username": "owner"}
	if got := request(s, http.MethodPost, "/api/account/security", injected, token, csrf, s.origin).Code; got != http.StatusBadRequest {
		t.Fatal("account injection accepted", got)
	}
	beforeStep := s.auth.credentials.LastStep
	beforeSessions := len(s.auth.sessions)
	response := request(s, http.MethodPost, "/api/account/security", valid, token, csrf, s.origin)
	if response.Code != http.StatusOK || s.auth.credentials.LastStep != beforeStep || len(s.auth.sessions) != beforeSessions {
		t.Fatalf("same-value request mutated state: %d %s", response.Code, response.Body.String())
	}
	if _, err := os.Stat(s.auth.path + ".backups"); !os.IsNotExist(err) {
		t.Fatal("same-value request created a backup")
	}
}

func TestAccountSecurityTogglePersistsAndScopesSessions(t *testing.T) {
	s := testServer(t)
	guest := addTestAccount(t, s, "guest-toggle")
	auth, err := newAuth(s.auth.path)
	if err != nil {
		t.Fatal(err)
	}
	s.auth = auth
	start := time.Now().UTC()
	current, ok := auth.login("owner", testPassword, codeAt(auth.credentials, start), start)
	if !ok {
		t.Fatal("current owner login failed")
	}
	otherAt := start.Add(31 * time.Second)
	other, ok := auth.login("owner", testPassword, codeAt(auth.credentials, otherAt), otherAt)
	if !ok {
		t.Fatal("other owner login failed")
	}
	guestToken, ok := auth.login(guest.Username, testPassword, codeAt(guest, start), start)
	if !ok {
		t.Fatal("guest login failed")
	}
	_, otherDone, _ := auth.get(other, false)
	toggleAt := start.Add(62 * time.Second)
	if status := auth.setTOTPEnabled(current, testPassword, codeAt(auth.credentials, toggleAt), false, toggleAt); status != accountSecurityOK {
		t.Fatal("disable failed", status)
	}
	select {
	case <-otherDone:
	default:
		t.Fatal("other same-account login remained connected")
	}
	if _, _, ok := auth.get(current, false); !ok {
		t.Fatal("current login was revoked")
	}
	if _, _, ok := auth.get(guestToken, false); !ok {
		t.Fatal("other account was revoked")
	}
	if _, _, ok := auth.get(other, false); ok {
		t.Fatal("other same-account login survived")
	}
	entries, err := os.ReadDir(auth.path + ".users")
	if err != nil || len(entries) != 1 || entries[0].Name() != "guest-toggle.json" {
		t.Fatalf("backup polluted account enumeration: entries=%v err=%v", entries, err)
	}
	backups, err := os.ReadDir(auth.path + ".backups")
	if err != nil || len(backups) != 1 || strings.Contains(backups[0].Name(), "owner") {
		t.Fatalf("invalid backup naming: entries=%v err=%v", backups, err)
	}
	backupInfo, err := backups[0].Info()
	if err != nil || backupInfo.Mode().Perm() != 0600 {
		t.Fatal("backup is not private")
	}
	reloaded, err := newAuth(auth.path)
	if err != nil {
		t.Fatal(err)
	}
	if !reloaded.credentials.TOTPDisabled {
		t.Fatal("primary toggle did not survive reload")
	}
	passwordOnlyAt := toggleAt.Add(31 * time.Second)
	if token, status := reloaded.loginWithChallenge("owner", testPassword, "", passwordOnlyAt); status != loginSucceeded || token == "" {
		t.Fatal("disabled account rejected password-only login", status)
	}
	passwordToken, status := reloaded.loginWithChallenge("owner", testPassword, "", passwordOnlyAt.Add(time.Second))
	if status != loginSucceeded {
		t.Fatal("second password-only login failed", status)
	}
	enableAt := passwordOnlyAt.Add(31 * time.Second)
	if status := reloaded.setTOTPEnabled(passwordToken, testPassword, codeAt(reloaded.credentials, enableAt), true, enableAt); status != accountSecurityOK {
		t.Fatal("primary account enable failed", status)
	}
	enabledAuth, err := newAuth(auth.path)
	if err != nil {
		t.Fatal(err)
	}
	if enabledAuth.credentials.TOTPDisabled {
		t.Fatal("primary enable did not survive reload")
	}
	if token, status := enabledAuth.loginWithChallenge("owner", testPassword, "", enableAt.Add(31*time.Second)); status != loginTOTPRequired || token != "" {
		t.Fatal("re-enabled primary account did not require TOTP", status)
	}
	if token, status := reloaded.loginWithChallenge(guest.Username, testPassword, "", passwordOnlyAt); status != loginTOTPRequired || token != "" {
		t.Fatal("primary toggle affected additional account", status)
	}
	guestCurrent, ok := enabledAuth.login(guest.Username, testPassword, codeAt(guest, passwordOnlyAt), passwordOnlyAt)
	if !ok {
		t.Fatal("guest fresh login failed")
	}
	guestToggleAt := passwordOnlyAt.Add(31 * time.Second)
	guestCredentials := enabledAuth.accounts[guest.Username].credentials
	if status := enabledAuth.setTOTPEnabled(guestCurrent, testPassword, codeAt(guestCredentials, guestToggleAt), false, guestToggleAt); status != accountSecurityOK {
		t.Fatal("additional account disable failed", status)
	}
	finalAuth, err := newAuth(auth.path)
	if err != nil {
		t.Fatal(err)
	}
	if finalAuth.credentials.TOTPDisabled || !finalAuth.accounts[guest.Username].credentials.TOTPDisabled {
		t.Fatal("account toggles did not survive reload independently")
	}
}

func TestAccountSecurityReauthenticationFailureAndRateLimit(t *testing.T) {
	s := testServer(t)
	token := loginForTest(t, s)
	csrf, _, _ := s.auth.get(token, false)
	now := time.Now().Add(30 * time.Second)
	before := s.auth.credentials
	for _, body := range []map[string]any{
		{"totp_enabled": false, "password": "wrong-password", "code": codeAt(before, now)},
		{"totp_enabled": false, "password": testPassword, "code": "bad"},
	} {
		response := request(s, http.MethodPost, "/api/account/security", body, token, csrf, s.origin)
		if response.Code != http.StatusForbidden {
			t.Fatalf("invalid reauthentication status: %d %s", response.Code, response.Body.String())
		}
		if s.auth.credentials.TOTPDisabled || s.auth.credentials.LastStep != before.LastStep {
			t.Fatal("invalid reauthentication mutated credentials")
		}
	}
	s.auth.mu.Lock()
	session := s.auth.sessions[sessionKey(token)]
	accountKey := session.username + "\x00" + session.profile
	s.auth.securityAttempts[accountKey] = []time.Time{now, now, now, now, now}
	s.auth.mu.Unlock()
	response := request(s, http.MethodPost, "/api/account/security", map[string]any{
		"totp_enabled": false, "password": testPassword, "code": codeAt(before, now),
	}, token, csrf, s.origin)
	if response.Code != http.StatusTooManyRequests {
		t.Fatalf("rate limit status: %d %s", response.Code, response.Body.String())
	}
}

func TestAccountSecurityStorageFailureFailsClosed(t *testing.T) {
	s := testServer(t)
	token := loginForTest(t, s)
	csrf, done, _ := s.auth.get(token, false)
	if err := os.Chmod(s.auth.sessionPath, 0644); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.Chmod(s.auth.sessionPath, 0600) })
	now := time.Now().Add(30 * time.Second)
	response := request(s, http.MethodPost, "/api/account/security", map[string]any{
		"totp_enabled": false, "password": testPassword, "code": codeAt(s.auth.credentials, now),
	}, token, csrf, s.origin)
	if response.Code != http.StatusServiceUnavailable {
		t.Fatalf("storage failure status: %d %s", response.Code, response.Body.String())
	}
	select {
	case <-done:
	default:
		t.Fatal("storage failure did not close authenticated connections")
	}
	if _, _, ok := s.auth.get(token, false); ok {
		t.Fatal("storage failure retained authenticated access")
	}
}

func TestInFlightLoginRechecksToggleAndToggleRechecksRevocation(t *testing.T) {
	s := testServer(t)
	current := loginForTest(t, s)
	baseHash := s.auth.hashPassword
	started := make(chan struct{}, 1)
	release := make(chan struct{})
	var callsMu sync.Mutex
	calls := 0
	s.auth.hashPassword = func(password string, salt []byte) ([]byte, error) {
		callsMu.Lock()
		calls++
		call := calls
		callsMu.Unlock()
		if call == 1 {
			started <- struct{}{}
			<-release
		}
		return baseHash(password, salt)
	}
	loginAt := time.Now().Add(31 * time.Second)
	loginResult := make(chan loginStatus, 1)
	go func() {
		_, status := s.auth.loginWithChallenge("owner", testPassword, "", loginAt, "concurrent-login")
		loginResult <- status
	}()
	<-started
	toggleAt := loginAt.Add(31 * time.Second)
	if status := s.auth.setTOTPEnabled(current, testPassword, codeAt(s.auth.credentials, toggleAt), false, toggleAt); status != accountSecurityOK {
		t.Fatal("concurrent disable failed", status)
	}
	close(release)
	if status := <-loginResult; status != loginInvalid {
		t.Fatal("in-flight login used stale enabled config", status)
	}

	// Give the account a fresh current login, then revoke it while its enable
	// request is hashing. The completed hash cannot resurrect the stale cookie.
	newCurrent, status := s.auth.loginWithChallenge("owner", testPassword, "", toggleAt.Add(time.Second), "new-current")
	if status != loginSucceeded {
		t.Fatal("password-only login failed", status)
	}
	s.auth.hashPassword = func(password string, salt []byte) ([]byte, error) {
		started <- struct{}{}
		<-release
		return baseHash(password, salt)
	}
	started = make(chan struct{}, 1)
	release = make(chan struct{})
	enableAt := toggleAt.Add(31 * time.Second)
	toggleResult := make(chan accountSecurityStatus, 1)
	go func() {
		toggleResult <- s.auth.setTOTPEnabled(newCurrent, testPassword, codeAt(s.auth.credentials, enableAt), true, enableAt)
	}()
	<-started
	sessions, ok := s.auth.listSessions(newCurrent)
	if !ok || len(sessions) < 1 {
		t.Fatal("current session missing before revoke")
	}
	var currentID string
	for _, session := range sessions {
		if session.Current {
			currentID = session.ID
		}
	}
	if currentID == "" {
		t.Fatal("current session marker missing")
	}
	if _, err := s.auth.revoke(newCurrent, currentID); err != nil {
		t.Fatal(err)
	}
	close(release)
	if status := <-toggleResult; status != accountSecurityStaleSession {
		t.Fatal("in-flight toggle survived current-session revoke", status)
	}
	if !s.auth.credentials.TOTPDisabled {
		t.Fatal("revoked in-flight request changed security state")
	}
	if backups, err := os.ReadDir(filepath.Clean(s.auth.path + ".backups")); err != nil || len(backups) != 1 {
		t.Fatalf("stale request created a backup: count=%d err=%v", len(backups), err)
	}
}
