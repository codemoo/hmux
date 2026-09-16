// Package auth holds OAuth credential parsing/representation for both
// providers and the local file source used by the standalone server.
package auth

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

// OAuthCredential mirrors Sources/TokenUsageCore/Auth/OAuthCredential.swift.
type OAuthCredential struct {
	Provider      wire.Provider
	AccessToken   string
	RefreshToken  string
	IDToken       string
	AccountID     string
	AccountEmail  string
	Scopes        []string
	ExpiresAt     *time.Time
	LastRefresh   *time.Time
	RateLimitTier string
}

// AccountKey is the stable identity used to detect login / account switches
// so cached snapshots from a previous account aren't served to the new one.
// Mirrors Swift OAuthCredential.accountKey.
func (c OAuthCredential) AccountKey() string {
	if c.AccountID != "" {
		return "id:" + c.AccountID
	}
	if c.AccountEmail != "" {
		return "em:" + c.AccountEmail
	}
	tail := c.AccessToken
	if len(tail) > 8 {
		tail = tail[len(tail)-8:]
	}
	return "tk:" + tail
}

// CredentialFileError matches Swift CredentialFileError variants.
type CredentialFileError struct {
	Kind    string // "not_found" | "invalid_json" | "missing_token"
	Message string
}

func (e CredentialFileError) Error() string {
	return fmt.Sprintf("credential %s: %s", e.Kind, e.Message)
}

// IsNotFound is a typed predicate for the not-found variant — useful when
// the daemon wants to surface auth-expired vs network errors distinctly.
func IsNotFound(err error) bool {
	var ce CredentialFileError
	return errors.As(err, &ce) && ce.Kind == "not_found"
}

// ParseClaude reads ~/.claude/.credentials.json bytes into an OAuthCredential.
// Mirrors CredentialFiles.parseClaude.
func ParseClaude(data []byte) (OAuthCredential, error) {
	var root struct {
		ClaudeAiOauth *struct {
			AccessToken   string   `json:"accessToken"`
			RefreshToken  string   `json:"refreshToken"`
			ExpiresAt     *float64 `json:"expiresAt"` // ms since epoch
			Scopes        []string `json:"scopes"`
			RateLimitTier string   `json:"rateLimitTier"`
		} `json:"claudeAiOauth"`
	}
	if err := json.Unmarshal(data, &root); err != nil {
		return OAuthCredential{}, CredentialFileError{Kind: "invalid_json", Message: err.Error()}
	}
	if root.ClaudeAiOauth == nil {
		return OAuthCredential{}, CredentialFileError{Kind: "missing_token", Message: "claudeAiOauth"}
	}
	access := strings.TrimSpace(root.ClaudeAiOauth.AccessToken)
	if access == "" {
		return OAuthCredential{}, CredentialFileError{Kind: "missing_token", Message: "claudeAiOauth.accessToken"}
	}
	c := OAuthCredential{
		Provider:      wire.ProviderClaude,
		AccessToken:   access,
		RefreshToken:  trim(root.ClaudeAiOauth.RefreshToken),
		Scopes:        root.ClaudeAiOauth.Scopes,
		RateLimitTier: trim(root.ClaudeAiOauth.RateLimitTier),
	}
	if root.ClaudeAiOauth.ExpiresAt != nil {
		t := time.Unix(0, int64(*root.ClaudeAiOauth.ExpiresAt*float64(time.Millisecond)))
		c.ExpiresAt = &t
	}
	return c, nil
}

// ParseCodex reads ~/.codex/auth.json bytes into an OAuthCredential.
// Mirrors CredentialFiles.parseCodex (handles both apiKey-only and OAuth shapes).
func ParseCodex(data []byte) (OAuthCredential, error) {
	var root map[string]any
	if err := json.Unmarshal(data, &root); err != nil {
		return OAuthCredential{}, CredentialFileError{Kind: "invalid_json", Message: err.Error()}
	}

	// API-key shortcut: opting out of OAuth flow with a static key.
	if v, ok := root["OPENAI_API_KEY"].(string); ok {
		key := trim(v)
		if key != "" {
			return OAuthCredential{Provider: wire.ProviderCodex, AccessToken: key}, nil
		}
	}

	tokens, _ := root["tokens"].(map[string]any)
	if tokens == nil {
		return OAuthCredential{}, CredentialFileError{Kind: "missing_token", Message: "tokens"}
	}
	access := stringIn(tokens, "access_token", "accessToken")
	if access == "" {
		return OAuthCredential{}, CredentialFileError{Kind: "missing_token", Message: "tokens.access_token"}
	}
	c := OAuthCredential{
		Provider:     wire.ProviderCodex,
		AccessToken:  access,
		RefreshToken: stringIn(tokens, "refresh_token", "refreshToken"),
		IDToken:      stringIn(tokens, "id_token", "idToken"),
		AccountID:    stringIn(tokens, "account_id", "accountId"),
		AccountEmail: stringIn(tokens, "account_email", "accountEmail"),
	}
	if v := root["last_refresh"]; v != nil {
		if t := parseDate(v); !t.IsZero() {
			c.LastRefresh = &t
		}
	}
	return c, nil
}

func stringIn(m map[string]any, keys ...string) string {
	for _, k := range keys {
		if v, ok := m[k].(string); ok {
			s := trim(v)
			if s != "" {
				return s
			}
		}
	}
	return ""
}

func parseDate(v any) time.Time {
	switch x := v.(type) {
	case string:
		s := strings.TrimSpace(x)
		if s == "" {
			return time.Time{}
		}
		// Try fractional then plain ISO8601.
		for _, layout := range []string{
			"2006-01-02T15:04:05.000Z",
			"2006-01-02T15:04:05Z",
			time.RFC3339Nano,
			time.RFC3339,
		} {
			if t, err := time.Parse(layout, s); err == nil {
				return t
			}
		}
	case float64:
		return time.Unix(int64(x), 0)
	}
	return time.Time{}
}

func trim(s string) string {
	return strings.TrimSpace(s)
}
