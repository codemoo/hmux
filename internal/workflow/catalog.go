package workflow

import (
	"errors"
	"os"
	"sort"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

func (s Store) Apply(value *model.Catalog) error {
	if value == nil {
		return errors.New("catalog is nil")
	}
	current, err := s.readPruned()
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	now := s.currentTime()
	for index := range value.Sessions {
		session := &value.Sessions[index]
		items := make([]model.Workflow, 0)
		for _, stored := range current.Workflows {
			if stored.TMUXSessionID == session.ID && stored.TMUXCreatedAt == session.CreatedAt {
				items = append(items, visibleWorkflow(stored, now))
			}
		}
		sort.Slice(items, func(i, j int) bool {
			if items[i].UpdatedAt == items[j].UpdatedAt {
				return items[i].ID < items[j].ID
			}
			return items[i].UpdatedAt > items[j].UpdatedAt
		})
		if len(items) > maxVisibleWorkflows {
			items = items[:maxVisibleWorkflows]
		}
		if len(items) == 0 {
			continue
		}
		session.Workflows = items
		selected := make([]model.Workflow, 0, len(items))
		for _, item := range items {
			if activeStatus(item.Status) {
				selected = append(selected, item)
			}
		}
		if len(selected) == 0 {
			selected = items[:1]
		}
		summary := summarize(selected)
		session.Workflow = &summary
	}
	return nil
}

func summarize(items []model.Workflow) model.WorkflowSummary {
	var result model.WorkflowSummary
	for _, item := range items {
		if item.UpdatedAt > result.UpdatedAt {
			result.UpdatedAt = item.UpdatedAt
		}
		for _, node := range item.Nodes {
			switch node.Status {
			case StatusRunning:
				result.Running++
			case StatusWaitingApproval:
				result.WaitingApproval++
			case StatusWaitingInput:
				result.WaitingInput++
			case StatusCompleted:
				result.Completed++
			case StatusFailed:
				result.Failed++
			case StatusInterrupted:
				result.Interrupted++
			case StatusStale:
				result.Stale++
			}
		}
	}
	return result
}

func visibleWorkflow(item storedWorkflow, now time.Time) model.Workflow {
	nodes := make([]model.WorkflowNode, 0, len(item.Nodes))
	for _, node := range item.Nodes {
		if activeStatus(node.Status) && now.Unix()-node.UpdatedAt > int64(staleAfter/time.Second) {
			node.Status = StatusStale
			node.EndedAt = 0
		}
		nodes = append(nodes, node)
	}
	sort.Slice(nodes, func(i, j int) bool {
		if nodes[i].StartedAt == nodes[j].StartedAt {
			return nodes[i].ID < nodes[j].ID
		}
		return nodes[i].StartedAt < nodes[j].StartedAt
	})
	result := model.Workflow{
		ID: item.ID, Source: item.Source, SessionID: item.SessionID, TurnID: item.TurnID,
		Status: item.Status, Model: item.Model, StartedAt: item.StartedAt,
		UpdatedAt: item.UpdatedAt, EndedAt: item.EndedAt, Nodes: nodes,
	}
	result.Status = statusFromNodes(nodes)
	if activeStatus(item.Status) && now.Unix()-item.UpdatedAt > int64(staleAfter/time.Second) {
		result.Status = StatusStale
	}
	return result
}
