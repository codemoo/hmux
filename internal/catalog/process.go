package catalog

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/safeexec"
	"github.com/codemoo/hmux/internal/timing"
)

const (
	processOutputLimit = 32 * 1024 * 1024
	eventTailLimit     = 8 * 1024 * 1024
	eventLineLimit     = 2 * 1024 * 1024
	maximumPanePIDs    = 4096
	maximumTreeNodes   = 10000
)

var safeModelPattern = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._:+-]{0,127}$`)
var safeSessionTokenPattern = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9-]{0,127}$`)

type processMetadata struct {
	Runtime      string
	Model        string
	State        string
	Process      string
	WorkingSince int64
}

type processNode struct {
	PID      int
	PPID     int
	State    string
	CPU      float64
	Process  string
	Provider string
}

type processCandidate struct {
	Node  processNode
	Depth int
}

type systemProcessInspector struct {
	HomeDir  string
	PSPath   string
	LsofPath string
}

func inspectProcesses(ctx context.Context, sessions map[string]*model.Session) map[int]processMetadata {
	seen := make(map[int]bool)
	var panePIDs []int
	for _, session := range sessions {
		if session.PanePID > 0 && !seen[session.PanePID] {
			if len(panePIDs) >= maximumPanePIDs {
				break
			}
			seen[session.PanePID] = true
			panePIDs = append(panePIDs, session.PanePID)
		}
	}
	if len(panePIDs) == 0 {
		return nil
	}
	result, err := (systemProcessInspector{}).Inspect(ctx, panePIDs)
	if err != nil {
		// Process metadata is deliberately best-effort. A transient ps/lsof
		// failure must not make the tmux catalog or attach path unavailable.
		return nil
	}
	return result
}

func (s systemProcessInspector) Inspect(ctx context.Context, panePIDs []int) (map[int]processMetadata, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if len(panePIDs) > maximumPanePIDs {
		return nil, errors.New("pane process count exceeds limit")
	}
	nodes, err := s.processSnapshot(ctx)
	if err != nil {
		return nil, err
	}
	children := processChildren(nodes)

	home := s.HomeDir
	if home == "" {
		home, _ = os.UserHomeDir()
	}
	bindings := s.resolveSessionBindings(ctx, nodes, panePIDs, home)
	result := make(map[int]processMetadata, len(panePIDs))
	for _, panePID := range panePIDs {
		if ctx.Err() != nil {
			return nil, ctx.Err()
		}
		binding := bindings[panePID]
		if binding.provider != "" {
			node := nodes[binding.providerPID]
			metadata := processMetadata{Runtime: binding.provider, Process: node.Process, State: inferAgentState(node)}
			if binding.status == sessionBindingReady {
				metadata.Model = binding.model
				if binding.state != "" {
					metadata.State = binding.state
				}
				metadata.WorkingSince = binding.workingSince
			}
			result[panePID] = metadata
			continue
		}
		candidate, ok := selectProcessCandidate(nodes, children, panePID)
		if ok {
			// Unbound/ambiguous agent trees must not regain an inferred provider
			// identity through a second selection heuristic.
			result[panePID] = processMetadata{Runtime: "process", Process: candidate.Node.Process, State: "running"}
		}
	}
	return result, nil
}

func (s systemProcessInspector) processSnapshot(ctx context.Context) (map[int]processNode, error) {
	defer timing.Start(ctx, "process-snapshot", false)()
	psPath := s.PSPath
	if psPath == "" {
		psPath = executablePath("ps", "/bin/ps", "/usr/bin/ps")
	}
	if psPath == "" {
		return nil, errors.New("ps executable not found")
	}
	raw, err := safeexec.Output(
		exec.CommandContext(ctx, psPath, "-axo", "pid=,ppid=,state=,pcpu=,comm="),
		processOutputLimit,
	)
	if err != nil {
		return nil, fmt.Errorf("process snapshot: %w", err)
	}
	return parseProcessTable(raw)
}

func parseProcessTable(raw []byte) (map[int]processNode, error) {
	if len(raw) > processOutputLimit {
		return nil, errors.New("process snapshot exceeds limit")
	}
	nodes := make(map[int]processNode)
	for _, line := range lines(raw) {
		fields := strings.Fields(line)
		if len(fields) < 5 {
			continue
		}
		pid, errPID := strconv.Atoi(fields[0])
		ppid, errPPID := strconv.Atoi(fields[1])
		cpu, errCPU := strconv.ParseFloat(fields[3], 64)
		if errPID != nil || errPPID != nil || errCPU != nil || pid < 1 || ppid < 0 {
			continue
		}
		command := strings.Join(fields[4:], " ")
		process := processName(command)
		if process == "" {
			continue
		}
		nodes[pid] = processNode{
			PID:      pid,
			PPID:     ppid,
			State:    model.SafeText(fields[2], 16),
			CPU:      cpu,
			Process:  process,
			Provider: providerName(command),
		}
	}
	if len(nodes) == 0 && len(bytes.TrimSpace(raw)) > 0 {
		return nil, errors.New("process snapshot contains no valid rows")
	}
	return nodes, nil
}

func processChildren(nodes map[int]processNode) map[int][]int {
	children := make(map[int][]int)
	for pid, node := range nodes {
		children[node.PPID] = append(children[node.PPID], pid)
	}
	for parent := range children {
		sort.Ints(children[parent])
	}
	return children
}

func selectProcessCandidate(nodes map[int]processNode, children map[int][]int, panePID int) (processCandidate, bool) {
	root, ok := nodes[panePID]
	if !ok {
		return processCandidate{}, false
	}
	type queued struct {
		PID   int
		Depth int
	}
	queue := []queued{{PID: panePID, Depth: 0}}
	seen := make(map[int]bool)
	var all []processCandidate
	for len(queue) > 0 && len(seen) < maximumTreeNodes {
		current := queue[0]
		queue = queue[1:]
		if seen[current.PID] {
			continue
		}
		seen[current.PID] = true
		node, present := nodes[current.PID]
		if !present {
			continue
		}
		all = append(all, processCandidate{Node: node, Depth: current.Depth})
		for _, child := range children[current.PID] {
			queue = append(queue, queued{PID: child, Depth: current.Depth + 1})
		}
	}

	var provider *processCandidate
	for index := range all {
		candidate := &all[index]
		if candidate.Node.Provider == "" {
			continue
		}
		if provider == nil || candidate.Depth < provider.Depth ||
			(candidate.Depth == provider.Depth && candidate.Node.PID < provider.Node.PID) {
			copy := *candidate
			provider = &copy
		}
	}
	if provider != nil {
		return *provider, true
	}

	best := processCandidate{Node: root, Depth: 0}
	bestRank := genericProcessRank(best)
	for _, candidate := range all {
		rank := genericProcessRank(candidate)
		if rank > bestRank ||
			(rank == bestRank && candidate.Depth > best.Depth) ||
			(rank == bestRank && candidate.Depth == best.Depth && candidate.Node.CPU > best.Node.CPU) {
			best = candidate
			bestRank = rank
		}
	}
	return best, true
}

func genericProcessRank(candidate processCandidate) int {
	rank := 0
	if !isShellProcess(candidate.Node.Process) {
		rank += 100
	}
	if strings.Contains(candidate.Node.State, "+") {
		rank += 30
	}
	if candidate.Node.CPU >= 0.5 {
		rank += 20
	}
	if candidate.Depth > 0 {
		rank += 10
	}
	return rank
}

func providerName(command string) string {
	clean := filepath.ToSlash(strings.TrimSpace(command))
	if strings.Contains(clean, "/claude/versions/") && safeModelPattern.MatchString(filepath.Base(clean)) {
		return "claude"
	}
	base := strings.ToLower(processName(command))
	switch base {
	case "claude":
		return "claude"
	case "codex":
		return "codex"
	default:
		return ""
	}
}

func processName(command string) string {
	command = strings.TrimSpace(command)
	if command == "" {
		return ""
	}
	if strings.Contains(command, "/") {
		command = filepath.Base(command)
	}
	command = strings.TrimLeft(command, "-")
	if fields := strings.Fields(command); len(fields) > 0 {
		command = fields[0]
	}
	return model.SafeText(command, 128)
}

func isShellProcess(process string) bool {
	switch strings.ToLower(strings.TrimLeft(process, "-")) {
	case "bash", "dash", "fish", "login", "sh", "tcsh", "zsh":
		return true
	default:
		return false
	}
}

func inferAgentState(node processNode) string {
	if strings.HasPrefix(node.State, "R") || strings.HasPrefix(node.State, "D") || node.CPU >= 0.5 {
		return "working"
	}
	return "idle"
}

func readCodexEvents(path, root string) (string, string, int64) {
	if !safeEventFile(path, root) {
		return "", "", 0
	}
	var latestModel string
	var latestState string
	var workingSince int64
	forEachTailRecord(path, root, func(line []byte) {
		var event struct {
			Type      string `json:"type"`
			Model     string `json:"model"`
			Timestamp string `json:"timestamp"`
			Payload   struct {
				Type  string `json:"type"`
				Model string `json:"model"`
			} `json:"payload"`
		}
		if json.Unmarshal(line, &event) != nil {
			return
		}
		if event.Type == "turn_context" {
			if value := validatedModel(firstNonEmpty(event.Payload.Model, event.Model)); value != "" {
				latestModel = value
			}
		}
		eventType := event.Payload.Type
		if event.Type == "task_started" || event.Type == "task_complete" {
			eventType = event.Type
		}
		switch eventType {
		case "task_started":
			latestState = "working"
			workingSince = parseTimestamp(event.Timestamp)
		case "task_complete":
			latestState = "idle"
			workingSince = 0
		}
	})
	return latestModel, latestState, workingSince
}

func parseTimestamp(value string) int64 {
	if value == "" {
		return 0
	}
	parsed, err := time.Parse(time.RFC3339Nano, value)
	if err != nil {
		return 0
	}
	return parsed.Unix()
}

func normalizedClaudeState(value string) string {
	switch strings.ToLower(model.SafeText(value, 32)) {
	case "active", "busy", "running", "working":
		return "working"
	case "idle", "waiting":
		return "idle"
	default:
		return ""
	}
}

func safeEventFile(path, root string) bool {
	if filepath.Ext(path) != ".jsonl" || !withinRoot(path, root) {
		return false
	}
	info, err := os.Lstat(path)
	return err == nil && info.Mode().IsRegular() && info.Mode()&os.ModeSymlink == 0
}

func withinRoot(path, root string) bool {
	path, errPath := filepath.EvalSymlinks(filepath.Clean(path))
	root, errRoot := filepath.EvalSymlinks(filepath.Clean(root))
	if errPath != nil || errRoot != nil {
		return false
	}
	path, errPath = filepath.Abs(path)
	root, errRoot = filepath.Abs(root)
	if errPath != nil || errRoot != nil {
		return false
	}
	relative, err := filepath.Rel(root, path)
	return err == nil && relative != ".." && !strings.HasPrefix(relative, ".."+string(os.PathSeparator))
}

func forEachTailRecord(path, root string, callback func([]byte)) {
	file, _, err := openSessionRecord(root, path)
	if err != nil {
		return
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil || !info.Mode().IsRegular() {
		return
	}
	start := info.Size() - eventTailLimit
	if start < 0 {
		start = 0
	}
	if _, err := file.Seek(start, io.SeekStart); err != nil {
		return
	}
	data, err := io.ReadAll(io.LimitReader(file, eventTailLimit+1))
	if err != nil || len(data) > eventTailLimit {
		return
	}
	if start > 0 {
		if newline := bytes.IndexByte(data, '\n'); newline >= 0 {
			data = data[newline+1:]
		} else {
			return
		}
	}
	for _, line := range bytes.Split(data, []byte{'\n'}) {
		if len(line) == 0 || len(line) > eventLineLimit {
			continue
		}
		callback(line)
	}
}

func executablePath(name string, fallbacks ...string) string {
	if path, err := exec.LookPath(name); err == nil {
		return path
	}
	for _, path := range fallbacks {
		if info, err := os.Stat(path); err == nil && info.Mode().IsRegular() && info.Mode()&0o111 != 0 {
			return path
		}
	}
	return ""
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if value != "" {
			return value
		}
	}
	return ""
}

func validatedModel(value string) string {
	value = model.SafeText(value, 128)
	if !safeModelPattern.MatchString(value) {
		return ""
	}
	return value
}
