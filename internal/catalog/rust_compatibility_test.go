package catalog

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

type rustBasicCatalogFixture struct {
	Name     string         `json:"name"`
	Sessions string         `json:"sessions"`
	Windows  string         `json:"windows"`
	Valid    bool           `json:"valid"`
	Catalog  *model.Catalog `json:"catalog,omitempty"`
}

func (f rustBasicCatalogFixture) Output(_ context.Context, args ...string) ([]byte, error) {
	switch args[0] {
	case "list-sessions":
		return []byte(f.Sessions), nil
	case "list-windows":
		return []byte(f.Windows), nil
	default:
		return nil, fmt.Errorf("unexpected synthetic command")
	}
}

// The existing Go basic reader is the oracle. No tmux command or process
// inspection is executed. Set UPDATE_HMUX_RUST_CATALOG_FIXTURE=1 to regenerate.
func TestRustBasicCatalogOracle(t *testing.T) {
	row := func(fields ...string) string { return strings.Join(fields, separator) + "\n" }
	session := row("$7", "코덱\x1b[31m", "1700000000", "1700000200", "0", "2", "", "3")
	active := row("$7", "editor", "1", "/tmp/긴 경로", "codex", "120", "40", "123")
	fixtures := []rustBasicCatalogFixture{
		{Name: "empty"},
		{Name: "no-windows", Sessions: session},
		{Name: "grouped-and-sorted", Sessions: session + row("$8", "internal", "1700000000", "1700000300", "1", "2", "1", "3") + row("$3", "shell", "1700000001", "1700000200", "1", "1", "", ""), Windows: active + row("$7", "logs", "0", "/tmp", "tail", "bad", "bad", "bad") + row("$8", "hidden", "1", "/tmp", "sh", "80", "24", "99")},
		{Name: "unavailable-dimensions", Sessions: session, Windows: row("$7", "editor", "1", "/tmp", "sh", "", "", "")},
		{Name: "invalid-identity", Sessions: row("$7;bad", "shell", "1", "1", "0", "1", "", "")},
		{Name: "duplicate", Sessions: session + session},
		{Name: "separator-in-path", Sessions: session, Windows: row("$7", "editor", "1", "/tmp/"+separator, "sh", "80", "24", "123")},
		{Name: "invalid-active-dimension", Sessions: session, Windows: row("$7", "editor", "1", "/tmp", "sh", "100001", "24", "123")},
		{Name: "invalid-group-count", Sessions: row("$7", "shell", "1", "1", "0", "1", "", "10001")},
	}
	for i := range fixtures {
		value, err := ReadBasic(context.Background(), fixtures[i])
		fixtures[i].Valid = err == nil
		if err == nil {
			value.GeneratedAt = time.Date(2026, 9, 24, 0, 0, 0, 0, time.UTC)
			fixtures[i].Catalog = &value
		}
	}
	raw, err := json.MarshalIndent(fixtures, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	raw = append(raw, '\n')
	path := filepath.Join("..", "..", "tests", "fixtures", "catalog-v1", "go-oracle.json")
	if os.Getenv("UPDATE_HMUX_RUST_CATALOG_FIXTURE") == "1" {
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
		t.Fatal("Go basic catalog changed; review and regenerate synthetic oracle")
	}
}
