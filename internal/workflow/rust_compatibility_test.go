package workflow

import (
	"bytes"
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"sync"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

func TestRustCurrentWorkflowStateAndConcurrentHelpers(t *testing.T) {
	binary := os.Getenv("HMUX_RUST_WORKFLOW_ORACLE")
	if binary == "" {
		t.Skip("requires separately built Rust workflow_oracle")
	}
	root, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	now := time.Date(2026, 9, 24, 0, 0, 0, 0, time.UTC)
	store := Store{StateDir: root, Now: func() time.Time { return now }}
	binding := Binding{SessionID: "$1", CreatedAt: 1700000000}
	run := func(operation string, extra map[string]any) ([]model.Session, error) {
		q := map[string]any{"root": root, "now": now, "operation": operation, "binding": map[string]any{"id": binding.SessionID, "created_at": binding.CreatedAt}}
		for k, v := range extra {
			q[k] = v
		}
		raw, _ := json.Marshal(q)
		ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
		defer cancel()
		cmd := exec.CommandContext(ctx, binary)
		cmd.Stdin = bytes.NewReader(raw)
		out, err := cmd.Output()
		if err != nil {
			return nil, err
		}
		var result []model.Session
		err = json.Unmarshal(out, &result)
		return result, err
	}
	event := HookEvent{SessionID: "synthetic-provider", TurnID: "synthetic-turn", HookEventName: "UserPromptSubmit", Model: "synthetic-model"}
	if err := store.RecordHook(binding, event); err != nil {
		t.Fatal(err)
	}
	rust, err := run("read", nil)
	if err != nil {
		t.Fatal(err)
	}
	compare := func(rust []model.Session) {
		t.Helper()
		goValue := model.Catalog{Sessions: []model.Session{{ID: binding.SessionID, CreatedAt: binding.CreatedAt}}}
		if err := store.Apply(&goValue); err != nil {
			t.Fatal(err)
		}
		if !reflect.DeepEqual(rust, goValue.Sessions) {
			a, _ := json.Marshal(rust)
			b, _ := json.Marshal(goValue.Sessions)
			t.Fatalf("workflow mismatch\nRust %s\nGo %s", a, b)
		}
		q, _ := json.Marshal(map[string]any{"root": root, "now": now, "operation": "read", "binding": map[string]any{"id": binding.SessionID, "created_at": binding.CreatedAt}, "render": true})
		ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
		defer cancel()
		cmd := exec.CommandContext(ctx, binary)
		cmd.Stdin = bytes.NewReader(q)
		out, err := cmd.Output()
		if err != nil {
			t.Fatal(err)
		}
		var rendered struct {
			Views []SessionView `json:"views"`
			Text  string        `json:"text"`
		}
		if err := json.Unmarshal(out, &rendered); err != nil {
			t.Fatal(err)
		}
		views, err := Views(goValue.Sessions, "")
		if err != nil {
			t.Fatal(err)
		}
		if !reflect.DeepEqual(rendered.Views, views) || rendered.Text != FormatViews(views) {
			t.Fatalf("workflow helper rendering mismatch\nRust %q\nGo %q", rendered.Text, FormatViews(views))
		}
	}
	compare(rust)
	event.HookEventName = "SubagentStop"
	event.AgentID = "synthetic-agent"
	rust, err = run("hook", map[string]any{"event": event})
	if err != nil {
		t.Fatal(err)
	}
	compare(rust)
	var wg sync.WaitGroup
	failures := make(chan error, 2)
	wg.Add(2)
	go func() {
		defer wg.Done()
		failures <- store.RecordReport(binding, Report{TaskID: "go-helper", Status: StatusRunning})
	}()
	go func() {
		defer wg.Done()
		_, err := run("report", map[string]any{"task_id": "rust-helper", "status": StatusCompleted})
		failures <- err
	}()
	wg.Wait()
	close(failures)
	for err := range failures {
		if err != nil {
			t.Fatal(err)
		}
	}
	rust, err = run("read", nil)
	if err != nil {
		t.Fatal(err)
	}
	compare(rust)
	if len(rust[0].Workflows) != 2 {
		t.Fatal("lost workflow during concurrent helpers")
	}
	now = now.Add(8 * 24 * time.Hour)
	rust, err = run("read", nil)
	if err != nil {
		t.Fatal(err)
	}
	compare(rust)
	if len(rust[0].Workflows) != 0 {
		t.Fatal("expired state retained")
	}
}
