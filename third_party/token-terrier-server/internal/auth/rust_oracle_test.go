package auth

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"testing"

	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

type rustCredentialCase struct {
	Name     string `json:"name"`
	Provider string `json:"provider"`
	Body     string `json:"body"`
}

type rustCredentialOracle struct {
	Name      string `json:"name"`
	Token     string `json:"token"`
	AccountID string `json:"account_id"`
	Identity  string `json:"identity"`
	DigestHex string `json:"digest_hex"`
}

func TestRustCredentialOracle(t *testing.T) {
	fixture := filepath.Join("..", "..", "..", "..", "tests", "fixtures", "usage-credentials-v1")
	data, err := os.ReadFile(filepath.Join(fixture, "cases.json"))
	if err != nil {
		t.Fatal(err)
	}
	var cases []rustCredentialCase
	if err := json.Unmarshal(data, &cases); err != nil {
		t.Fatal(err)
	}
	actual := make([]rustCredentialOracle, 0, len(cases))
	for _, testCase := range cases {
		var credential OAuthCredential
		switch testCase.Provider {
		case "claude":
			credential, err = ParseClaude([]byte(testCase.Body))
		case "codex":
			credential, err = ParseCodex([]byte(testCase.Body))
		default:
			t.Fatalf("unknown provider %q", testCase.Provider)
		}
		if err != nil {
			t.Fatalf("%s: %v", testCase.Name, err)
		}
		if credential.Provider != wire.Provider(testCase.Provider) {
			t.Fatalf("%s: provider mismatch", testCase.Name)
		}
		identity := credential.AccountKey()
		digest := sha256.Sum256([]byte(identity))
		actual = append(actual, rustCredentialOracle{
			Name: testCase.Name, Token: credential.AccessToken,
			AccountID: credential.AccountID, Identity: identity,
			DigestHex: hex.EncodeToString(digest[:]),
		})
	}
	data, err = os.ReadFile(filepath.Join(fixture, "go-oracle.json"))
	if err != nil {
		t.Fatal(err)
	}
	var expected []rustCredentialOracle
	if err := json.Unmarshal(data, &expected); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(actual, expected) {
		t.Fatalf("Go credential oracle drift: got %+v, want %+v", actual, expected)
	}
}
