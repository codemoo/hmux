package main

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestWorkflowHookIsFailOpenAndPersistsOnlyHashedMetadata(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("HMUX_TMUX_SESSION_ID", "$4")
	t.Setenv("HMUX_TMUX_SESSION_CREATED_AT", "1700000000")
	input := `{"session_id":"raw-session","turn_id":"raw-turn","hook_event_name":"UserPromptSubmit","prompt":"private prompt"}`
	var output bytes.Buffer
	workflowHook(strings.NewReader(input), &output)
	if output.String() != "{}\n" {
		t.Fatalf("hook output=%q", output.String())
	}
	data, err := os.ReadFile(filepath.Join(home, ".local", "state", "hmux", "workflows", "state.json"))
	if err != nil {
		t.Fatal(err)
	}
	for _, forbidden := range []string{"raw-session", "raw-turn", "private prompt"} {
		if strings.Contains(string(data), forbidden) {
			t.Fatalf("hook persisted %q in %s", forbidden, data)
		}
	}
	output.Reset()
	workflowHook(strings.NewReader("not-json"), &output)
	if output.String() != "{}\n" {
		t.Fatalf("malformed hook output=%q", output.String())
	}
}

func TestParseAttachAcceptsOnlyValidatedLauncherID(t *testing.T) {
	id, launcher, createdAt, shared, detach, appView, err := parseAttach([]string{
		"--shared", "--launcher", "0123456789abcdef0123456789abcdef", "$7",
	})
	if err != nil || id != "$7" || launcher == "" || createdAt != 0 || !shared || detach || appView {
		t.Fatalf("id=%q launcher=%q createdAt=%d shared=%t detach=%t err=%v", id, launcher, createdAt, shared, detach, err)
	}
	if _, _, _, _, _, _, err := parseAttach([]string{"--launcher", "../bad", "$7"}); err == nil {
		t.Fatal("unsafe launcher ID accepted")
	}
	if _, _, _, _, _, _, err := parseAttach([]string{"--launcher"}); err == nil {
		t.Fatal("missing launcher ID accepted")
	}
	id, launcher, createdAt, shared, detach, appView, err = parseAttach([]string{
		"--shared", "--app-view", "--created-at", "1700000000", "$7",
	})
	if err != nil || id != "$7" || launcher != "" || createdAt != 1700000000 || !shared || detach || !appView {
		t.Fatalf("expected attach parse failed: id=%q createdAt=%d err=%v", id, createdAt, err)
	}
}

func TestReadBoundedSingleLineRejectsExtraLinesAndOversizedInput(t *testing.T) {
	got, err := readBoundedSingleLine(strings.NewReader("friendly alias\n"), 32)
	if err != nil || got != "friendly alias" {
		t.Fatalf("alias=%q err=%v", got, err)
	}
	for _, input := range []string{"first\nsecond\n", strings.Repeat("x", 34)} {
		if _, err := readBoundedSingleLine(strings.NewReader(input), 32); err == nil {
			t.Fatalf("unsafe input accepted: %q", input)
		}
	}
}

func TestParseCreateNameStdinIsExclusive(t *testing.T) {
	profile, name, inventory, dryRun, nameStdin, jsonOutput, err := parseCreate([]string{
		"--json", "--name-stdin", "codex",
	})
	if err != nil || profile != "codex" || name != "" || inventory == "" || dryRun || !nameStdin || !jsonOutput {
		t.Fatalf(
			"profile=%q name=%q inventory=%q dryRun=%t nameStdin=%t json=%t err=%v",
			profile, name, inventory, dryRun, nameStdin, jsonOutput, err,
		)
	}
	if _, _, _, _, _, _, err := parseCreate([]string{
		"--name", "one", "--name-stdin", "codex",
	}); err == nil {
		t.Fatal("--name and --name-stdin were accepted together")
	}
	if _, _, _, _, _, _, err := parseCreate([]string{"--json", "--dry-run", "codex"}); err == nil {
		t.Fatal("--json and --dry-run were accepted together")
	}
}
