package catalog

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/codemoo/hmux/internal/model"
)

type rustTranscriptFixture struct {
	Name      string                      `json:"name"`
	Provider  string                      `json:"provider"`
	Line      string                      `json:"line"`
	Messages  []model.ConversationMessage `json:"messages"`
	Truncated bool                        `json:"truncated"`
}

// Fixed identity and offsets exercise the real public parsers without opening
// provider storage. Synthetic records are inert test input.
func TestRustTranscriptOracle(t *testing.T) {
	codex := func(role, text string) string {
		kind := "input_text"
		if role == "assistant" {
			kind = "output_text"
		}
		raw, _ := json.Marshal(map[string]any{"type": "response_item", "payload": map[string]any{"type": "message", "role": role, "content": []map[string]string{{"type": kind, "text": text}}}})
		return string(raw)
	}
	claude := func(role string, content any) string {
		raw, _ := json.Marshal(map[string]any{"type": role, "message": map[string]any{"role": role, "content": content}})
		return string(raw)
	}
	fixtures := []rustTranscriptFixture{}
	add := func(name, provider, line string) {
		fixtures = append(fixtures, rustTranscriptFixture{Name: name, Provider: provider, Line: line})
	}
	for _, role := range []string{"user", "assistant"} {
		for _, text := range []string{"hello", " ", "한글\n<A>&\u2028\u2029\ttext", "| A | B |\n|---|---|", " ordinary\nmessage "} {
			add("codex-"+role+"-"+text, "codex", codex(role, text))
			add("claude-"+role+"-"+text, "claude", claude(role, text))
		}
	}
	for _, prefix := range []string{"# AGENTS.md instructions", "<environment_context>", "<codex_internal_context", "<system-reminder>", "<task-notification>", "This session is being continued from a previous conversation that ran out of context."} {
		add("codex-filter-"+prefix, "codex", codex("user", prefix+"synthetic hidden text"))
		add("claude-filter-"+prefix, "claude", claude("user", prefix+"synthetic hidden text"))
		add("claude-mixed-"+prefix, "claude", claude("user", []map[string]string{{"type": "text", "text": prefix + "hidden"}, {"type": "text", "text": "visible"}}))
	}
	for _, channel := range []string{"analysis", "commentary", "final", "summary", ""} {
		line := codex("assistant", "channel check")
		line = strings.Replace(line, `"role":"assistant"`, `"role":"assistant","channel":"`+channel+`"`, 1)
		add("codex-channel-"+channel, "codex", line)
	}
	add("claude-parts", "claude", claude("assistant", []map[string]string{{"type": "thinking", "text": "hidden"}, {"type": "tool_use", "text": "hidden"}, {"type": "text", "text": "one"}, {"type": "text", "text": "two"}}))
	for _, flag := range []string{"isSidechain", "isMeta", "isCompactSummary"} {
		add("claude-"+flag, "claude", strings.Replace(claude("assistant", "hidden"), `{`, `{"`+flag+`":true,`, 1))
	}
	add("codex-escaped-role", "codex", strings.Replace(codex("user", "escaped"), `"role":"user"`, `"role":"\u0075ser"`, 1))
	add("claude-escaped-type", "claude", strings.ReplaceAll(claude("user", "escaped"), `"user"`, `"\u0075ser"`))
	add("codex-null-part", "codex", `{"type":"response_item","payload":{"type":"message","role":"user","content":[null,{"type":"input_text","text":"visible"}]}}`)
	add("codex-tool-recipient", "codex", strings.Replace(codex("assistant", "hidden"), `"role":"assistant"`, `"role":"assistant","recipient":"functions.exec"`, 1))
	for i := range fixtures {
		f := &fixtures[i]
		parser := parseConversationMessage
		if f.Provider == "claude" {
			parser = parseClaudeConversationMessage
		}
		message, ok, omitted := parser([]byte(f.Line), make([]byte, 16), 0)
		f.Messages = []model.ConversationMessage{}
		if ok {
			f.Messages = append(f.Messages, message)
		}
		f.Truncated = omitted
	}
	raw, err := json.MarshalIndent(fixtures, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	raw = append(raw, '\n')
	path := filepath.Join("..", "..", "tests", "fixtures", "conversation-v1", "go-oracle.json")
	if os.Getenv("UPDATE_HMUX_RUST_CONVERSATION_FIXTURE") == "1" {
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
		t.Fatal("Go public transcript parser changed; review synthetic oracle")
	}
}
