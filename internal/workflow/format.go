package workflow

import (
	"errors"
	"fmt"
	"sort"
	"strings"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

type SessionView struct {
	ID        string                 `json:"id"`
	Name      string                 `json:"name"`
	Alias     string                 `json:"alias,omitempty"`
	Summary   *model.WorkflowSummary `json:"summary,omitempty"`
	Workflows []model.Workflow       `json:"workflows"`
}

func SummaryBadge(summary *model.WorkflowSummary) string {
	if summary == nil {
		return ""
	}
	attention := summary.WaitingApproval + summary.WaitingInput + summary.Failed + summary.Interrupted + summary.Stale
	parts := make([]string, 0, 3)
	if summary.Running > 0 {
		parts = append(parts, fmt.Sprintf("%d▶", summary.Running))
	}
	if summary.Completed > 0 {
		parts = append(parts, fmt.Sprintf("%d✓", summary.Completed))
	}
	if attention > 0 {
		parts = append(parts, fmt.Sprintf("%d!", attention))
	}
	return strings.Join(parts, " ")
}

func Views(sessions []model.Session, filter string) ([]SessionView, error) {
	filter = strings.TrimSpace(model.SafeText(filter, 512))
	matched := make([]model.Session, 0, len(sessions))
	for _, session := range sessions {
		if filter != "" && filter != session.ID && filter != session.Name && filter != session.Alias {
			continue
		}
		if len(session.Workflows) == 0 && filter == "" {
			continue
		}
		matched = append(matched, session)
	}
	if filter != "" {
		if len(matched) == 0 {
			return nil, fmt.Errorf("session %q does not exist", filter)
		}
		if len(matched) > 1 {
			return nil, fmt.Errorf("session %q is ambiguous; use its stable ID", filter)
		}
	}
	sort.Slice(matched, func(i, j int) bool {
		left := matched[i].ActivityAt
		if matched[i].Workflow != nil && matched[i].Workflow.UpdatedAt > left {
			left = matched[i].Workflow.UpdatedAt
		}
		right := matched[j].ActivityAt
		if matched[j].Workflow != nil && matched[j].Workflow.UpdatedAt > right {
			right = matched[j].Workflow.UpdatedAt
		}
		if left == right {
			return matched[i].ID < matched[j].ID
		}
		return left > right
	})
	views := make([]SessionView, 0, len(matched))
	for _, session := range matched {
		views = append(views, SessionView{
			ID: session.ID, Name: model.SafeText(session.Name, 512),
			Alias: model.SafeText(session.Alias, 128), Summary: session.Workflow,
			Workflows: append([]model.Workflow(nil), session.Workflows...),
		})
	}
	return views, nil
}

func FormatViews(views []SessionView) string {
	if len(views) == 0 {
		return "No workflow state is available for live tmux sessions.\n"
	}
	var output strings.Builder
	for sessionIndex, session := range views {
		if sessionIndex > 0 {
			output.WriteByte('\n')
		}
		name := session.Name
		if session.Alias != "" {
			name = session.Alias
		}
		fmt.Fprintf(&output, "%s (%s)", name, session.ID)
		if badge := SummaryBadge(session.Summary); badge != "" {
			fmt.Fprintf(&output, "  %s", badge)
		}
		output.WriteByte('\n')
		if len(session.Workflows) == 0 {
			output.WriteString("└─ no recorded workflow\n")
			continue
		}
		for workflowIndex, item := range session.Workflows {
			workflowBranch := "├─"
			nodePrefix := "│  "
			if workflowIndex == len(session.Workflows)-1 {
				workflowBranch = "└─"
				nodePrefix = "   "
			}
			fmt.Fprintf(&output, "%s %s  %s  %s", workflowBranch, shortID(item.ID), item.Source, item.Status)
			if item.Model != "" {
				fmt.Fprintf(&output, "  %s", item.Model)
			}
			fmt.Fprintf(&output, "  updated %s\n", formatTime(item.UpdatedAt))
			for nodeIndex, node := range item.Nodes {
				branch := "├─"
				if nodeIndex == len(item.Nodes)-1 {
					branch = "└─"
				}
				fmt.Fprintf(&output, "%s%s %s  %s  %s  %s\n",
					nodePrefix, branch, shortID(node.ID), node.Type, node.Provider, node.Status)
			}
		}
	}
	return output.String()
}

func ValidateSessionPayload(session model.Session) error {
	if len(session.Workflows) > maxVisibleWorkflows {
		return errors.New("remote workflow count exceeds limit")
	}
	if len(session.Workflows) == 0 {
		if session.Workflow != nil {
			return errors.New("remote workflow summary has no workflow")
		}
		return nil
	}
	if session.Workflow == nil {
		return errors.New("remote workflow summary is missing")
	}
	workflowIDs := make(map[string]struct{}, len(session.Workflows))
	for _, item := range session.Workflows {
		if _, exists := workflowIDs[item.ID]; exists {
			return errors.New("remote workflow identifier is duplicated")
		}
		workflowIDs[item.ID] = struct{}{}
		if !validStatus(item.Status) || len(item.Nodes) == 0 || len(item.Nodes) > maxNodesPerWorkflow ||
			item.StartedAt < 1 || item.UpdatedAt < item.StartedAt {
			return errors.New("remote workflow is invalid")
		}
		switch item.Source {
		case "codex-hook":
			if !validHashedID(item.ID, "wf-") || !validHashedID(item.SessionID, "cx-") ||
				!validHashedID(item.TurnID, "turn-") {
				return errors.New("remote Codex workflow identity is invalid")
			}
		case "codex-orchestra":
			if !validHashedID(item.ID, "orch-") || item.SessionID != "" || item.TurnID != "" {
				return errors.New("remote orchestra workflow identity is invalid")
			}
		default:
			return errors.New("remote workflow source is invalid")
		}
		if model.SafeText(item.Model, 128) != item.Model {
			return errors.New("remote workflow model is unsafe")
		}
		nodeParents := make(map[string]string, len(item.Nodes))
		for _, node := range item.Nodes {
			if !validHashedID(node.ID, "root-", "agent-", "task-") ||
				(node.ParentID != "" && !validHashedID(node.ParentID, "root-", "agent-")) ||
				!validStatus(node.Status) || node.StartedAt < 1 || node.UpdatedAt < node.StartedAt ||
				model.SafeText(node.Type, 128) != node.Type || model.SafeText(node.Provider, 64) != node.Provider {
				return errors.New("remote workflow node is invalid")
			}
			if _, exists := nodeParents[node.ID]; exists {
				return errors.New("remote workflow node identifier is duplicated")
			}
			nodeParents[node.ID] = node.ParentID
		}
		for nodeID, parentID := range nodeParents {
			if parentID == "" {
				continue
			}
			if _, exists := nodeParents[parentID]; !exists {
				return errors.New("remote workflow node parent is missing")
			}
			seen := map[string]struct{}{nodeID: {}}
			for current := parentID; current != ""; current = nodeParents[current] {
				if _, exists := seen[current]; exists {
					return errors.New("remote workflow node ancestry is cyclic")
				}
				seen[current] = struct{}{}
			}
		}
		if statusFromNodes(item.Nodes) != item.Status {
			return errors.New("remote workflow status does not match its nodes")
		}
	}
	selected := make([]model.Workflow, 0, len(session.Workflows))
	for _, item := range session.Workflows {
		if activeStatus(item.Status) {
			selected = append(selected, item)
		}
	}
	if len(selected) == 0 {
		selected = session.Workflows[:1]
	}
	expected := summarize(selected)
	if expected != *session.Workflow {
		return errors.New("remote workflow summary does not match its nodes")
	}
	return nil
}

func validHashedID(value string, prefixes ...string) bool {
	prefix := ""
	for _, candidate := range prefixes {
		if strings.HasPrefix(value, candidate) {
			prefix = candidate
			break
		}
	}
	if prefix == "" || len(value) != len(prefix)+32 {
		return false
	}
	for _, character := range value[len(prefix):] {
		if (character < '0' || character > '9') && (character < 'a' || character > 'f') {
			return false
		}
	}
	return true
}

func shortID(value string) string {
	if len(value) <= 15 {
		return value
	}
	return value[:15]
}

func formatTime(timestamp int64) string {
	if timestamp < 1 {
		return "unknown"
	}
	return time.Unix(timestamp, 0).Local().Format("2006-01-02 15:04:05")
}
