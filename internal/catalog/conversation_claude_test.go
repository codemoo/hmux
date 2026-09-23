package catalog

import (
	"encoding/json"
	"testing"
)

func TestClaudePublicConversation(t *testing.T) {
	tests := []struct{ line, text string }{
		{`{"type":"user","message":{"role":"user","content":"Hello"}}`, "Hello"},
		{`{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"secret"},{"type":"tool_use","input":{"secret":"private"}},{"type":"text","text":"| A | B |\n|---|---|\n|1|2|"}]}}`, "| A | B |\n|---|---|\n|1|2|"},
		{`{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"secret"}]}}`, ""},
		{`{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":"secret"}}`, ""},
		{`{"type":"user","isCompactSummary":true,"message":{"role":"user","content":"summary"}}`, ""},
		{`{"type":"user","isMeta":true,"message":{"role":"user","content":"injected"}}`, ""},
		{`{"type":"user","message":{"role":"user","content":"<system-reminder>secret</system-reminder>"}}`, ""},
		{`{"type":"user","message":{"role":"assistant","content":"wrong role"}}`, ""},
	}
	for _, tt := range tests {
		m, ok, _ := parseClaudeConversationMessage([]byte(tt.line), []byte("fixture"), 1)
		if ok != (tt.text != "") || m.Text != tt.text {
			t.Fatalf("unexpected public message: %+v", m)
		}
	}
	raw, _ := json.Marshal(map[string]any{"type": "user", "message": map[string]any{"role": "user", "content": string(make([]byte, conversationMessageSize+1))}})
	_, ok, omitted := parseClaudeConversationMessage(raw, nil, 0)
	if ok || !omitted {
		t.Fatal("size bound not preserved")
	}
}
