package workflow

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strconv"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

// HookEvent intentionally models only lifecycle metadata. Unknown official
// hook fields are accepted for forward compatibility and discarded.
type HookEvent struct {
	SessionID     string `json:"session_id"`
	TurnID        string `json:"turn_id"`
	HookEventName string `json:"hook_event_name"`
	Model         string `json:"model"`
	AgentID       string `json:"agent_id"`
	AgentType     string `json:"agent_type"`
	ToolName      string `json:"tool_name"`
}

type Report struct {
	TaskID string
	Status string
}

func ParseHook(reader io.Reader) (HookEvent, error) {
	var event HookEvent
	data, err := io.ReadAll(io.LimitReader(reader, maxHookInput+1))
	if err != nil {
		return event, err
	}
	if len(data) == 0 || len(data) > maxHookInput {
		return event, errors.New("hook input has an invalid size")
	}
	if err := json.Unmarshal(data, &event); err != nil {
		return event, fmt.Errorf("decode hook input: %w", err)
	}
	if err := validateOpaque("session_id", event.SessionID, 512); err != nil {
		return event, err
	}
	if event.HookEventName != "SessionEnd" {
		if err := validateOpaque("turn_id", event.TurnID, 512); err != nil {
			return event, err
		}
	}
	switch event.HookEventName {
	case "UserPromptSubmit", "SubagentStart", "SubagentStop", "PermissionRequest", "PreToolUse", "PostToolUse", "Stop", "SessionEnd":
	default:
		return event, fmt.Errorf("unsupported hook event %q", model.SafeText(event.HookEventName, 64))
	}
	if event.HookEventName == "SubagentStart" || event.HookEventName == "SubagentStop" {
		if err := validateOpaque("agent_id", event.AgentID, 512); err != nil {
			return event, err
		}
	}
	if len(event.Model) > 256 || len(event.AgentType) > 256 || len(event.ToolName) > 256 {
		return event, errors.New("hook metadata exceeds limit")
	}
	return event, nil
}

func (s Store) RecordHook(binding Binding, event HookEvent) error {
	if err := validateBinding(binding); err != nil {
		return err
	}
	if err := validateOpaque("session_id", event.SessionID, 512); err != nil {
		return err
	}
	if event.HookEventName == "SessionEnd" {
		return s.update(func(current *state, now time.Time) error {
			sessionID := hashID("cx-", event.SessionID)
			for id, item := range current.Workflows {
				if item.TMUXSessionID != binding.SessionID || item.TMUXCreatedAt != binding.CreatedAt || item.SessionID != sessionID {
					continue
				}
				interruptActive(&item, now.Unix())
				current.Workflows[id] = item
			}
			return nil
		})
	}
	if err := validateOpaque("turn_id", event.TurnID, 512); err != nil {
		return err
	}
	return s.update(func(current *state, now time.Time) error {
		unixNow := now.Unix()
		workflowID := hashID("wf-", event.SessionID+"\x00"+event.TurnID)
		sessionID := hashID("cx-", event.SessionID)
		turnID := hashID("turn-", event.TurnID)
		if event.HookEventName == "UserPromptSubmit" {
			for id, item := range current.Workflows {
				if id == workflowID || item.Source != "codex-hook" || item.SessionID != sessionID ||
					item.TMUXSessionID != binding.SessionID || item.TMUXCreatedAt != binding.CreatedAt || !activeStatus(item.Status) {
					continue
				}
				interruptActive(&item, unixNow)
				current.Workflows[id] = item
			}
		}
		item, exists := current.Workflows[workflowID]
		if !exists {
			item = storedWorkflow{
				ID: workflowID, TMUXSessionID: binding.SessionID, TMUXCreatedAt: binding.CreatedAt,
				Source: "codex-hook", SessionID: sessionID, TurnID: turnID,
				Status: StatusRunning, StartedAt: unixNow, UpdatedAt: unixNow,
				Nodes: map[string]model.WorkflowNode{},
			}
		}
		if item.TMUXSessionID != binding.SessionID || item.TMUXCreatedAt != binding.CreatedAt ||
			item.SessionID != sessionID || item.TurnID != turnID || item.Source != "codex-hook" {
			return errors.New("workflow identity collision")
		}
		if modelName := sanitizeLabel(event.Model, 128, ""); modelName != "" {
			item.Model = modelName
		}
		rootID := hashID("root-", event.SessionID)
		switch event.HookEventName {
		case "UserPromptSubmit":
			setNode(&item, rootID, "", "root", "codex", StatusRunning, unixNow, true)
		case "SubagentStart":
			setNode(&item, rootID, "", "root", "codex", StatusRunning, unixNow, false)
			agentID := hashID("agent-", event.AgentID)
			if err := ensureNodeCapacity(&item, agentID); err != nil {
				return err
			}
			setNode(&item, agentID, rootID, sanitizeLabel(event.AgentType, 128, "subagent"), "native", StatusRunning, unixNow, true)
		case "SubagentStop":
			setNode(&item, rootID, "", "root", "codex", StatusRunning, unixNow, false)
			agentID := hashID("agent-", event.AgentID)
			if err := ensureNodeCapacity(&item, agentID); err != nil {
				return err
			}
			setNode(&item, agentID, rootID, sanitizeLabel(event.AgentType, 128, "subagent"), "native", StatusCompleted, unixNow, true)
		case "PermissionRequest":
			setNode(&item, rootID, "", "root", "codex", StatusWaitingApproval, unixNow, false)
		case "PreToolUse":
			status := StatusRunning
			if isUserInputTool(event.ToolName) {
				status = StatusWaitingInput
			}
			setNode(&item, rootID, "", "root", "codex", status, unixNow, false)
		case "PostToolUse":
			setNode(&item, rootID, "", "root", "codex", StatusRunning, unixNow, false)
		case "Stop":
			setNode(&item, rootID, "", "root", "codex", StatusCompleted, unixNow, true)
			for id, node := range item.Nodes {
				if id != rootID && activeStatus(node.Status) {
					node.Status = StatusInterrupted
					node.UpdatedAt = unixNow
					node.EndedAt = unixNow
					item.Nodes[id] = node
				}
			}
		}
		refreshWorkflowStatus(&item, unixNow)
		current.Workflows[workflowID] = item
		return nil
	})
}

func (s Store) RecordReport(binding Binding, report Report) error {
	if err := validateBinding(binding); err != nil {
		return err
	}
	if err := validateOpaque("task_id", report.TaskID, 256); err != nil {
		return err
	}
	if !validReportStatus(report.Status) {
		return fmt.Errorf("invalid report status %q", model.SafeText(report.Status, 64))
	}
	return s.update(func(current *state, now time.Time) error {
		unixNow := now.Unix()
		workflowID := hashID("orch-", binding.SessionID+"\x00"+strconv.FormatInt(binding.CreatedAt, 10))
		item, exists := current.Workflows[workflowID]
		if !exists {
			item = storedWorkflow{
				ID: workflowID, TMUXSessionID: binding.SessionID, TMUXCreatedAt: binding.CreatedAt,
				Source: "codex-orchestra", Status: report.Status,
				StartedAt: unixNow, UpdatedAt: unixNow, Nodes: map[string]model.WorkflowNode{},
			}
		}
		if item.Source != "codex-orchestra" || item.TMUXSessionID != binding.SessionID || item.TMUXCreatedAt != binding.CreatedAt {
			return errors.New("orchestra workflow identity collision")
		}
		taskID := hashID("task-", report.TaskID)
		if err := ensureNodeCapacity(&item, taskID); err != nil {
			return err
		}
		setNode(&item, taskID, "", "task", "detached-codex", report.Status, unixNow, true)
		refreshWorkflowStatus(&item, unixNow)
		current.Workflows[workflowID] = item
		return nil
	})
}
