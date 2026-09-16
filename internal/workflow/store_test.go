package workflow

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/model"
	"golang.org/x/sys/unix"
)

func TestParseHookSelectsLifecycleFieldsAndRejectsOversize(t *testing.T) {
	event, err := ParseHook(strings.NewReader(`{
  "session_id":"session-secret-id",
  "turn_id":"turn-secret-id",
  "hook_event_name":"SubagentStart",
  "model":"gpt-test",
  "agent_id":"agent-secret-id",
  "agent_type":"reviewer",
  "prompt":"must-not-persist",
  "transcript_path":"/private/transcript",
  "tool_input":{"command":"must-not-persist"}
}`))
	if err != nil {
		t.Fatal(err)
	}
	if event.AgentType != "reviewer" || event.AgentID != "agent-secret-id" {
		t.Fatalf("event=%#v", event)
	}
	if _, err := ParseHook(bytes.NewReader(bytes.Repeat([]byte("x"), maxHookInput+1))); err == nil {
		t.Fatal("oversized hook input was accepted")
	}
}

func TestStoreLifecycleApplyAndNoSensitivePersistence(t *testing.T) {
	now := time.Unix(1_800_000_000, 0).UTC()
	store := Store{StateDir: t.TempDir(), Now: func() time.Time { return now }}
	binding := Binding{SessionID: "$7", CreatedAt: 1_700_000_000}
	base := HookEvent{SessionID: "session-secret-id", TurnID: "turn-secret-id", Model: "gpt-test"}

	record := func(name, agentID string) {
		t.Helper()
		event := base
		event.HookEventName = name
		event.AgentID = agentID
		event.AgentType = "reviewer"
		if err := store.RecordHook(binding, event); err != nil {
			t.Fatal(err)
		}
		now = now.Add(time.Second)
	}
	record("UserPromptSubmit", "")
	record("SubagentStart", "agent-secret-one")
	record("SubagentStart", "agent-secret-two")
	record("SubagentStop", "agent-secret-one")
	permission := base
	permission.HookEventName = "PermissionRequest"
	if err := store.RecordHook(binding, permission); err != nil {
		t.Fatal(err)
	}

	catalog := model.Catalog{Sessions: []model.Session{{ID: "$7", CreatedAt: binding.CreatedAt}}}
	if err := store.Apply(&catalog); err != nil {
		t.Fatal(err)
	}
	summary := catalog.Sessions[0].Workflow
	if summary == nil || summary.Running != 1 || summary.WaitingApproval != 1 || summary.Completed != 1 {
		t.Fatalf("summary=%#v", summary)
	}
	if len(catalog.Sessions[0].Workflows) != 1 || len(catalog.Sessions[0].Workflows[0].Nodes) != 3 {
		t.Fatalf("workflows=%#v", catalog.Sessions[0].Workflows)
	}

	data, err := os.ReadFile(filepath.Join(store.StateDir, "workflows", "state.json"))
	if err != nil {
		t.Fatal(err)
	}
	for _, forbidden := range []string{"session-secret-id", "turn-secret-id", "agent-secret-one", "agent-secret-two", "must-not-persist", "transcript"} {
		if strings.Contains(string(data), forbidden) {
			t.Fatalf("sensitive value %q persisted in %s", forbidden, data)
		}
	}

	stop := base
	stop.HookEventName = "Stop"
	if err := store.RecordHook(binding, stop); err != nil {
		t.Fatal(err)
	}
	catalog = model.Catalog{Sessions: []model.Session{{ID: "$7", CreatedAt: binding.CreatedAt}}}
	if err := store.Apply(&catalog); err != nil {
		t.Fatal(err)
	}
	summary = catalog.Sessions[0].Workflow
	if summary.Completed != 2 || summary.Interrupted != 1 || summary.Running != 0 {
		t.Fatalf("terminal summary=%#v", summary)
	}
}

func TestStoreWaitingInputStaleAndSessionIdentity(t *testing.T) {
	now := time.Unix(1_800_000_000, 0).UTC()
	store := Store{StateDir: t.TempDir(), Now: func() time.Time { return now }}
	binding := Binding{SessionID: "$3", CreatedAt: 1_700_000_000}
	event := HookEvent{SessionID: "session", TurnID: "turn", HookEventName: "PreToolUse", ToolName: "request_user_input"}
	if err := store.RecordHook(binding, event); err != nil {
		t.Fatal(err)
	}
	catalog := model.Catalog{Sessions: []model.Session{
		{ID: "$3", CreatedAt: binding.CreatedAt},
		{ID: "$3", CreatedAt: binding.CreatedAt + 1},
	}}
	if err := store.Apply(&catalog); err != nil {
		t.Fatal(err)
	}
	if catalog.Sessions[0].Workflow == nil || catalog.Sessions[0].Workflow.WaitingInput != 1 {
		t.Fatalf("waiting summary=%#v", catalog.Sessions[0].Workflow)
	}
	if catalog.Sessions[1].Workflow != nil {
		t.Fatal("workflow leaked to a reused tmux stable ID")
	}

	now = now.Add(staleAfter + time.Second)
	catalog = model.Catalog{Sessions: []model.Session{{ID: "$3", CreatedAt: binding.CreatedAt}}}
	if err := store.Apply(&catalog); err != nil {
		t.Fatal(err)
	}
	if catalog.Sessions[0].Workflow.Stale != 1 || catalog.Sessions[0].Workflows[0].Status != StatusStale {
		t.Fatalf("stale workflow=%#v", catalog.Sessions[0])
	}
}

func TestStoreAcceptsSubagentStopAfterMissedStart(t *testing.T) {
	store := Store{StateDir: t.TempDir(), Now: func() time.Time {
		return time.Unix(1_800_000_000, 0).UTC()
	}}
	binding := Binding{SessionID: "$4", CreatedAt: 1_700_000_000}
	event := HookEvent{
		SessionID: "session", TurnID: "turn", HookEventName: "SubagentStop",
		AgentID: "agent", AgentType: "reviewer",
	}
	if err := store.RecordHook(binding, event); err != nil {
		t.Fatal(err)
	}
	catalog := model.Catalog{Sessions: []model.Session{{ID: "$4", CreatedAt: binding.CreatedAt}}}
	if err := store.Apply(&catalog); err != nil {
		t.Fatal(err)
	}
	if len(catalog.Sessions[0].Workflows) != 1 || len(catalog.Sessions[0].Workflows[0].Nodes) != 2 {
		t.Fatalf("workflow=%#v", catalog.Sessions[0].Workflows)
	}
	if summary := catalog.Sessions[0].Workflow; summary == nil || summary.Running != 1 || summary.Completed != 1 {
		t.Fatalf("summary=%#v", summary)
	}
}

func TestApplyPrunesExpiredTerminalWorkflowWithoutAnotherHookWrite(t *testing.T) {
	now := time.Unix(1_800_000_000, 0).UTC()
	store := Store{StateDir: t.TempDir(), Now: func() time.Time { return now }}
	binding := Binding{SessionID: "$5", CreatedAt: 1_700_000_000}
	base := HookEvent{SessionID: "session", TurnID: "turn"}
	base.HookEventName = "UserPromptSubmit"
	if err := store.RecordHook(binding, base); err != nil {
		t.Fatal(err)
	}
	base.HookEventName = "Stop"
	if err := store.RecordHook(binding, base); err != nil {
		t.Fatal(err)
	}
	now = now.Add(terminalRetention + time.Second)
	catalog := model.Catalog{Sessions: []model.Session{{ID: "$5", CreatedAt: binding.CreatedAt}}}
	if err := store.Apply(&catalog); err != nil {
		t.Fatal(err)
	}
	if catalog.Sessions[0].Workflow != nil || len(catalog.Sessions[0].Workflows) != 0 {
		t.Fatalf("expired workflow remained visible: %#v", catalog.Sessions[0])
	}
	current, err := store.read()
	if err != nil {
		t.Fatal(err)
	}
	if len(current.Workflows) != 0 {
		t.Fatalf("expired workflow remained stored: %#v", current.Workflows)
	}
}

func TestPruneToSizeEvictsOldTerminalWorkflowFirst(t *testing.T) {
	binding := Binding{SessionID: "$6", CreatedAt: 1_700_000_000}
	makeItem := func(key string, status string, updated int64) storedWorkflow {
		workflowID := hashID("orch-", key)
		nodeID := hashID("task-", key)
		return storedWorkflow{
			ID: workflowID, TMUXSessionID: binding.SessionID,
			TMUXCreatedAt: binding.CreatedAt, Source: "codex-orchestra",
			Status: status, StartedAt: updated, UpdatedAt: updated,
			Nodes: map[string]model.WorkflowNode{nodeID: {
				ID: nodeID, Type: "task", Provider: "detached-codex",
				Status: status, StartedAt: updated, UpdatedAt: updated,
			}},
		}
	}
	terminal := makeItem("terminal", StatusCompleted, 100)
	active := makeItem("active", StatusRunning, 200)
	current := state{Version: stateVersion, UpdatedAt: "test", Workflows: map[string]storedWorkflow{
		terminal.ID: terminal,
		active.ID:   active,
	}}
	withoutTerminal := state{Version: stateVersion, UpdatedAt: "test", Workflows: map[string]storedWorkflow{
		active.ID: active,
	}}
	data, err := json.Marshal(withoutTerminal)
	if err != nil {
		t.Fatal(err)
	}
	changed, err := pruneToSize(&current, len(data)+1)
	if err != nil {
		t.Fatal(err)
	}
	if !changed {
		t.Fatal("oversized workflow state was not pruned")
	}
	if _, exists := current.Workflows[terminal.ID]; exists {
		t.Fatal("old terminal workflow was not evicted first")
	}
	if _, exists := current.Workflows[active.ID]; !exists {
		t.Fatal("active workflow was evicted before terminal history")
	}
	if err := validateState(current); err != nil {
		t.Fatal(err)
	}
}

func TestStoreDetachedReportsAndConcurrentUpdates(t *testing.T) {
	now := time.Unix(1_800_000_000, 0).UTC()
	store := Store{StateDir: t.TempDir(), Now: func() time.Time { return now }}
	binding := Binding{SessionID: "$11", CreatedAt: 1_700_000_000}
	if err := store.RecordReport(binding, Report{TaskID: "review-1", Status: StatusRunning}); err != nil {
		t.Fatal(err)
	}
	if err := store.RecordReport(binding, Report{TaskID: "review-1", Status: StatusCompleted}); err != nil {
		t.Fatal(err)
	}

	var group sync.WaitGroup
	for index := 0; index < 24; index++ {
		group.Add(1)
		go func(index int) {
			defer group.Done()
			event := HookEvent{
				SessionID: "session", TurnID: "turn", HookEventName: "SubagentStart",
				AgentID: "agent-" + string(rune('a'+index)), AgentType: "worker",
			}
			if err := store.RecordHook(binding, event); err != nil {
				t.Errorf("record %d: %v", index, err)
			}
		}(index)
	}
	group.Wait()
	catalog := model.Catalog{Sessions: []model.Session{{ID: "$11", CreatedAt: binding.CreatedAt}}}
	if err := store.Apply(&catalog); err != nil {
		t.Fatal(err)
	}
	if catalog.Sessions[0].Workflow == nil || catalog.Sessions[0].Workflow.Running != 25 {
		t.Fatalf("concurrent summary=%#v", catalog.Sessions[0].Workflow)
	}
}

func TestStoreRejectsSymlinkedStateTarget(t *testing.T) {
	root := t.TempDir()
	store := Store{StateDir: root}
	binding := Binding{SessionID: "$1", CreatedAt: 1_700_000_000}
	event := HookEvent{SessionID: "session", TurnID: "turn", HookEventName: "UserPromptSubmit"}
	if err := store.RecordHook(binding, event); err != nil {
		t.Fatal(err)
	}
	statePath := filepath.Join(root, "workflows", "state.json")
	if err := os.Remove(statePath); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(filepath.Join(t.TempDir(), "outside"), statePath); err != nil {
		t.Fatal(err)
	}
	if err := store.RecordHook(binding, event); err == nil {
		t.Fatal("symlinked workflow state target was accepted")
	}
}

func TestStoreFailsOpenWhenWorkflowLockStaysBusy(t *testing.T) {
	root := t.TempDir()
	store := Store{StateDir: root, lockWait: 25 * time.Millisecond}
	if err := store.ensureRoot(); err != nil {
		t.Fatal(err)
	}
	lock, err := openLock(filepath.Join(store.root(), "state.lock"))
	if err != nil {
		t.Fatal(err)
	}
	defer lock.Close()
	if err := unix.Flock(int(lock.Fd()), unix.LOCK_EX); err != nil {
		t.Fatal(err)
	}
	defer unix.Flock(int(lock.Fd()), unix.LOCK_UN) //nolint:errcheck

	started := time.Now()
	err = store.RecordHook(
		Binding{SessionID: "$1", CreatedAt: 1_700_000_000},
		HookEvent{SessionID: "session", TurnID: "turn", HookEventName: "UserPromptSubmit"},
	)
	if err == nil || !strings.Contains(err.Error(), "lock is busy") {
		t.Fatalf("error=%v", err)
	}
	if elapsed := time.Since(started); elapsed > 500*time.Millisecond {
		t.Fatalf("busy workflow lock was not bounded: %s", elapsed)
	}
}

func TestSummaryBadgeViewsAndPayloadValidation(t *testing.T) {
	summary := &model.WorkflowSummary{Running: 2, Completed: 1, WaitingInput: 1}
	if got := SummaryBadge(summary); got != "2▶ 1✓ 1!" {
		t.Fatalf("badge=%q", got)
	}
	item := model.Workflow{
		ID: "wf-0123456789abcdef0123456789abcdef", Source: "codex-hook",
		SessionID: "cx-0123456789abcdef0123456789abcdef",
		TurnID:    "turn-0123456789abcdef0123456789abcdef",
		Status:    StatusRunning, StartedAt: 100, UpdatedAt: 101,
		Nodes: []model.WorkflowNode{{
			ID: "root-0123456789abcdef0123456789abcdef", Type: "root",
			Provider: "codex", Status: StatusRunning, StartedAt: 100, UpdatedAt: 101,
		}},
	}
	payloadSummary := &model.WorkflowSummary{Running: 1, UpdatedAt: 101}
	session := model.Session{ID: "$1", Name: "native", Alias: "display", Workflow: payloadSummary, Workflows: []model.Workflow{item}}
	if err := ValidateSessionPayload(session); err != nil {
		t.Fatal(err)
	}
	views, err := Views([]model.Session{session}, "display")
	if err != nil || len(views) != 1 {
		t.Fatalf("views=%#v err=%v", views, err)
	}
	if text := FormatViews(views); !strings.Contains(text, "1▶") || !strings.Contains(text, "codex-hook") {
		t.Fatalf("tree=%q", text)
	}
	session.Workflow = summary
	if err := ValidateSessionPayload(session); err == nil {
		t.Fatal("inconsistent remote workflow summary was accepted")
	}
	session.Workflow = payloadSummary
	session.Workflows[0].Status = StatusCompleted
	if err := ValidateSessionPayload(session); err == nil {
		t.Fatal("remote workflow status inconsistent with its nodes was accepted")
	}
	session.Workflows[0].Status = StatusRunning
	session.Workflows[0].ID = "raw-provider-id"
	if err := ValidateSessionPayload(session); err == nil {
		t.Fatal("unhashed remote workflow identifier was accepted")
	}
}

func TestValidateSessionPayloadRejectsInvalidWorkflowTopology(t *testing.T) {
	validSession := func() model.Session {
		item := model.Workflow{
			ID: "wf-0123456789abcdef0123456789abcdef", Source: "codex-hook",
			SessionID: "cx-0123456789abcdef0123456789abcdef",
			TurnID:    "turn-0123456789abcdef0123456789abcdef",
			Status:    StatusRunning, StartedAt: 100, UpdatedAt: 101,
			Nodes: []model.WorkflowNode{{
				ID: "root-0123456789abcdef0123456789abcdef", Type: "root",
				Provider: "codex", Status: StatusRunning, StartedAt: 100, UpdatedAt: 101,
			}},
		}
		return model.Session{
			ID: "$1", CreatedAt: 1_700_000_000,
			Workflow:  &model.WorkflowSummary{Running: 1, UpdatedAt: 101},
			Workflows: []model.Workflow{item},
		}
	}

	t.Run("duplicate workflow identifier", func(t *testing.T) {
		session := validSession()
		session.Workflows = append(session.Workflows, session.Workflows[0])
		if err := ValidateSessionPayload(session); err == nil || !strings.Contains(err.Error(), "duplicated") {
			t.Fatalf("error=%v", err)
		}
	})

	t.Run("duplicate node identifier", func(t *testing.T) {
		session := validSession()
		session.Workflows[0].Nodes = append(session.Workflows[0].Nodes, session.Workflows[0].Nodes[0])
		if err := ValidateSessionPayload(session); err == nil || !strings.Contains(err.Error(), "duplicated") {
			t.Fatalf("error=%v", err)
		}
	})

	t.Run("missing parent", func(t *testing.T) {
		session := validSession()
		session.Workflows[0].Nodes[0].ParentID = "agent-11111111111111111111111111111111"
		if err := ValidateSessionPayload(session); err == nil || !strings.Contains(err.Error(), "parent is missing") {
			t.Fatalf("error=%v", err)
		}
	})

	t.Run("ancestry cycle", func(t *testing.T) {
		session := validSession()
		rootID := session.Workflows[0].Nodes[0].ID
		agentID := "agent-11111111111111111111111111111111"
		session.Workflows[0].Nodes[0].ParentID = agentID
		session.Workflows[0].Nodes = append(session.Workflows[0].Nodes, model.WorkflowNode{
			ID: agentID, ParentID: rootID, Type: "agent", Provider: "codex",
			Status: StatusRunning, StartedAt: 100, UpdatedAt: 101,
		})
		if err := ValidateSessionPayload(session); err == nil || !strings.Contains(err.Error(), "cyclic") {
			t.Fatalf("error=%v", err)
		}
	})
}
