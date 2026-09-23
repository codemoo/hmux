package catalog

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/codemoo/hmux/internal/safeexec"
	"github.com/codemoo/hmux/internal/timing"
)

// sessionBinding is the sole provider-session association used by catalog and
// explicit conversation reads. No transcript path or provider ID leaves Home.
type sessionBinding struct {
	provider             string
	providerPID, filePID int
	path, root, recordID string
	model, state         string
	workingSince         int64
	status               sessionBindingStatus
}

func (s systemProcessInspector) resolveSessionBindings(ctx context.Context, nodes map[int]processNode, panes []int, home string) map[int]sessionBinding {
	return s.resolveBindings(ctx, nodes, panes, home, bindingFull)
}

type bindingMode uint8

const (
	bindingFull bindingMode = iota
	bindingCompletion
	bindingResume
)

// Resume capture needs both providers' exact identity, but no live model or
// activity scan. It still runs the normal descriptor and ambiguity checks.
func (s systemProcessInspector) resolveResumeBindings(ctx context.Context, nodes map[int]processNode, panes []int, home string) map[int]sessionBinding {
	return s.resolveBindings(ctx, nodes, panes, home, bindingResume)
}

// Completion discovery needs exact Codex ownership, not another all-provider
// metadata/state scan. Keep the same ambiguity and wrapper-chain checks.
func (s systemProcessInspector) resolveCompletionBindings(ctx context.Context, nodes map[int]processNode, panes []int) map[int]sessionBinding {
	return s.resolveBindings(ctx, nodes, panes, "", bindingCompletion)
}

func (s systemProcessInspector) resolveBindings(ctx context.Context, nodes map[int]processNode, panes []int, home string, mode bindingMode) map[int]sessionBinding {
	result := make(map[int]sessionBinding, len(panes))
	children := processChildren(nodes)
	owners := map[int]bool{}
	for _, pane := range panes {
		if ctx.Err() != nil {
			break
		}
		pid, status := nearestSessionProvider(nodes, children, pane)
		binding := sessionBinding{providerPID: pid, status: status}
		if pid > 0 {
			binding.provider = nodes[pid].Provider
			if mode != bindingCompletion || binding.provider == "codex" {
				owners[pid] = true
			}
		}
		result[pane] = binding
	}
	var candidates []int
	for pid := range owners {
		candidates = append(candidates, pid)
	}
	// Descriptor discovery is batched; the same provider is never read per pane.
	files := s.providerRecordFiles(ctx, candidates)
	resolved := map[int]sessionBinding{}
	for pid := range owners {
		if ctx.Err() != nil {
			break
		}
		base := sessionBinding{provider: nodes[pid].Provider, providerPID: pid, status: sessionBindingUnavailable}
		if base.provider == "codex" {
			base = bindCodexRecordsContext(ctx, base, pid, files[pid])
			// Only one non-provider wrapper chain may own the descriptor. Never search
			// sibling providers or select another agent's record from a process tree.
			if base.status == sessionBindingUnavailable && len(files[pid]) == 0 {
				chain, status := sessionWrapperChain(nodes, children, pid)
				if status == sessionBindingReady && len(chain) > 0 {
					wrappers := s.providerRecordFiles(ctx, chain)
					var bound []sessionBinding
					for _, child := range chain {
						b := bindCodexRecordsContext(ctx, base, child, wrappers[child])
						if b.status == sessionBindingReady {
							bound = append(bound, b)
						} else if b.status == sessionBindingAmbiguous {
							base.status = sessionBindingAmbiguous
						}
					}
					if len(bound) == 1 && base.status != sessionBindingAmbiguous {
						base = bound[0]
					} else if len(bound) > 1 {
						base.status = sessionBindingAmbiguous
					}
				}
			}
			if base.status == sessionBindingReady && mode == bindingFull {
				base.model, base.state, base.workingSince = readCodexEvents(base.path, base.root)
			}
		} else if base.provider == "claude" {
			base = bindClaudeSessionMode(home, base, mode == bindingFull)
		}
		resolved[pid] = base
	}
	for pane, binding := range result {
		if binding.providerPID > 0 {
			result[pane] = resolved[binding.providerPID]
		}
	}
	return result
}

// nearestSessionProvider stops at the first provider on each branch, excluding
// nested agents. Foreground providers win over suspended/background peers.
func nearestSessionProvider(nodes map[int]processNode, children map[int][]int, pane int) (int, sessionBindingStatus) {
	queue := []int{pane}
	seen := map[int]bool{}
	var found, foreground []int
	for len(queue) > 0 && len(seen) < maximumTreeNodes {
		current := queue[0]
		queue = queue[1:]
		if seen[current] {
			continue
		}
		seen[current] = true
		node, ok := nodes[current]
		if !ok {
			continue
		}
		if node.Provider != "" {
			found = append(found, node.PID)
			if strings.Contains(node.State, "+") {
				foreground = append(foreground, node.PID)
			}
			continue
		}
		for _, pid := range children[current] {
			queue = append(queue, pid)
		}
	}
	if len(queue) > 0 {
		return 0, sessionBindingAmbiguous
	}
	if len(foreground) == 1 {
		return foreground[0], sessionBindingReady
	}
	if len(found) == 0 {
		return 0, sessionBindingUnavailable
	}
	if len(found) != 1 {
		return 0, sessionBindingAmbiguous
	}
	return found[0], sessionBindingReady
}

func (s systemProcessInspector) providerRecordFiles(ctx context.Context, pids []int) map[int][]string {
	defer timing.Start(ctx, "provider-open-files", false)()
	out := map[int][]string{}
	if len(pids) == 0 {
		return out
	}
	sort.Ints(pids)
	values := make([]string, 0, len(pids))
	for _, pid := range pids {
		if pid > 0 {
			values = append(values, strconv.Itoa(pid))
		}
	}
	path := s.LsofPath
	if path == "" {
		path = executablePath("lsof", "/usr/sbin/lsof", "/usr/bin/lsof")
	}
	if path == "" {
		return out
	}
	raw, err := safeexec.OutputOnPartialExit(exec.CommandContext(ctx, path, "-n", "-a", "-p", strings.Join(values, ","), "-Fpn"), processOutputLimit, 1)
	// lsof may exit 1 for one vanished PID while returning valid records for others.
	if err != nil && len(raw) == 0 {
		return out
	}
	pid := 0
	for _, line := range lines(raw) {
		if strings.HasPrefix(line, "p") {
			pid, _ = strconv.Atoi(line[1:])
			continue
		}
		if pid > 0 && strings.HasPrefix(line, "n") && strings.HasSuffix(line, ".jsonl") {
			out[pid] = append(out[pid], line[1:])
		}
	}
	return out
}

func codexRecordRoot(path string) string {
	if !filepath.IsAbs(path) || !strings.HasPrefix(filepath.Base(path), "rollout-") || filepath.Ext(path) != ".jsonl" {
		return ""
	}
	clean := filepath.Clean(path)
	at := strings.LastIndex(clean, string(os.PathSeparator)+"sessions"+string(os.PathSeparator))
	if at < 0 {
		return ""
	}
	root := clean[:at+len("/sessions")]
	relative, _ := filepath.Rel(root, clean)
	parts := strings.Split(filepath.ToSlash(relative), "/")
	if len(parts) != 4 {
		return ""
	}
	if _, err := time.Parse("2006/01/02", strings.Join(parts[:3], "/")); err != nil {
		return ""
	}
	return root
}
func bindCodexRecords(base sessionBinding, filePID int, paths []string) sessionBinding {
	return bindCodexRecordsContext(context.Background(), base, filePID, paths)
}

func bindCodexRecordsContext(ctx context.Context, base sessionBinding, filePID int, paths []string) sessionBinding {
	var matches []sessionBinding
	unknown := false
	for _, path := range uniqueStrings(paths) {
		if ctx.Err() != nil {
			base.status = sessionBindingUnavailable
			return base
		}
		root := codexRecordRoot(path)
		if root == "" {
			continue
		}
		file, _, err := openSessionRecord(root, path)
		if err != nil {
			unknown = true
			continue
		}
		line, err := bufio.NewReader(io.LimitReader(file, 128*1024)).ReadBytes('\n')
		file.Close()
		if err != nil {
			unknown = true
			continue
		}
		var header struct {
			Type    string `json:"type"`
			Payload struct {
				ID     string          `json:"id"`
				Source json.RawMessage `json:"source"`
			} `json:"payload"`
		}
		if json.Unmarshal(line, &header) != nil || header.Type != "session_meta" || !safeSessionTokenPattern.MatchString(header.Payload.ID) || !strings.HasSuffix(filepath.Base(path), "-"+header.Payload.ID+".jsonl") {
			unknown = true
			continue
		}
		source := bytes.TrimSpace(header.Payload.Source)
		if len(source) > 0 && source[0] == '{' {
			continue
		} // subagent descriptors belong to their own sessions
		var kind string
		if json.Unmarshal(source, &kind) != nil {
			unknown = true
			continue
		}
		if kind != "cli" && kind != "exec" && kind != "vscode" {
			unknown = true
			continue
		}
		b := base
		b.filePID = filePID
		b.root = root
		b.path = path
		b.recordID = header.Payload.ID
		b.status = sessionBindingReady
		matches = append(matches, b)
	}
	if len(matches) == 1 && !unknown {
		return matches[0]
	}
	if len(matches) > 1 || unknown {
		base.status = sessionBindingAmbiguous
	} else {
		base.status = sessionBindingUnavailable
	}
	return base
}

func claudeConfigRoots(home string) []string {
	roots := []string{filepath.Join(home, ".claude")}
	parent := filepath.Join(home, ".claude-swap-backup", "sessions")
	dir, err := os.Open(parent)
	if err != nil {
		return roots
	}
	defer dir.Close()
	entries, err := dir.ReadDir(129)
	if err != nil && err != io.EOF {
		return roots
	}
	if len(entries) > 128 {
		return roots
	}
	for _, entry := range entries {
		if entry.IsDir() && entry.Type()&os.ModeSymlink == 0 {
			roots = append(roots, filepath.Join(parent, entry.Name()))
		}
	}
	return roots
}
func bindClaudeSession(home string, base sessionBinding) sessionBinding {
	return bindClaudeSessionMode(home, base, true)
}

func bindClaudeSessionMode(home string, base sessionBinding, scanModel bool) sessionBinding {
	var matches []sessionBinding
	for _, root := range claudeConfigRoots(home) {
		path := filepath.Join(root, "sessions", strconv.Itoa(base.providerPID)+".json")
		file, _, err := openSessionRecord(filepath.Join(root, "sessions"), path)
		if err != nil {
			continue
		}
		raw, err := io.ReadAll(io.LimitReader(file, 128*1024+1))
		file.Close()
		if err != nil || len(raw) > 128*1024 {
			continue
		}
		var record struct {
			PID     int    `json:"pid"`
			ID      string `json:"sessionId"`
			Status  string `json:"status"`
			Updated int64  `json:"statusUpdatedAt"`
		}
		if json.Unmarshal(raw, &record) != nil || record.PID != base.providerPID || !safeSessionTokenPattern.MatchString(record.ID) {
			continue
		}
		b := base
		b.root = filepath.Join(root, "projects")
		b.recordID = record.ID
		b.state = normalizedClaudeState(record.Status)
		b.filePID = base.providerPID
		if b.state == "working" && record.Updated > 0 {
			b.workingSince = record.Updated / 1000
		}
		projects, err := os.Open(b.root)
		if err != nil {
			continue
		}
		entries, err := projects.ReadDir(513)
		projects.Close()
		if (err != nil && err != io.EOF) || len(entries) > 512 {
			continue
		}
		for _, entry := range entries {
			if !entry.IsDir() || entry.Type()&os.ModeSymlink != 0 {
				continue
			}
			candidate := filepath.Join(b.root, entry.Name(), record.ID+".jsonl")
			f, _, err := openSessionRecord(b.root, candidate)
			if err != nil {
				continue
			}
			f.Close()
			if b.path != "" {
				base.status = sessionBindingAmbiguous
				return base
			}
			b.path = candidate
		}
		if b.path == "" {
			continue
		}
		b.status = sessionBindingReady
		if scanModel {
			forEachTailRecord(b.path, b.root, func(line []byte) {
				var e struct {
					Message struct {
						Model string `json:"model"`
					} `json:"message"`
				}
				if json.Unmarshal(line, &e) == nil {
					if m := validatedModel(e.Message.Model); m != "" {
						b.model = m
					}
				}
			})
		}
		// PID registry must still refer to the same session after reading its metadata.
		f, _, err := openSessionRecord(filepath.Join(root, "sessions"), path)
		if err != nil {
			continue
		}
		again, _ := io.ReadAll(io.LimitReader(f, 128*1024+1))
		f.Close()
		if !bytes.Equal(raw, again) {
			continue
		}
		matches = append(matches, b)
	}
	if len(matches) == 1 {
		return matches[0]
	}
	if len(matches) > 1 {
		base.status = sessionBindingAmbiguous
	}
	return base
}

// Shared safe opener for binding metadata and explicitly requested public text.
func openSessionRecord(rootPath, path string) (*os.File, os.FileInfo, error) {
	fail := func() (*os.File, os.FileInfo, error) { return nil, nil, errors.New("session record unavailable") }
	if !filepath.IsAbs(path) || !filepath.IsAbs(rootPath) {
		return fail()
	}
	relative, err := filepath.Rel(rootPath, filepath.Clean(path))
	if err != nil || relative == "." || relative == ".." || filepath.IsAbs(relative) || strings.HasPrefix(relative, ".."+string(os.PathSeparator)) {
		return fail()
	}
	// Reject symlinks at every component rather than merely their final target.
	for current := filepath.Clean(path); ; current = filepath.Dir(current) {
		info, err := os.Lstat(current)
		if err != nil || info.Mode()&os.ModeSymlink != 0 {
			return fail()
		}
		if current == filepath.Clean(rootPath) {
			break
		}
		if current == filepath.Dir(current) {
			return fail()
		}
	}
	root, err := os.OpenRoot(rootPath)
	if err != nil {
		return fail()
	}
	defer root.Close()
	before, err := root.Lstat(relative)
	if err != nil || !before.Mode().IsRegular() || !sessionRecordOwnedByUser(before) {
		return fail()
	}
	file, err := root.Open(relative)
	if err != nil {
		return fail()
	}
	info, err := file.Stat()
	after, lastErr := root.Lstat(relative)
	if err != nil || lastErr != nil || !info.Mode().IsRegular() || !os.SameFile(before, info) || !os.SameFile(after, info) || !sessionRecordOwnedByUser(info) {
		file.Close()
		return fail()
	}
	return file, info, nil
}

func sessionWrapperChain(nodes map[int]processNode, children map[int][]int, providerPID int) ([]int, sessionBindingStatus) {
	const maximumWrapperDepth = 16
	current := providerPID
	chain := make([]int, 0, maximumWrapperDepth)
	for range maximumWrapperDepth {
		descendants := children[current]
		if len(descendants) == 0 {
			return chain, sessionBindingReady
		}
		if len(descendants) != 1 {
			return nil, sessionBindingAmbiguous
		}
		child := nodes[descendants[0]]
		if child.PID < 1 {
			return nil, sessionBindingUnavailable
		}
		if child.Provider != "" {
			return nil, sessionBindingAmbiguous
		}
		chain = append(chain, child.PID)
		current = child.PID
	}
	if len(children[current]) != 0 {
		return nil, sessionBindingUnavailable
	}
	return chain, sessionBindingReady
}

func uniqueStrings(values []string) []string {
	seen := make(map[string]bool, len(values))
	result := make([]string, 0, len(values))
	for _, value := range values {
		if value != "" && !seen[value] {
			seen[value] = true
			result = append(result, value)
		}
	}
	sort.Strings(result)
	return result
}

type sessionBindingStatus int

const (
	sessionBindingUnavailable sessionBindingStatus = iota
	sessionBindingReady
	sessionBindingAmbiguous
)

func sessionRecordOwnedByUser(info os.FileInfo) bool {
	stat, ok := info.Sys().(*syscall.Stat_t)
	return ok && stat.Uid == uint32(os.Geteuid())
}
