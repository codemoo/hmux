// Package jsonl parses Claude Code / Codex session logs into TokenEvents.
//
// Both providers append one JSON object per line to per-session files. We
// extract token-bearing lines and report tokens in the same convention as
// the Swift JSONLLineParser:
//
//	Claude:  type == "assistant" && message.usage exists
//	         tokens = input + output + cache_creation
//	         (cache_read excluded — replay, not fresh work)
//
//	Codex:   type == "event_msg" && payload.type == "token_count"
//	         tokens = (input - cached) + output
//	         (output already includes reasoning in current Codex JSONL)
package jsonl

import (
	"encoding/json"
	"strings"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

// TokenEvent is one token-counted activity observed in a session log.
type TokenEvent struct {
	Provider      wire.Provider
	Timestamp     time.Time
	Tokens        int
	Model         string
	SessionKey    string
	AccountNumber int
	Source        string
}

// ParseLine returns a TokenEvent for the given JSONL line if it carries
// token usage; nil otherwise. Returns nil for blank lines, malformed JSON,
// and lines that aren't of the right `type`.
func ParseLine(provider wire.Provider, line []byte, sessionFilePath string) *TokenEvent {
	line = trim(line)
	if len(line) == 0 {
		return nil
	}
	var obj map[string]json.RawMessage
	if err := json.Unmarshal(line, &obj); err != nil {
		return nil
	}
	switch provider {
	case wire.ProviderClaude:
		return parseClaude(obj, sessionFilePath)
	case wire.ProviderCodex:
		return parseCodex(obj, sessionFilePath)
	}
	return nil
}

func parseClaude(obj map[string]json.RawMessage, sessionFilePath string) *TokenEvent {
	if !stringEq(obj["type"], "assistant") {
		return nil
	}
	msgRaw, ok := obj["message"]
	if !ok {
		return nil
	}
	var msg map[string]json.RawMessage
	if err := json.Unmarshal(msgRaw, &msg); err != nil {
		return nil
	}
	usageRaw, ok := msg["usage"]
	if !ok {
		return nil
	}
	var usage map[string]json.RawMessage
	if err := json.Unmarshal(usageRaw, &usage); err != nil {
		return nil
	}
	input := intField(usage, "input_tokens")
	output := intField(usage, "output_tokens")
	cacheCreate := intField(usage, "cache_creation_input_tokens")
	total := input + output + cacheCreate
	if total <= 0 {
		return nil
	}
	timestamp := timestampField(obj, "timestamp")
	model := stringField(msg, "model")
	sessionKey := stringField(obj, "sessionId")
	if sessionKey == "" {
		sessionKey = sessionFilePath
	}
	return &TokenEvent{
		Provider:   wire.ProviderClaude,
		Timestamp:  timestamp,
		Tokens:     total,
		Model:      model,
		SessionKey: sessionKey,
	}
}

func parseCodex(obj map[string]json.RawMessage, sessionFilePath string) *TokenEvent {
	if !stringEq(obj["type"], "event_msg") {
		return nil
	}
	payloadRaw, ok := obj["payload"]
	if !ok {
		return nil
	}
	var payload map[string]json.RawMessage
	if err := json.Unmarshal(payloadRaw, &payload); err != nil {
		return nil
	}
	if !stringEq(payload["type"], "token_count") {
		return nil
	}
	infoRaw, ok := payload["info"]
	if !ok {
		return nil
	}
	var info map[string]json.RawMessage
	if err := json.Unmarshal(infoRaw, &info); err != nil {
		return nil
	}
	var lastUsage map[string]json.RawMessage
	if v, ok := info["last_token_usage"]; ok {
		_ = json.Unmarshal(v, &lastUsage)
	}
	if lastUsage == nil {
		lastUsage = map[string]json.RawMessage{}
	}
	input := intField(lastUsage, "input_tokens")
	cached := intField(lastUsage, "cached_input_tokens")
	output := intField(lastUsage, "output_tokens")
	nonCached := input - cached
	if nonCached < 0 {
		nonCached = 0
	}
	// Current Codex contract has total_tokens == input_tokens + output_tokens;
	// reasoning_output_tokens is a subset of output_tokens, not an additive
	// bucket. Adding it here would inflate every reasoning-bearing turn.
	total := nonCached + output
	if total <= 0 {
		return nil
	}
	timestamp := timestampField(obj, "timestamp")
	return &TokenEvent{
		Provider:   wire.ProviderCodex,
		Timestamp:  timestamp,
		Tokens:     total,
		SessionKey: sessionFilePath,
	}
}

func stringEq(raw json.RawMessage, want string) bool {
	if len(raw) == 0 {
		return false
	}
	var s string
	if err := json.Unmarshal(raw, &s); err != nil {
		return false
	}
	return s == want
}

func intField(m map[string]json.RawMessage, key string) int {
	raw, ok := m[key]
	if !ok {
		return 0
	}
	var n int
	if err := json.Unmarshal(raw, &n); err == nil {
		return n
	}
	var f float64
	if err := json.Unmarshal(raw, &f); err == nil {
		return int(f)
	}
	return 0
}

func stringField(m map[string]json.RawMessage, key string) string {
	raw, ok := m[key]
	if !ok {
		return ""
	}
	var s string
	if err := json.Unmarshal(raw, &s); err == nil {
		return s
	}
	return ""
}

func timestampField(m map[string]json.RawMessage, key string) time.Time {
	s := stringField(m, key)
	if s == "" {
		return time.Now()
	}
	for _, layout := range []string{
		"2006-01-02T15:04:05.000Z",
		"2006-01-02T15:04:05Z",
		time.RFC3339Nano,
		time.RFC3339,
	} {
		if t, err := time.Parse(layout, s); err == nil {
			return t
		}
	}
	return time.Now()
}

func trim(b []byte) []byte {
	return []byte(strings.TrimSpace(string(b)))
}
