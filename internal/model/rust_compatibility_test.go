package model

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// Synthetic model JSON is emitted by Go itself for the Rust codec comparison.
func TestRustModelOracle(t *testing.T) {
	observed := time.Date(2026, 9, 8, 12, 0, 0, 123000000, time.UTC)
	cpu, used, total := 20.5, uint64(4), uint64(8)
	catalog := Catalog{
		ProtocolVersion: ProtocolVersion,
		GeneratedAt:     observed,
		Sessions: []Session{{
			ID: "$7", Name: "synthetic", CreatedAt: 1700000000,
			WindowNames: []string{}, Tags: []string{"ai"},
			Workflow:  &WorkflowSummary{Running: 1, UpdatedAt: 1700000001},
			Workflows: []Workflow{{ID: "wf-synthetic", Source: "codex-hook", Status: "running", StartedAt: 1700000000, UpdatedAt: 1700000001, Nodes: []WorkflowNode{}}},
			PanePID:   12345,
		}},
		HostMetrics: &HostMetrics{ObservedAt: observed, CPUPercent: &cpu, MemoryUsedBytes: &used, MemoryTotalBytes: &total},
	}
	conversation := Conversation{Provider: "codex", SessionID: "$7", CreatedAt: 1700000000, Status: ConversationReady,
		Messages: []ConversationMessage{{ID: "synthetic-1", Role: "assistant", Text: "안녕하세요"}}}
	nilSlices := Session{ID: "$8", Name: "omission", CreatedAt: 1700000002}
	emptySlices := nilSlices
	emptySlices.Tags = []string{}
	emptySlices.Workflows = []Workflow{}
	metricsArrayInput := `{"protocol_version":1,"host_metrics":["2026-09-08T12:00:00Z",20.5,null,null,null,null,null]}`
	var metricsArrayResult Catalog
	if err := json.Unmarshal([]byte(metricsArrayInput), &metricsArrayResult); err != nil || metricsArrayResult.HostMetrics == nil || ValidateHostMetrics(metricsArrayResult.HostMetrics) == nil {
		t.Fatalf("Go did not fail open on positional host metrics: %v", err)
	}
	arrayRejections := map[string]string{
		"catalog": "[]", "conversation": "[]", "inventory": "[]", "session": "[]",
		"catalog_nested_session":      `{"sessions":[[]]}`,
		"catalog_nested_workflow":     `{"sessions":[{"workflows":[[]]}]}`,
		"catalog_nested_node":         `{"sessions":[{"workflows":[{"nodes":[[]]}]}]}`,
		"inventory_nested_profile":    `{"profiles":[[]]}`,
		"conversation_nested_message": `{"messages":[[]]}`,
		"session_nested_identity":     `{"restored_from":[]}`,
		"session_nested_summary":      `{"workflow":[]}`,
	}
	for name, raw := range arrayRejections {
		var destination any
		switch name {
		case "catalog", "catalog_nested_session", "catalog_nested_workflow", "catalog_nested_node":
			destination = &Catalog{}
		case "conversation", "conversation_nested_message":
			destination = &Conversation{}
		case "inventory", "inventory_nested_profile":
			destination = &Inventory{}
		case "session", "session_nested_identity", "session_nested_summary":
			destination = &Session{}
		}
		if err := json.Unmarshal([]byte(raw), destination); err == nil {
			t.Fatalf("Go accepted array as %s struct", name)
		}
	}
	result := map[string]any{
		"catalog": catalog, "conversation": conversation,
		"session_nil_slices": nilSlices, "session_empty_slices": emptySlices,
		"array_decode_rejections": arrayRejections,
		"metrics_array_input":     metricsArrayInput, "metrics_array_result": metricsArrayResult,
	}
	raw, err := json.MarshalIndent(result, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	raw = append(raw, '\n')
	path := filepath.Join("..", "..", "tests", "fixtures", "config-v1", "go-model-oracle.json")
	if os.Getenv("UPDATE_HMUX_RUST_MODEL_FIXTURE") == "1" {
		if err := os.WriteFile(path, raw, 0o600); err != nil {
			t.Fatal(err)
		}
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(raw, want) {
		t.Fatal("Go model oracle changed; review and regenerate fixture")
	}
}
