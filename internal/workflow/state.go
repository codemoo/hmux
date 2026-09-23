package workflow

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"sort"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/codemoo/hmux/internal/model"
)

func validateState(current state) error {
	if current.Version != stateVersion || current.Workflows == nil {
		return errors.New("invalid workflow state version")
	}
	if len(current.Workflows) > maxWorkflows {
		return errors.New("workflow count exceeds limit")
	}
	for id, item := range current.Workflows {
		if id != item.ID || !strings.HasPrefix(id, "wf-") && !strings.HasPrefix(id, "orch-") {
			return errors.New("workflow state key mismatch")
		}
		if err := validateBinding(Binding{SessionID: item.TMUXSessionID, CreatedAt: item.TMUXCreatedAt}); err != nil {
			return err
		}
		if item.Source != "codex-hook" && item.Source != "codex-orchestra" {
			return errors.New("invalid workflow source")
		}
		if item.Source == "codex-hook" &&
			(!validHashedID(item.SessionID, "cx-") || !validHashedID(item.TurnID, "turn-")) {
			return errors.New("invalid Codex workflow identity hash")
		}
		if item.Source == "codex-orchestra" && (item.SessionID != "" || item.TurnID != "") {
			return errors.New("detached workflow contains an unexpected provider identity")
		}
		if model.SafeText(item.Model, 128) != item.Model {
			return errors.New("unsafe workflow model metadata")
		}
		if !validStatus(item.Status) || item.StartedAt < 1 || item.UpdatedAt < item.StartedAt || len(item.Nodes) > maxNodesPerWorkflow {
			return errors.New("invalid workflow lifecycle")
		}
		for nodeID, node := range item.Nodes {
			if nodeID != node.ID || !validHashedID(node.ID, "root-", "agent-", "task-") ||
				(node.ParentID != "" && !validHashedID(node.ParentID, "root-", "agent-")) ||
				!validStatus(node.Status) || node.StartedAt < 1 || node.UpdatedAt < node.StartedAt {
				return errors.New("invalid workflow node")
			}
			if model.SafeText(node.Type, 128) != node.Type || model.SafeText(node.Provider, 64) != node.Provider {
				return errors.New("unsafe workflow node metadata")
			}
			if node.ParentID != "" {
				if _, exists := item.Nodes[node.ParentID]; !exists {
					return errors.New("workflow node references an absent parent")
				}
			}
		}
	}
	return nil
}

func validateBinding(binding Binding) error {
	if err := model.ValidateSessionID(binding.SessionID); err != nil {
		return err
	}
	if binding.CreatedAt < 1 {
		return errors.New("tmux session creation time is invalid")
	}
	return nil
}

func validateOpaque(name, value string, maximum int) error {
	if value == "" || len(value) > maximum || !utf8.ValidString(value) || strings.TrimSpace(value) != value || model.SafeText(value, maximum) != value {
		return fmt.Errorf("%s is invalid", name)
	}
	return nil
}

func sanitizeLabel(value string, maximum int, fallback string) string {
	value = model.SafeText(value, maximum)
	if value == "" {
		return fallback
	}
	return value
}

func hashID(prefix, value string) string {
	digest := sha256.Sum256([]byte(prefix + "\x00" + value))
	return prefix + hex.EncodeToString(digest[:16])
}

func setNode(item *storedWorkflow, id, parentID, nodeType, provider, status string, now int64, force bool) {
	node, exists := item.Nodes[id]
	if !exists {
		node = model.WorkflowNode{ID: id, ParentID: parentID, Type: nodeType, Provider: provider, StartedAt: now}
	}
	if exists && !force && terminalStatus(node.Status) && node.Status != StatusStale {
		return
	}
	node.Status = status
	node.UpdatedAt = now
	if terminalStatus(status) {
		node.EndedAt = now
	} else {
		node.EndedAt = 0
	}
	item.Nodes[id] = node
	item.UpdatedAt = now
}

func ensureNodeCapacity(item *storedWorkflow, wantedID string) error {
	if _, exists := item.Nodes[wantedID]; exists {
		return nil
	}
	if len(item.Nodes) < maxNodesPerWorkflow {
		return nil
	}
	oldestID := ""
	oldest := int64(0)
	for id, node := range item.Nodes {
		if !terminalStatus(node.Status) || node.Type == "root" {
			continue
		}
		if oldestID == "" || node.UpdatedAt < oldest {
			oldestID, oldest = id, node.UpdatedAt
		}
	}
	if oldestID == "" {
		return errors.New("workflow node count exceeds limit")
	}
	delete(item.Nodes, oldestID)
	return nil
}

func refreshWorkflowStatus(item *storedWorkflow, now int64) {
	nodes := make([]model.WorkflowNode, 0, len(item.Nodes))
	for _, node := range item.Nodes {
		nodes = append(nodes, node)
	}
	item.Status = statusFromNodes(nodes)
	item.UpdatedAt = now
	if terminalStatus(item.Status) {
		item.EndedAt = now
	} else {
		item.EndedAt = 0
	}
}

func statusFromNodes(nodes []model.WorkflowNode) string {
	counts := map[string]int{}
	for _, node := range nodes {
		counts[node.Status]++
	}
	for _, status := range []string{StatusWaitingApproval, StatusWaitingInput, StatusRunning, StatusFailed, StatusStale, StatusInterrupted} {
		if counts[status] > 0 {
			return status
		}
	}
	return StatusCompleted
}

func interruptActive(item *storedWorkflow, now int64) {
	for id, node := range item.Nodes {
		if activeStatus(node.Status) {
			node.Status = StatusInterrupted
			node.UpdatedAt = now
			node.EndedAt = now
			item.Nodes[id] = node
		}
	}
	refreshWorkflowStatus(item, now)
}

func needsPrune(current state, now time.Time) bool {
	staleCutoff := now.Add(-staleAfter).Unix()
	retentionCutoff := now.Add(-terminalRetention).Unix()
	if len(current.Workflows) > maxWorkflows {
		return true
	}
	for _, item := range current.Workflows {
		if activeStatus(item.Status) && item.UpdatedAt < staleCutoff ||
			terminalStatus(item.Status) && item.UpdatedAt < retentionCutoff {
			return true
		}
	}
	return false
}

func prune(current *state, now time.Time) bool {
	changed := false
	staleCutoff := now.Add(-staleAfter).Unix()
	for id, item := range current.Workflows {
		if activeStatus(item.Status) && item.UpdatedAt < staleCutoff {
			for nodeID, node := range item.Nodes {
				if activeStatus(node.Status) {
					node.Status = StatusStale
					node.EndedAt = node.UpdatedAt
					item.Nodes[nodeID] = node
				}
			}
			item.Status = StatusStale
			item.EndedAt = item.UpdatedAt
			current.Workflows[id] = item
			changed = true
		}
	}
	cutoff := now.Add(-terminalRetention).Unix()
	for id, item := range current.Workflows {
		if terminalStatus(item.Status) && item.UpdatedAt < cutoff {
			delete(current.Workflows, id)
			changed = true
		}
	}
	if len(current.Workflows) <= maxWorkflows {
		return changed
	}
	type candidate struct {
		id      string
		updated int64
	}
	items := make([]candidate, 0, len(current.Workflows))
	for id, item := range current.Workflows {
		items = append(items, candidate{id: id, updated: item.UpdatedAt})
	}
	sort.Slice(items, func(i, j int) bool {
		if items[i].updated == items[j].updated {
			return items[i].id < items[j].id
		}
		return items[i].updated < items[j].updated
	})
	for len(current.Workflows) > maxWorkflows {
		delete(current.Workflows, items[0].id)
		items = items[1:]
		changed = true
	}
	return changed
}

func pruneToSize(current *state, maximum int) (bool, error) {
	if maximum < 1 {
		return false, errors.New("workflow state size limit is invalid")
	}
	changed := false
	for {
		data, err := json.Marshal(current)
		if err != nil {
			return changed, err
		}
		if len(data)+1 <= maximum {
			return changed, nil
		}
		if len(current.Workflows) == 0 {
			return changed, errors.New("workflow state cannot fit within size limit")
		}
		oldestID := ""
		oldestUpdated := int64(0)
		oldestTerminal := false
		for id, item := range current.Workflows {
			terminal := terminalStatus(item.Status)
			if oldestID == "" || terminal && !oldestTerminal ||
				terminal == oldestTerminal && (item.UpdatedAt < oldestUpdated ||
					item.UpdatedAt == oldestUpdated && id < oldestID) {
				oldestID = id
				oldestUpdated = item.UpdatedAt
				oldestTerminal = terminal
			}
		}
		delete(current.Workflows, oldestID)
		changed = true
	}
}

func isUserInputTool(name string) bool {
	switch strings.ToLower(strings.TrimSpace(name)) {
	case "request_user_input", "askuserquestion", "ask_user_question":
		return true
	default:
		return false
	}
}

func activeStatus(status string) bool {
	return status == StatusRunning || status == StatusWaitingApproval || status == StatusWaitingInput
}

func terminalStatus(status string) bool {
	return status == StatusCompleted || status == StatusFailed || status == StatusInterrupted || status == StatusStale
}

func validReportStatus(status string) bool {
	return status == StatusRunning || status == StatusCompleted || status == StatusFailed || status == StatusInterrupted
}

func validStatus(status string) bool {
	return activeStatus(status) || terminalStatus(status)
}
