package webgateway

import (
	"bytes"
	"crypto/sha256"
	"encoding/base32"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// This fixture is generated only from synthetic inputs through the Go v1 implementation.
// Set HMUX_UPDATE_RUST_AUTH_FIXTURE=1 to refresh it after an intentional contract change.
func TestRustAuthCompatibilityFixture(t *testing.T) {
	salt := make([]byte, 32)
	for i := range salt {
		salt[i] = byte(i)
	}
	secret := make([]byte, 20)
	for i := range secret {
		secret[i] = byte(20 - i)
	}
	hash, err := derivePassword("synthetic-password-123", salt)
	if err != nil {
		t.Fatal(err)
	}
	c := Credentials{Username: "user<&\u2028name", Salt: salt, Hash: hash,
		TOTPSecret: base32.StdEncoding.WithPadding(base32.NoPadding).EncodeToString(secret), LastStep: 123}
	tokenBytes := make([]byte, 32)
	for i := range tokenBytes {
		tokenBytes[i] = byte(31 - i)
	}
	token := base64.RawURLEncoding.EncodeToString(tokenBytes)
	at := time.Date(2024, 2, 3, 4, 5, 6, 123456789, time.UTC)
	session := persistedSession{TokenHash: base64.RawURLEncoding.EncodeToString([]byte(sessionKey(token))),
		ID: token, Username: c.Username, Profile: "", CredentialFingerprint: credentialFingerprint(c),
		Browser: "Chrome on macOS", IP: "unknown", CreatedAt: at, LastSeenAt: at.Add(time.Minute), ExpiresAt: at.Add(loginLifetime)}
	type credentialCase struct {
		Credentials      Credentials `json:"credentials"`
		CredentialJSON   string      `json:"credential_json"`
		FingerprintInput string      `json:"fingerprint_input"`
		Fingerprint      string      `json:"fingerprint"`
	}
	caseFor := func(v Credentials) credentialCase {
		credentialJSON, err := json.Marshal(v)
		if err != nil {
			t.Fatal(err)
		}
		stable := struct {
			Username     string `json:"username"`
			Salt         []byte `json:"salt"`
			Hash         []byte `json:"hash"`
			TOTPSecret   string `json:"totp_secret"`
			TOTPDisabled bool   `json:"totp_disabled,omitempty"`
		}{v.Username, v.Salt, v.Hash, v.TOTPSecret, v.TOTPDisabled}
		input, err := json.Marshal(stable)
		if err != nil {
			t.Fatal(err)
		}
		return credentialCase{v, string(credentialJSON), string(input), credentialFingerprint(v)}
	}
	disabled := c
	disabled.TOTPDisabled = true
	disabled.LastStep = 999
	type secretCase struct {
		Label       string `json:"label"`
		Secret      string `json:"secret"`
		GoLoadValid bool   `json:"go_load_valid"`
	}
	secretCases := make([]secretCase, 0, 4)
	secretInputs := []secretCase{
		{Label: "short-19", Secret: base32.StdEncoding.WithPadding(base32.NoPadding).EncodeToString(secret[:19])},
		{Label: "exact-20", Secret: c.TOTPSecret},
		{Label: "long-21", Secret: base32.StdEncoding.WithPadding(base32.NoPadding).EncodeToString(append(append([]byte{}, secret...), 21))},
		{Label: "invalid-alphabet", Secret: c.TOTPSecret[:len(c.TOTPSecret)-1] + "*"},
	}
	secretPath := filepath.Join(t.TempDir(), "synthetic-credentials.json")
	for _, input := range secretInputs {
		candidate := c
		candidate.TOTPSecret = input.Secret
		encoded, err := json.Marshal(candidate)
		if err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(secretPath, encoded, 0600); err != nil {
			t.Fatal(err)
		}
		_, err = LoadCredentials(secretPath)
		input.GoLoadValid = err == nil
		secretCases = append(secretCases, input)
	}
	sessionsJSON, err := json.Marshal(persistedSessionFile{Version: 1, Sessions: []persistedSession{session}})
	if err != nil {
		t.Fatal(err)
	}
	fixture := struct {
		Version       int                  `json:"version"`
		Password      string               `json:"password"`
		Enabled       credentialCase       `json:"enabled"`
		Disabled      credentialCase       `json:"disabled"`
		Token         string               `json:"token"`
		TokenHash     string               `json:"token_hash"`
		CSRF          string               `json:"csrf"`
		TOTPCode      string               `json:"totp_code"`
		TOTPLowerCode string               `json:"totp_lower_code"`
		UnixSeconds   int64                `json:"unix_seconds"`
		Step          int64                `json:"step"`
		SessionFile   persistedSessionFile `json:"session_file"`
		SessionJSON   string               `json:"session_json"`
		SecretCases   []secretCase         `json:"secret_cases"`
	}{1, "synthetic-password-123", caseFor(c), caseFor(disabled), token,
		base64.RawURLEncoding.EncodeToString([]byte(sessionKey(token))), csrfToken(token),
		codeAt(c, at), codeAt(c, at.Add(-30*time.Second)), at.Unix(), at.Unix() / 30,
		persistedSessionFile{Version: 1, Sessions: []persistedSession{session}}, string(sessionsJSON), secretCases}
	raw, err := json.MarshalIndent(fixture, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	raw = append(raw, '\n')
	path := filepath.Join("..", "..", "tests", "fixtures", "auth-v1", "synthetic.json")
	if os.Getenv("HMUX_UPDATE_RUST_AUTH_FIXTURE") == "1" {
		if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, raw, 0644); err != nil {
			t.Fatal(err)
		}
	}
	stored, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(stored, raw) {
		t.Fatal("Rust auth compatibility fixture differs from Go oracle")
	}
}

// Exercises Go rollback against current Rust-authored state, not restored copies
// of pre-revocation credentials. All inputs come from an isolated Rust fixture.
func TestRustAuthStoreHandoff(t *testing.T) {
	dir := os.Getenv("HMUX_RUST_AUTH_HANDOFF")
	if dir == "" {
		t.Skip("synthetic handoff directory is supplied by make rust-compat")
	}
	raw, err := os.ReadFile(filepath.Join(dir, "synthetic-handoff.json"))
	if err != nil {
		t.Fatal(err)
	}
	var tokens struct {
		Primary string `json:"primary"`
		Revoked string `json:"revoked"`
		Guest   string `json:"guest"`
	}
	if err := json.Unmarshal(raw, &tokens); err != nil {
		t.Fatal(err)
	}
	a, err := newAuth(filepath.Join(dir, "credentials.json"))
	if err != nil {
		t.Fatal(err)
	}
	defer a.closeConnections()
	for _, token := range []string{tokens.Primary, tokens.Revoked} {
		if _, _, ok := a.get(token, false); ok {
			t.Fatal("Rust-revoked session became valid in Go")
		}
	}
	if _, _, ok := a.get(tokens.Guest, false); !ok {
		t.Fatal("retained guest did not survive Go rollback")
	}
	username, profile, ok := a.identity(tokens.Guest)
	if !ok || username != "guest" || profile != fmt.Sprintf("%x", sha256.Sum256([]byte("guest"))) {
		t.Fatal("guest account scope changed")
	}
	if a.credentials.TOTPDisabled {
		t.Fatal("Rust-enabled TOTP was lost on Go rollback")
	}
	if err := a.logout(tokens.Guest); err != nil {
		t.Fatal(err)
	}
}
