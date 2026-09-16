package jsonl

import (
	"bufio"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

func TestCodexSharedContractFixture(t *testing.T) {
	path := filepath.Join(
		"..", "..", "Tests", "TokenUsageCoreTests", "Fixtures",
		"codex_token_count_contract.jsonl")
	file, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	caseNumber := 0
	for scanner.Scan() {
		caseNumber++
		line := append([]byte(nil), scanner.Bytes()...)
		var fixture struct {
			Expected int `json:"test_expected_fresh_tokens"`
		}
		if err := json.Unmarshal(line, &fixture); err != nil {
			t.Fatalf("case %d: decode expected value: %v", caseNumber, err)
		}
		event := ParseLine(wire.ProviderCodex, line, "fixture.jsonl")
		if event == nil {
			t.Fatalf("case %d: expected token event", caseNumber)
		}
		if event.Tokens != fixture.Expected {
			t.Fatalf("case %d: tokens = %d, want %d", caseNumber, event.Tokens, fixture.Expected)
		}
	}
	if err := scanner.Err(); err != nil {
		t.Fatal(err)
	}
	if caseNumber != 2 {
		t.Fatalf("fixture cases = %d, want 2", caseNumber)
	}
}
