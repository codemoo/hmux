package catalog

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func bindingRollout(t *testing.T, root, id, source string) string {
	t.Helper()
	dir := filepath.Join(root, "2026", "09", "09")
	if err := os.MkdirAll(dir, 0700); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(dir, "rollout-2026-09-09T00-00-00-"+id+".jsonl")
	data := fmt.Sprintf("{\"type\":\"session_meta\",\"payload\":{\"id\":%q,\"source\":%s}}\n{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-test\"}}\n", id, source)
	if err := os.WriteFile(path, []byte(data), 0600); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestCompletionResolverSkipsClaudeAndCodexStateScan(t *testing.T) {
	home := t.TempDir()
	path := bindingRollout(t, filepath.Join(home, ".codex", "sessions"), "completion", `"cli"`)
	fake := filepath.Join(t.TempDir(), "lsof")
	// Completion discovery must query only the Codex provider, never Claude.
	if err := os.WriteFile(fake, []byte("#!/bin/sh\ncase \"$*\" in *30*) exit 1;; esac\nprintf '%s\\n' \"$HMUX_BINDING_LSOF\"\n"), 0700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HMUX_BINDING_LSOF", "p20\nn"+path)
	nodes := map[int]processNode{10: {PID: 10, Process: "zsh"}, 20: {PID: 20, PPID: 10, Provider: "codex", Process: "codex"}, 30: {PID: 30, Provider: "claude", Process: "claude"}}
	inspector := systemProcessInspector{HomeDir: home, LsofPath: fake}
	bindings := inspector.resolveCompletionBindings(context.Background(), nodes, []int{10, 30})
	if bindings[10].status != sessionBindingReady || bindings[10].path != path {
		t.Fatal("Codex binding missing")
	}
	if bindings[10].model != "" || bindings[10].state != "" {
		t.Fatal("completion resolver scanned Codex state tail")
	}
	if bindings[30].path != "" {
		t.Fatal("completion resolver read Claude metadata")
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if b := bindCodexRecordsContext(ctx, sessionBinding{provider: "codex"}, 20, []string{path}); b.status == sessionBindingReady {
		t.Fatal("cancelled binding read accepted")
	}
}

func TestResumeResolverBindsBothProvidersWithoutTranscriptScan(t *testing.T) {
	home := t.TempDir()
	codex := bindingRollout(t, filepath.Join(home, ".codex", "sessions"), "resume-codex", `"cli"`)
	claudeRoot := filepath.Join(home, ".claude")
	for _, dir := range []string{"sessions", "projects/project"} {
		if err := os.MkdirAll(filepath.Join(claudeRoot, dir), 0700); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.WriteFile(filepath.Join(claudeRoot, "sessions", "30.json"), []byte(`{"pid":30,"sessionId":"resume-claude","status":"busy"}`), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(claudeRoot, "projects", "project", "resume-claude.jsonl"), []byte("{\"message\":{\"model\":\"claude-test\"}}\n"), 0600); err != nil {
		t.Fatal(err)
	}
	fake := filepath.Join(t.TempDir(), "lsof")
	if err := os.WriteFile(fake, []byte("#!/bin/sh\nprintf '%s\\n' \"$HMUX_BINDING_LSOF\"\n"), 0700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HMUX_BINDING_LSOF", "p20\nn"+codex)
	nodes := map[int]processNode{10: {PID: 10, Process: "zsh"}, 20: {PID: 20, PPID: 10, Provider: "codex"}, 30: {PID: 30, Provider: "claude"}}
	bindings := (systemProcessInspector{HomeDir: home, LsofPath: fake}).resolveResumeBindings(context.Background(), nodes, []int{10, 30}, home)
	for pane, id := range map[int]string{10: "resume-codex", 30: "resume-claude"} {
		if b := bindings[pane]; b.status != sessionBindingReady || b.recordID != id || b.model != "" {
			t.Fatalf("resume binding for %d = %#v", pane, b)
		}
	}
}
func TestCodexBindingUsesMainMetadataNotFilenameOrAge(t *testing.T) {
	root := filepath.Join(t.TempDir(), "custom-codex-home", "sessions")
	main := bindingRollout(t, root, "z-main", `"cli"`)
	sub := bindingRollout(t, root, "a-sub", `{"subagent":{"thread_spawn":{}}}`)
	base := sessionBinding{provider: "codex", providerPID: 20}
	binding := bindCodexRecords(base, 20, []string{sub, main, main})
	if binding.status != sessionBindingReady || binding.path != main || binding.root != root || binding.recordID != "z-main" {
		t.Fatal("unique main was not selected")
	}
	other := bindingRollout(t, root, "other-main", `"cli"`)
	if got := bindCodexRecords(base, 20, []string{other, main}); got.status != sessionBindingAmbiguous {
		t.Fatal("multiple main sessions guessed")
	}
	if got := bindCodexRecords(base, 20, []string{sub}); got.status == sessionBindingReady {
		t.Fatal("subagent substituted for main")
	}
	if err := os.WriteFile(other, []byte(`{"type":"session_meta","payload":{"id":"wrong-id","source":"cli"}}`+"\n"), 0600); err != nil {
		t.Fatal(err)
	}
	if got := bindCodexRecords(base, 20, []string{other}); got.status == sessionBindingReady {
		t.Fatal("header identity mismatch accepted")
	}
}
func TestSessionRecordRejectsSymlinkAndPartialHeader(t *testing.T) {
	root := filepath.Join(t.TempDir(), "sessions")
	path := bindingRollout(t, root, "main", `"cli"`)
	if err := os.WriteFile(path, []byte(`{"type":"session_meta"`), 0600); err != nil {
		t.Fatal(err)
	}
	if b := bindCodexRecords(sessionBinding{provider: "codex"}, 1, []string{path}); b.status == sessionBindingReady {
		t.Fatal("partial record accepted")
	}
	target := bindingRollout(t, filepath.Join(t.TempDir(), "sessions"), "main", `"cli"`)
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(target, path); err != nil {
		t.Fatal(err)
	}
	if f, _, err := openSessionRecord(root, path); err == nil {
		f.Close()
		t.Fatal("symlink accepted")
	}
}
func TestSharedResolverKeepsOriginalActivePaneProvider(t *testing.T) {
	home := t.TempDir()
	root := filepath.Join(home, ".codex", "sessions")
	main := bindingRollout(t, root, "main", `"cli"`)
	sub := bindingRollout(t, root, "sub", `{"subagent":{"other":"review"}}`)
	fake := filepath.Join(t.TempDir(), "lsof")
	if err := os.WriteFile(fake, []byte("#!/bin/sh\nprintf '%s\\n' \"$HMUX_BINDING_LSOF\"\n"), 0700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HMUX_BINDING_LSOF", "p20\nn"+sub+"\nn"+main)
	nodes := map[int]processNode{10: {PID: 10, Process: "zsh"}, 20: {PID: 20, PPID: 10, Provider: "codex", Process: "codex"}, 30: {PID: 30, PPID: 20, Provider: "codex", Process: "codex"}, 40: {PID: 40, Process: "zsh"}}
	inspector := systemProcessInspector{HomeDir: home, LsofPath: fake}
	bindings := inspector.resolveSessionBindings(context.Background(), nodes, []int{10, 40}, home)
	if bindings[10].providerPID != 20 || bindings[10].path != main || bindings[10].model != "gpt-test" {
		t.Fatal("shared resolver did not bind main record")
	}
	if bindings[40].status == sessionBindingReady {
		t.Fatal("another tmux pane borrowed the active record")
	}
}
func TestClaudeBindingFindsCSwapProfileAndRejectsDuplicateRegistry(t *testing.T) {
	home := t.TempDir()
	id := "claude-session"
	profile := filepath.Join(home, ".claude-swap-backup", "sessions", "slot-1")
	write := func(root string) {
		t.Helper()
		for _, dir := range []string{"sessions", "projects/project"} {
			if err := os.MkdirAll(filepath.Join(root, dir), 0700); err != nil {
				t.Fatal(err)
			}
		}
		if err := os.WriteFile(filepath.Join(root, "sessions", "20.json"), []byte(`{"pid":20,"sessionId":"claude-session","status":"busy"}`), 0600); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(root, "projects", "project", id+".jsonl"), []byte("{\"type\":\"assistant\",\"message\":{\"model\":\"claude-test\"}}\n"), 0600); err != nil {
			t.Fatal(err)
		}
	}
	write(profile)
	base := sessionBinding{provider: "claude", providerPID: 20}
	b := bindClaudeSession(home, base)
	if b.status != sessionBindingReady || b.recordID != id || b.model != "claude-test" || b.state != "working" {
		t.Fatal("cswap profile session was not resolved")
	}
	write(filepath.Join(home, ".claude"))
	if b := bindClaudeSession(home, base); b.status != sessionBindingAmbiguous {
		t.Fatal("duplicate Claude registries guessed")
	}
	if providerName("/synthetic/.local/share/claude/versions/2.1.263") != "claude" {
		t.Fatal("versioned Claude executable missed")
	}
	if providerName("/synthetic/other/2.1.263") != "" {
		t.Fatal("unrelated versioned executable classified")
	}
}
func TestNearestProviderDoesNotChoosePeerByPID(t *testing.T) {
	nodes := map[int]processNode{1: {PID: 1}, 2: {PID: 2, PPID: 1, Provider: "codex"}, 3: {PID: 3, PPID: 1, Provider: "claude"}}
	if _, status := nearestSessionProvider(nodes, processChildren(nodes), 1); status != sessionBindingAmbiguous {
		t.Fatal("ambiguous peers selected")
	}
	node := nodes[3]
	node.State = "S+"
	nodes[3] = node
	if pid, status := nearestSessionProvider(nodes, processChildren(nodes), 1); status != sessionBindingReady || pid != 3 {
		t.Fatal("foreground process not selected")
	}
	for _, path := range []string{"relative/sessions/2026/09/09/rollout-id.jsonl", "/tmp/sessions/rollout-id.jsonl", "/tmp/sessions/2026/99/99/rollout-id.jsonl"} {
		if codexRecordRoot(path) != "" {
			t.Fatal("invalid record layout")
		}
	}
	if !strings.HasSuffix(codexRecordRoot("/tmp/custom/sessions/2026/09/09/rollout-id.jsonl"), "/sessions") {
		t.Fatal("valid custom root rejected")
	}
}

func TestNearestProviderTraversesIndependentWrapperBranches(t *testing.T) {
	nodes := map[int]processNode{1: {PID: 1}, 2: {PID: 2, PPID: 1, Provider: "codex", State: "S"}, 3: {PID: 3, PPID: 1}, 4: {PID: 4, PPID: 3, Provider: "claude", State: "S+"}, 5: {PID: 5, PPID: 2, Provider: "codex", State: "R+"}}
	if pid, status := nearestSessionProvider(nodes, processChildren(nodes), 1); pid != 4 || status != sessionBindingReady {
		t.Fatal("foreground wrapper branch missed or nested agent selected")
	}
	for pid := 6; pid < maximumTreeNodes+10; pid++ {
		nodes[pid] = processNode{PID: pid, PPID: 1}
	}
	if _, status := nearestSessionProvider(nodes, processChildren(nodes), 1); status != sessionBindingAmbiguous {
		t.Fatal("truncated tree accepted")
	}
}

func TestResumeReferenceRequiresStableExactBinding(t *testing.T) {
	a := sessionBinding{provider: "codex", providerPID: 2, filePID: 2, root: "/synthetic/.codex/sessions", path: "/synthetic/record", recordID: "test-id", status: sessionBindingReady}
	first := map[int]sessionBinding{1: a}
	got := stableResumeReferences(first, first)
	if got[1].ConfigDir != "/synthetic/.codex" || got[1].SessionID != "test-id" {
		t.Fatal("resume identity lost")
	}
	b := a
	b.recordID = "replacement"
	if len(stableResumeReferences(first, map[int]sessionBinding{1: b})) != 0 {
		t.Fatal("replacement persisted")
	}
	a.status = sessionBindingAmbiguous
	if len(stableResumeReferences(map[int]sessionBinding{1: a}, first)) != 0 {
		t.Fatal("ambiguous binding persisted")
	}
	for _, r := range []ResumeReference{{Provider: "sh", SessionID: "abc", ConfigDir: "/tmp"}, {Provider: "codex", SessionID: "--last", ConfigDir: "/tmp"}, {Provider: "claude", SessionID: "abc", ConfigDir: "relative"}} {
		if r.Validate() == nil {
			t.Fatal("unsafe resume reference accepted")
		}
	}
}
