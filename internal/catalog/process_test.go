package catalog

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/codemoo/hmux/internal/model"
)

func TestProcessTreeDetectsRealAgentsAndGenericProcesses(t *testing.T) {
	raw := []byte(strings.Join([]string{
		"100 1 Ss 0.0 -zsh",
		"101 100 S+ 0.0 node",
		"102 101 S+ 1.2 /opt/tool/@openai/codex/vendor/bin/codex",
		"103 102 S 0.0 codex-code-mode-host",
		"200 1 Ss 0.0 -zsh",
		"201 200 S+ 0.0 claude",
		"202 201 S 4.0 node",
		"300 1 Ss 0.0 -zsh",
		"301 300 S+ 0.0 uv",
		"302 301 S+ 3.4 /opt/python/bin/Python",
	}, "\n"))
	nodes, err := parseProcessTable(raw)
	if err != nil {
		t.Fatal(err)
	}
	children := processChildren(nodes)
	for panePID, runtimeName := range map[int]string{100: "codex", 200: "claude"} {
		candidate, ok := selectProcessCandidate(nodes, children, panePID)
		if !ok || candidate.Node.Provider != runtimeName {
			t.Fatalf("pane=%d candidate=%#v ok=%v", panePID, candidate, ok)
		}
	}
	generic, ok := selectProcessCandidate(nodes, children, 300)
	if !ok || generic.Node.Provider != "" || generic.Node.Process != "Python" {
		t.Fatalf("generic candidate=%#v ok=%v", generic, ok)
	}
}

func TestProcessInspectorHonorsCanceledContextBeforeSnapshot(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	_, err := (systemProcessInspector{PSPath: "/path/that/must/not/run"}).Inspect(ctx, []int{1})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("error=%v", err)
	}
}

func TestProcessChildrenSharedAcrossLargePaneSet(t *testing.T) {
	nodes := make(map[int]processNode, 5001)
	nodes[1] = processNode{PID: 1, Process: "zsh"}
	for pid := 2; pid <= 5001; pid++ {
		nodes[pid] = processNode{PID: pid, PPID: pid - 1, Process: "worker"}
	}
	children := processChildren(nodes)
	candidate, ok := selectProcessCandidate(nodes, children, 1)
	if !ok || candidate.Node.PID != 5001 {
		t.Fatalf("candidate=%#v ok=%v", candidate, ok)
	}
}

func TestClassificationDoesNotTrustSessionNames(t *testing.T) {
	session := model.Session{
		Name:           "codex-looking-name",
		Profile:        "claude",
		CurrentCommand: "node",
	}
	classify(&session, processMetadata{})
	if session.Runtime != "process" || session.Kind != "shell" || session.Process != "node" {
		t.Fatalf("name-based misclassification: %#v", session)
	}
	classify(&session, processMetadata{
		Runtime: "codex", Model: "gpt-5.6-sol", State: "working", Process: "codex", WorkingSince: 1700000000,
	})
	if session.Runtime != "codex" || session.Kind != "codex" ||
		session.Model != "gpt-5.6-sol" || session.State != "working" ||
		session.WorkingSince != 1700000000 {
		t.Fatalf("actual process metadata was not applied: %#v", session)
	}
}

func TestCodexEventReaderUsesOnlyValidatedMetadata(t *testing.T) {
	home := t.TempDir()
	root := filepath.Join(home, ".codex", "sessions")
	if err := os.MkdirAll(root, 0o700); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(root, "rollout-2026-07-29T00-00-00-session.jsonl")
	content := strings.Join([]string{
		`{"type":"turn_context","payload":{"model":"gpt-5.6-sol","user_prompt":"not inspected"}}`,
		`{"timestamp":"2026-07-29T01:02:03.456Z","type":"event_msg","payload":{"type":"task_started","message":"not inspected"}}`,
		`{"type":"turn_context","payload":{"model":"unsafe model;token"}}`,
		`{"type":"event_msg","payload":{"type":"task_complete"}}`,
	}, "\n")
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
	detectedModel, state, workingSince := readCodexEvents(path, root)
	if detectedModel != "gpt-5.6-sol" || state != "idle" || workingSince != 0 {
		t.Fatalf("model=%q state=%q workingSince=%d", detectedModel, state, workingSince)
	}

	workingPath := filepath.Join(root, "rollout-2026-07-29T00-00-01-session.jsonl")
	workingContent := strings.Join([]string{
		`{"type":"turn_context","payload":{"model":"gpt-5.6-sol"}}`,
		`{"timestamp":"2026-07-29T01:02:03.456Z","type":"event_msg","payload":{"type":"task_started"}}`,
	}, "\n")
	if err := os.WriteFile(workingPath, []byte(workingContent), 0o600); err != nil {
		t.Fatal(err)
	}
	_, state, workingSince = readCodexEvents(workingPath, root)
	if state != "working" || workingSince != 1785286923 {
		t.Fatalf("state=%q workingSince=%d", state, workingSince)
	}
}

func TestClaudeMetadataMapsPIDToSessionModelAndStatus(t *testing.T) {
	home := t.TempDir()
	sessionRoot := filepath.Join(home, ".claude", "sessions")
	projectDir := filepath.Join(home, ".claude", "projects", "-work-project")
	if err := os.MkdirAll(sessionRoot, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(projectDir, 0o700); err != nil {
		t.Fatal(err)
	}
	const pid = 321
	const sessionID = "34c08042-3a7b-464f-8c75-01289d6d2aac"
	session := `{"pid":321,"sessionId":"` + sessionID + `","status":"busy","statusUpdatedAt":1700000000123,"cwd":"/not/exposed"}`
	if err := os.WriteFile(filepath.Join(sessionRoot, "321.json"), []byte(session), 0o600); err != nil {
		t.Fatal(err)
	}
	events := strings.Join([]string{
		`{"type":"assistant","message":{"model":"claude-sonnet-4-5","content":"not inspected"}}`,
		`{"type":"assistant","message":{"model":"claude-opus-5","content":"not inspected"}}`,
	}, "\n")
	if err := os.WriteFile(filepath.Join(projectDir, sessionID+".jsonl"), []byte(events), 0o600); err != nil {
		t.Fatal(err)
	}
	binding := bindClaudeSession(home, sessionBinding{provider: "claude", providerPID: pid})
	detectedModel, state, workingSince := binding.model, binding.state, binding.workingSince
	if detectedModel != "claude-opus-5" || state != "working" || workingSince != 1700000000 {
		t.Fatalf("model=%q state=%q workingSince=%d", detectedModel, state, workingSince)
	}
}

func TestMetadataValidationRejectsSecretLikeOrEscapingValues(t *testing.T) {
	for _, value := range []string{"", "--token=secret value", "../model", "model\nsecret"} {
		if got := validatedModel(value); got != "" {
			t.Fatalf("unsafe model %q accepted as %q", value, got)
		}
	}
	root := t.TempDir()
	outside := filepath.Join(filepath.Dir(root), "outside.jsonl")
	if safeEventFile(outside, root) {
		t.Fatal("event path outside metadata root was accepted")
	}
	outsideDir := t.TempDir()
	outsideEvent := filepath.Join(outsideDir, "event.jsonl")
	if err := os.WriteFile(outsideEvent, []byte("{}\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(root, "linked")
	if err := os.Symlink(outsideDir, link); err != nil {
		t.Fatal(err)
	}
	if safeEventFile(filepath.Join(link, "event.jsonl"), root) {
		t.Fatal("event path escaping through an intermediate symlink was accepted")
	}
}
