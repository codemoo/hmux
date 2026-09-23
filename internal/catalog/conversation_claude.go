package catalog

import (
	"encoding/json"
	"strings"

	"github.com/codemoo/hmux/internal/model"
)

// Decode only visible text from the bound main transcript. Tool results,
// thinking, progress, sidechains and compaction summaries are never public text.
func parseClaudeConversationMessage(line, identity []byte, offset int64) (model.ConversationMessage, bool, bool) {
	var event struct {
		Type      string `json:"type"`
		Sidechain bool   `json:"isSidechain"`
		Meta      bool   `json:"isMeta"`
		Summary   bool   `json:"isCompactSummary"`
		SessionID string `json:"sessionId"`
		Message   struct {
			Role    string          `json:"role"`
			Content json.RawMessage `json:"content"`
		} `json:"message"`
	}
	if json.Unmarshal(line, &event) != nil || event.Sidechain || event.Meta || event.Summary ||
		(event.Type != "user" && event.Type != "assistant") || event.Message.Role != event.Type {
		return model.ConversationMessage{}, false, false
	}
	var parts []struct {
		Type string `json:"type"`
		Text string `json:"text"`
	}
	var plain string
	if json.Unmarshal(event.Message.Content, &plain) == nil {
		parts = append(parts, struct {
			Type string `json:"type"`
			Text string `json:"text"`
		}{"text", plain})
	} else if json.Unmarshal(event.Message.Content, &parts) != nil {
		return model.ConversationMessage{}, false, false
	}
	content := []map[string]string{}
	for _, part := range parts {
		if part.Type != "text" || strings.TrimSpace(part.Text) == "" {
			continue
		}
		if event.Type == "user" && claudeInjectedText(part.Text) {
			continue
		}
		kind := "input_text"
		if event.Type == "assistant" {
			kind = "output_text"
		}
		content = append(content, map[string]string{"type": kind, "text": part.Text})
	}
	// Reuse public-text filtering, size limits and stable position identities.
	canonical, _ := json.Marshal(map[string]any{"type": "response_item", "payload": map[string]any{"type": "message", "role": event.Type, "content": content}})
	return parseConversationMessage(canonical, identity, offset)
}
func claudeInjectedText(text string) bool {
	trimmed := strings.TrimSpace(text)
	for _, prefix := range []string{"<local-command-caveat>", "<local-command-stdout>", "<command-name>", "<system-reminder>", "<task-notification>", "<teammate-message>", "This session is being continued from a previous conversation that ran out of context."} {
		if strings.HasPrefix(trimmed, prefix) {
			return true
		}
	}
	return false
}
