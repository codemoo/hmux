package catalog

import (
	"encoding/json"
	"os"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

// This synthetic fixture is shared with the Rust tracker tests. Keep Go's
// decoder and ID function as the oracle when the fixture changes.
func TestRustCompletionCompatibility(t *testing.T) {
	fixturePath := os.Getenv("HMUX_COMPLETION_FIXTURE")
	if fixturePath == "" {
		fixturePath = "../../tests/fixtures/completion-v1/go-oracle.json"
	}
	raw, err := os.ReadFile(fixturePath)
	if err != nil {
		t.Fatal(err)
	}
	var fixture struct {
		Events []struct {
			Name        string `json:"name"`
			Line        string `json:"line"`
			Kind        string `json:"kind"`
			CompletedAt string `json:"completed_at"`
		} `json:"events"`
		IDs []struct {
			SessionID string `json:"session_id"`
			CreatedAt int64  `json:"created_at"`
			RecordID  string `json:"record_id"`
			Offset    int64  `json:"offset"`
			ID        string `json:"id"`
		} `json:"ids"`
	}
	if err := json.Unmarshal(raw, &fixture); err != nil {
		t.Fatal(err)
	}
	if len(fixture.Events) < 15 || len(fixture.Events) > 25 || len(fixture.IDs) == 0 {
		t.Fatalf("unexpected fixture coverage: %d events, %d IDs", len(fixture.Events), len(fixture.IDs))
	}
	for _, test := range fixture.Events {
		t.Run(test.Name, func(t *testing.T) {
			record, ok := decodeCompletionRecord([]byte(test.Line), 123)
			if !ok {
				if test.Kind != "" {
					t.Fatalf("Go rejected expected %q", test.Kind)
				}
				return
			}
			if record.kind != test.Kind || record.offset != 123 {
				t.Fatalf("Go record kind=%q offset=%d", record.kind, record.offset)
			}
			actual := ""
			if !record.timestamp.IsZero() {
				actual = record.timestamp.UTC().Format(time.RFC3339Nano)
			}
			if actual != test.CompletedAt {
				t.Fatalf("Go timestamp=%q, fixture=%q", actual, test.CompletedAt)
			}
		})
	}
	for _, test := range fixture.IDs {
		identity := model.SessionIdentity{ID: test.SessionID, CreatedAt: test.CreatedAt}
		if actual := completionEventID(identity, test.RecordID, test.Offset); actual != test.ID {
			t.Fatalf("Go event ID for %q=%q, fixture=%q", test.SessionID, actual, test.ID)
		}
	}
}
