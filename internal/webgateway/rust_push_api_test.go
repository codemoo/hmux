package webgateway

import (
	"bytes"
	"crypto/ecdh"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	webpush "github.com/SherClockHolmes/webpush-go"
)

type rustPushAPICase struct {
	Name       string `json:"name"`
	Path       string `json:"path"`
	Body       string `json:"body"`
	GoStatus   int    `json:"go_status"`
	RustStatus int    `json:"rust_status"`
	Delta      string `json:"delta,omitempty"`
}

// Execute the actual Go handlers with synthetic local state, without provider I/O.
func TestRustPushAPIOracle(t *testing.T) {
	s := testServer(t)
	token := loginForTest(t, s)
	csrf, _, _ := s.auth.get(token, false)
	s.push.client = pushClientFunc(func(*http.Request) (*http.Response, error) { t.Fatal("oracle must not send"); return nil, nil })
	encode := base64.RawURLEncoding.EncodeToString
	key, err := ecdh.P256().NewPrivateKey(bytes.Repeat([]byte{7}, 32))
	if err != nil {
		t.Fatal(err)
	}
	sub := webpush.Subscription{Endpoint: "https://web.push.apple.com/api-fixture", Keys: webpush.Keys{Auth: encode(bytes.Repeat([]byte{7}, 16)), P256dh: encode(key.PublicKey().Bytes())}}
	raw, _ := json.Marshal(sub)
	base := string(raw)
	insert := func(field string) string { return base[:len(base)-1] + "," + field + "}" }
	type candidate struct{ name, path, body, delta string }
	var cases []candidate
	add := func(name, path, body, delta string) {
		cases = append(cases, candidate{name, "/api/push/" + path, body, delta})
	}
	for _, c := range []struct{ name, body, delta string }{
		{"valid", base, ""},
		{"expiration-null", insert(`"expirationTime":null`), ""},
		{"expiration-number", insert(`"expirationTime":1790000000000`), ""},
		{"expiration-fraction", insert(`"expirationTime":0.5`), ""},
		{"expiration-negative", insert(`"expirationTime":-1`), ""},
		{"expiration-string", insert(`"expirationTime":"1790000000000"`), ""},
		{"expiration-bool", insert(`"expirationTime":false`), ""},
		{"expiration-array", insert(`"expirationTime":[]`), ""},
		{"expiration-overflow", insert(`"expirationTime":1e999`), ""},
		{"expiration-duplicate-null", insert(`"expirationTime":null,"expirationTime":null`), "duplicate fields rejected"},
		{"endpoint-duplicate", strings.Replace(base, `"endpoint":`, `"endpoint":"https://invalid.example/x","endpoint":`, 1), "duplicate fields rejected"},
		{"keys-duplicate", insert(`"keys":` + strings.SplitN(base, `"keys":`, 2)[1][:len(strings.SplitN(base, `"keys":`, 2)[1])-1]), "duplicate fields rejected"},
		{"unknown", insert(`"login_id":"not-authority"`), ""},
		{"unknown-key", strings.Replace(base, `"auth":`, `"secret":true,"auth":`, 1), ""},
		{"array", "[]", ""},
		{"null", "null", ""},
		{"empty", "", ""},
		{"trailing", base + "{}", ""},
	} {
		add("subscribe-"+c.name, "subscribe", c.body, c.delta)
	}
	for _, c := range []struct{ name, body, delta string }{
		{"valid", `{"client_id":"client","session":{"id":"$1","created_at":42}}`, ""},
		{"clear-null", `{"client_id":"client","session":null}`, ""},
		{"clear-omitted", `{"client_id":"client"}`, ""},
		{"empty-client", `{"client_id":"","session":null}`, ""},
		{"null-client", `{"client_id":null,"session":null}`, ""},
		{"long-client", `{"client_id":"` + strings.Repeat("x", 65) + `"}`, ""},
		{"control-client", `{"client_id":"a\nb"}`, ""},
		{"unicode-client", `{"client_id":"한글"}`, ""},
		{"long-unicode-client", `{"client_id":"` + strings.Repeat("한", 22) + `"}`, ""},
		{"bad-session", `{"client_id":"client","session":{"id":"bad","created_at":42}}`, ""},
		{"missing-created", `{"client_id":"client","session":{"id":"$1"}}`, ""},
		{"zero-created", `{"client_id":"client","session":{"id":"$1","created_at":0}}`, ""},
		{"fraction-created", `{"client_id":"client","session":{"id":"$1","created_at":1.5}}`, ""},
		{"duplicate-client", `{"client_id":"a","client_id":"b"}`, "duplicate fields rejected"},
		{"null-session-duplicate", `{"client_id":"client","session":null,"session":null}`, "duplicate fields rejected"},
		{"unknown", `{"client_id":"client","user":"owner"}`, ""},
		{"null", "null", ""},
		{"array", "[]", ""},
	} {
		add("presence-"+c.name, "presence", c.body, c.delta)
	}
	for _, c := range []struct{ name, body, delta string }{{"empty", "{}", ""}, {"null", "null", "request object required"}, {"unknown", `{"login_id":"other"}`, ""}, {"array", "[]", ""}, {"trailing", "{}{}", ""}} {
		add("unsubscribe-"+c.name, "unsubscribe", c.body, c.delta)
	}
	rows := make([]rustPushAPICase, 0, len(cases))
	for _, c := range cases {
		req := httptest.NewRequest("POST", s.origin+c.path, strings.NewReader(c.body))
		req.Header.Set("Content-Type", "application/json")
		req.Header.Set("Origin", s.origin)
		req.Header.Set("X-CSRF-Token", csrf)
		req.AddCookie(&http.Cookie{Name: cookieName, Value: token})
		res := httptest.NewRecorder()
		s.ServeHTTP(res, req)
		rustStatus := res.Code
		if c.delta != "" {
			rustStatus = 400
		}
		rows = append(rows, rustPushAPICase{c.name, c.path, c.body, res.Code, rustStatus, c.delta})
	}
	expected, err := json.MarshalIndent(rows, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	expected = append(expected, '\n')
	path := filepath.Join("..", "..", "tests", "fixtures", "push-v1", "go-api.json")
	if os.Getenv("UPDATE_HMUX_RUST_PUSH_API_FIXTURE") == "1" {
		if err = os.WriteFile(path, expected, 0644); err != nil {
			t.Fatal(err)
		}
	}
	old, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(old, expected) {
		t.Fatal("Go push API contract changed")
	}
}
