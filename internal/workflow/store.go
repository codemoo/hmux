package workflow

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"time"
	"unicode/utf8"

	"github.com/codemoo/hmux/internal/model"
	"golang.org/x/sys/unix"
)

const (
	stateVersion          = 1
	maxHookInput          = 256 * 1024
	maxStateSize          = 16 * 1024 * 1024
	maxWorkflows          = 1024
	maxNodesPerWorkflow   = 128
	maxVisibleWorkflows   = 32
	terminalRetention     = 7 * 24 * time.Hour
	staleAfter            = 2 * time.Hour
	defaultLockTimeout    = 750 * time.Millisecond
	lockPollInterval      = 10 * time.Millisecond
	StatusRunning         = "running"
	StatusWaitingApproval = "waiting_approval"
	StatusWaitingInput    = "waiting_input"
	StatusCompleted       = "completed"
	StatusFailed          = "failed"
	StatusInterrupted     = "interrupted"
	StatusStale           = "stale"
)

type Store struct {
	StateDir string
	Now      func() time.Time
	lockWait time.Duration
}

type Binding struct {
	SessionID string
	CreatedAt int64
}

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

type storedWorkflow struct {
	ID            string                        `json:"id"`
	TMUXSessionID string                        `json:"tmux_session_id"`
	TMUXCreatedAt int64                         `json:"tmux_created_at"`
	Source        string                        `json:"source"`
	SessionID     string                        `json:"session_id,omitempty"`
	TurnID        string                        `json:"turn_id,omitempty"`
	Status        string                        `json:"status"`
	Model         string                        `json:"model,omitempty"`
	StartedAt     int64                         `json:"started_at"`
	UpdatedAt     int64                         `json:"updated_at"`
	EndedAt       int64                         `json:"ended_at,omitempty"`
	Nodes         map[string]model.WorkflowNode `json:"nodes"`
}

type state struct {
	Version   int                       `json:"version"`
	Workflows map[string]storedWorkflow `json:"workflows"`
	UpdatedAt string                    `json:"updated_at"`
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

func (s Store) readPruned() (state, error) {
	current, err := s.read()
	if err != nil {
		return current, err
	}
	now := s.currentTime()
	if !needsPrune(current, now) {
		return current, nil
	}
	lock, err := openLock(filepath.Join(s.root(), "state.lock"))
	if err != nil {
		return state{}, err
	}
	defer lock.Close()
	if err := s.acquireLock(lock); err != nil {
		return state{}, err
	}
	defer unix.Flock(int(lock.Fd()), unix.LOCK_UN) //nolint:errcheck
	current, err = s.read()
	if err != nil {
		return state{}, err
	}
	if !prune(&current, now) {
		return current, nil
	}
	current.UpdatedAt = now.UTC().Format(time.RFC3339Nano)
	if err := validateState(current); err != nil {
		return state{}, err
	}
	if err := s.write(current); err != nil {
		return state{}, err
	}
	return current, nil
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

func (s Store) update(action func(*state, time.Time) error) error {
	if err := s.ensureRoot(); err != nil {
		return err
	}
	lock, err := openLock(filepath.Join(s.root(), "state.lock"))
	if err != nil {
		return err
	}
	defer lock.Close()
	if err := s.acquireLock(lock); err != nil {
		return err
	}
	defer unix.Flock(int(lock.Fd()), unix.LOCK_UN) //nolint:errcheck

	current, err := s.read()
	if errors.Is(err, os.ErrNotExist) {
		current = state{Version: stateVersion, Workflows: map[string]storedWorkflow{}}
	} else if err != nil {
		return err
	}
	now := s.currentTime()
	if err := action(&current, now); err != nil {
		return err
	}
	prune(&current, now)
	current.Version = stateVersion
	current.UpdatedAt = now.UTC().Format(time.RFC3339Nano)
	if _, err := pruneToSize(&current, maxStateSize); err != nil {
		return err
	}
	if err := validateState(current); err != nil {
		return err
	}
	return s.write(current)
}

func (s Store) acquireLock(lock *os.File) error {
	wait := s.lockWait
	if wait <= 0 {
		wait = defaultLockTimeout
	}
	deadline := time.Now().Add(wait)
	for {
		err := unix.Flock(int(lock.Fd()), unix.LOCK_EX|unix.LOCK_NB)
		if err == nil {
			return nil
		}
		if !errors.Is(err, unix.EWOULDBLOCK) && !errors.Is(err, unix.EAGAIN) {
			return err
		}
		remaining := time.Until(deadline)
		if remaining <= 0 {
			return errors.New("workflow state lock is busy")
		}
		if remaining > lockPollInterval {
			remaining = lockPollInterval
		}
		time.Sleep(remaining)
	}
}

func (s Store) read() (state, error) {
	var current state
	path := filepath.Join(s.root(), "state.json")
	fd, err := unix.Open(path, unix.O_RDONLY|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0)
	if err != nil {
		return current, err
	}
	file := os.NewFile(uintptr(fd), path)
	if file == nil {
		_ = unix.Close(fd)
		return current, errors.New("open workflow state file")
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return current, err
	}
	if err := validatePrivateFile(info, maxStateSize); err != nil {
		return current, err
	}
	decoder := json.NewDecoder(io.LimitReader(file, maxStateSize+1))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&current); err != nil {
		return current, err
	}
	if decoder.Decode(&struct{}{}) != io.EOF {
		return current, errors.New("workflow state contains trailing data")
	}
	if err := validateState(current); err != nil {
		return current, err
	}
	return current, nil
}

func (s Store) write(current state) error {
	data, err := json.Marshal(current)
	if err != nil {
		return err
	}
	data = append(data, '\n')
	if len(data) > maxStateSize {
		return errors.New("workflow state exceeds size limit")
	}
	path := filepath.Join(s.root(), "state.json")
	if info, err := os.Lstat(path); err == nil {
		if err := validatePrivateFile(info, maxStateSize); err != nil {
			return err
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	temp, err := os.CreateTemp(s.root(), ".hmux-workflows-*")
	if err != nil {
		return err
	}
	tempPath := temp.Name()
	defer os.Remove(tempPath)
	if err := temp.Chmod(0o600); err != nil {
		return errors.Join(err, temp.Close())
	}
	if _, err := temp.Write(data); err != nil {
		return errors.Join(err, temp.Close())
	}
	if err := temp.Sync(); err != nil {
		return errors.Join(err, temp.Close())
	}
	if err := temp.Close(); err != nil {
		return err
	}
	if err := os.Rename(tempPath, path); err != nil {
		return err
	}
	directory, err := os.Open(s.root())
	if err != nil {
		return err
	}
	defer directory.Close()
	return directory.Sync()
}

func (s Store) ensureRoot() error {
	root := filepath.Clean(s.StateDir)
	if !filepath.IsAbs(root) || root == string(os.PathSeparator) {
		return errors.New("workflow state directory must be an absolute non-root path")
	}
	if err := os.MkdirAll(root, 0o700); err != nil {
		return err
	}
	if err := ensureOwnedDirectory(root); err != nil {
		return err
	}
	workflowRoot := s.root()
	if err := os.Mkdir(workflowRoot, 0o700); err != nil && !errors.Is(err, os.ErrExist) {
		return err
	}
	return ensureOwnedDirectory(workflowRoot)
}

func (s Store) root() string {
	return filepath.Join(filepath.Clean(s.StateDir), "workflows")
}

func (s Store) currentTime() time.Time {
	if s.Now != nil {
		return s.Now().UTC()
	}
	return time.Now().UTC()
}

func ensureOwnedDirectory(path string) error {
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if !info.IsDir() || info.Mode()&os.ModeSymlink != 0 {
		return errors.New("workflow state path is not a directory")
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok || int(stat.Uid) != os.Geteuid() {
		return errors.New("workflow state directory is not owned by the current user")
	}
	return os.Chmod(path, 0o700)
}

func openLock(path string) (*os.File, error) {
	fd, err := unix.Open(path, unix.O_CREAT|unix.O_RDWR|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0o600)
	if err != nil {
		return nil, err
	}
	file := os.NewFile(uintptr(fd), path)
	if file == nil {
		_ = unix.Close(fd)
		return nil, errors.New("create workflow lock file")
	}
	var stat unix.Stat_t
	if err := unix.Fstat(fd, &stat); err != nil {
		_ = file.Close()
		return nil, err
	}
	if stat.Mode&unix.S_IFMT != unix.S_IFREG || int(stat.Uid) != os.Geteuid() {
		_ = file.Close()
		return nil, errors.New("workflow lock is not a current-user regular file")
	}
	if err := unix.Fchmod(fd, 0o600); err != nil {
		_ = file.Close()
		return nil, err
	}
	return file, nil
}

func validatePrivateFile(info os.FileInfo, maximum int64) error {
	if !info.Mode().IsRegular() || info.Mode()&os.ModeSymlink != 0 || info.Size() < 1 || info.Size() > maximum {
		return errors.New("workflow state is not a small regular file")
	}
	if info.Mode().Perm()&0o077 != 0 {
		return errors.New("workflow state permissions are not private")
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok || int(stat.Uid) != os.Geteuid() {
		return errors.New("workflow state is not owned by the current user")
	}
	return nil
}

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
