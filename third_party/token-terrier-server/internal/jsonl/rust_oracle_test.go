package jsonl

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

func TestRustParserOracle(t *testing.T) {
	type input struct {
		Name     string        `json:"name"`
		Provider wire.Provider `json:"provider"`
		Line     string        `json:"line"`
		Path     string        `json:"path"`
	}
	type output struct {
		Name      string `json:"name"`
		Tokens    int    `json:"tokens"`
		Timestamp string `json:"timestamp"`
		Model     string `json:"model"`
		Session   string `json:"session"`
	}
	cases := []input{
		{"claude", wire.ProviderClaude, `{"type":"assistant","timestamp":"2026-09-24T00:00:00Z","sessionId":"s1","message":{"model":"sonnet","usage":{"input_tokens":100,"output_tokens":20,"cache_creation_input_tokens":5,"cache_read_input_tokens":900}}}`, "synthetic.jsonl"},
		{"claude_fraction", wire.ProviderClaude, `{"type":"assistant","timestamp":"2026-09-24T00:00:01+09:00","message":{"usage":{"input_tokens":2.9,"output_tokens":3,"cache_creation_input_tokens":1}}}`, "synthetic.jsonl"},
		{"codex", wire.ProviderCodex, `{"type":"event_msg","timestamp":"2026-09-24T00:00:02Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":130,"cached_input_tokens":100,"output_tokens":40,"reasoning_output_tokens":20}}}}`, "synthetic.jsonl"},
		{"cached_only", wire.ProviderCodex, `{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":100}}}}`, "synthetic.jsonl"},
		{"bad_line", wire.ProviderClaude, `{broken`, "synthetic.jsonl"},
		{"claude_irrelevant_wrong_type", wire.ProviderClaude, `{"type":"assistant","timestamp":"2026-09-24T00:00:00Z","sessionId":"s1","payload":42,"message":{"model":"sonnet","usage":{"input_tokens":100,"output_tokens":20,"cache_creation_input_tokens":5}}}`, "synthetic.jsonl"},
		{"codex_irrelevant_wrong_type", wire.ProviderCodex, `{"type":"event_msg","timestamp":"2026-09-24T00:00:02Z","message":42,"payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":130,"cached_input_tokens":100,"output_tokens":40}}}}`, "synthetic.jsonl"},
	}
	outputs := make([]output, 0, len(cases))
	for _, c := range cases {
		o := output{Name: c.Name}
		if ev := ParseLine(c.Provider, []byte(c.Line), c.Path); ev != nil {
			o.Tokens = ev.Tokens
			o.Timestamp = ev.Timestamp.UTC().Format("2006-01-02T15:04:05Z07:00")
			o.Model = ev.Model
			o.Session = ev.SessionKey
		}
		outputs = append(outputs, o)
	}
	root := filepath.Join("..", "..", "..", "..", "tests", "fixtures", "usage-activity-v1")
	verifyJSON(t, filepath.Join(root, "parser-cases.json"), cases)
	verifyJSON(t, filepath.Join(root, "parser-go-oracle.json"), outputs)
}
func verifyJSON(t *testing.T, path string, value any) {
	t.Helper()
	data, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	data = append(data, '\n')
	if os.Getenv("HMUX_UPDATE_USAGE_ORACLES") == "1" {
		if err := os.WriteFile(path, data, 0644); err != nil {
			t.Fatal(err)
		}
	}
	stored, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(data, stored) {
		t.Fatalf("oracle changed: %s", path)
	}
}
