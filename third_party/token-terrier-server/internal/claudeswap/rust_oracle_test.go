package claudeswap

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

// Synthetic source fixtures are parsed by the Go command path before their
// transport-allowlisted projection is compared with the Rust implementation.
func TestRustCommandOracle(t *testing.T) {
	base := filepath.Join("..", "..", "..", "..", "tests", "fixtures", "usage-cswap-v1")
	data, err := os.ReadFile(filepath.Join(base, "cases.json"))
	if err != nil {
		t.Fatal(err)
	}
	var cases []struct {
		Name  string          `json:"name"`
		Now   string          `json:"now"`
		Input json.RawMessage `json:"input"`
	}
	if err := json.Unmarshal(data, &cases); err != nil {
		t.Fatal(err)
	}
	var result []struct {
		Name      string              `json:"name"`
		Valid     bool                `json:"valid"`
		Accounts  []wire.AccountUsage `json:"accounts,omitempty"`
		UpdatedAt *string             `json:"updated_at,omitempty"`
	}
	for _, c := range cases {
		now, err := time.Parse(time.RFC3339Nano, c.Now)
		if err != nil {
			t.Fatal(err)
		}
		accounts, updated, err := parseCommandAccounts(c.Input, now)
		item := struct {
			Name      string              `json:"name"`
			Valid     bool                `json:"valid"`
			Accounts  []wire.AccountUsage `json:"accounts,omitempty"`
			UpdatedAt *string             `json:"updated_at,omitempty"`
		}{Name: c.Name, Valid: err == nil}
		if err == nil {
			item.Accounts = accounts
			item.UpdatedAt = updated
		}
		result = append(result, item)
	}
	want, err := json.MarshalIndent(result, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	want = append(want, '\n')
	path := filepath.Join(base, "go-oracle.json")
	if os.Getenv("HMUX_UPDATE_CSWAP_ORACLE") == "1" {
		if err := os.WriteFile(path, want, 0644); err != nil {
			t.Fatal(err)
		}
		return
	}
	got, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got, want) {
		t.Fatalf("Go oracle differs from synthetic command parse projection")
	}
}
