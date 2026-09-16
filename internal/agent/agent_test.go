package agent

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/codemoo/hmux/internal/model"
)

func TestShellCommandQuotesEveryArgument(t *testing.T) {
	got := shellCommand([]string{"/usr/bin/tool", "plain", "has space", "x'; touch /tmp/nope"})
	want := "'/usr/bin/tool' 'plain' 'has space' 'x'\"'\"'; touch /tmp/nope'"
	if got != want {
		t.Fatalf("got %q want %q", got, want)
	}
}

func TestValidateCreateDoesNotRequireOrInvokeTmux(t *testing.T) {
	inventory := model.Inventory{
		SchemaVersion: model.SchemaVersion,
		Clients:       []model.Client{{ID: "home-mac", Role: "home"}},
		IdentityRefs:  []model.IdentityRef{{ID: "key", Path: "~/.ssh/test_key"}},
		Hosts: []model.Host{{
			ID: "home", SSHAlias: "hmux-home", Address: "home.invalid",
			User: "user", Port: 22, IdentityRef: "key",
		}},
		Profiles: []model.Profile{{
			ID: "shell", Label: "Shell", DefaultDirectory: t.TempDir(),
			Command: []string{"sh", "-l"},
		}},
	}
	name, err := ValidateCreate(inventory, "shell", "hmux-e2e-dry-run")
	if err != nil {
		t.Fatal(err)
	}
	if name != "hmux-e2e-dry-run" {
		t.Fatalf("name=%q", name)
	}
}

func TestSessionNameUnicodeValidation(t *testing.T) {
	for _, name := range []string{"한글 세션-01", "Codex_Main"} {
		if !validSessionName(name) {
			t.Errorf("valid session name %q was rejected", name)
		}
	}
	for _, name := range []string{"bad:name", "bad.name", "bad\nname", "emoji🚀"} {
		if validSessionName(name) {
			t.Errorf("unsafe session name %q was accepted", name)
		}
	}
}

func TestAutomaticSessionNamesAreDistinctAndBounded(t *testing.T) {
	profileID := strings.Repeat("a", 63)
	inventory := model.Inventory{Profiles: []model.Profile{{
		ID: profileID, DefaultDirectory: t.TempDir(), Command: []string{"sh"},
	}}}
	first, err := ValidateCreate(inventory, profileID, "")
	if err != nil {
		t.Fatal(err)
	}
	second, err := ValidateCreate(inventory, profileID, "")
	if err != nil {
		t.Fatal(err)
	}
	if first == second || !validSessionName(first) || !validSessionName(second) {
		t.Fatalf("automatic names must be distinct and valid: %q, %q", first, second)
	}
}

func TestPreviewIncludesRuntimeModelAndStateWithoutPaneContent(t *testing.T) {
	preview := FormatPreview(model.Session{
		ID: "$7", Name: "main\x1b[31m\u202eevil", Runtime: "codex", Model: "gpt-5.6-sol",
		State: "working", Process: "codex", CurrentPath: "/work/project",
	})
	for _, value := range []string{
		"Runtime: codex", "Model: gpt-5.6-sol", "State: working", "Process: codex",
	} {
		if !strings.Contains(preview, value) {
			t.Fatalf("preview missing %q: %s", value, preview)
		}
	}
	if strings.Contains(preview, "\x1b") || strings.Contains(preview, "\u202e") {
		t.Fatal("preview retained terminal-control text")
	}
}

func TestCatalogIgnoresCorruptOptionalWorkflowState(t *testing.T) {
	dir := t.TempDir()
	tmuxPath := filepath.Join(dir, "tmux")
	tmuxScript := `#!/bin/sh
case "$1" in
list-sessions)
	printf '%s\n' '$41|:hmux-sep-v1:|workflow-test|:hmux-sep-v1:|1700000000|:hmux-sep-v1:|1700000001|:hmux-sep-v1:|0|:hmux-sep-v1:|1|:hmux-sep-v1:||:hmux-sep-v1:|'
	;;
list-windows)
	printf '%s\n' '$41|:hmux-sep-v1:|main|:hmux-sep-v1:|1|:hmux-sep-v1:|/tmp|:hmux-sep-v1:|zsh|:hmux-sep-v1:|120|:hmux-sep-v1:|40|:hmux-sep-v1:|1'
	;;
*) exit 99 ;;
esac
`
	if err := os.WriteFile(tmuxPath, []byte(tmuxScript), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", dir)
	stateDir := filepath.Join(dir, "state")
	workflowDir := filepath.Join(stateDir, "workflows")
	if err := os.MkdirAll(workflowDir, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(workflowDir, "state.json"), []byte("{not-json\n"), 0o600); err != nil {
		t.Fatal(err)
	}

	value, err := CatalogAt(t.Context(), stateDir)
	if err != nil {
		t.Fatalf("optional workflow state broke catalog: %v", err)
	}
	if len(value.Sessions) != 1 || value.Sessions[0].ID != "$41" {
		t.Fatalf("unexpected catalog: %+v", value.Sessions)
	}
	if value.Sessions[0].Workflow != nil || len(value.Sessions[0].Workflows) != 0 {
		t.Fatalf("corrupt workflow state was exposed: %+v", value.Sessions[0])
	}
}

func TestCreateFailureNeverKillsTheNewTmuxSession(t *testing.T) {
	dir := t.TempDir()
	logPath := filepath.Join(dir, "tmux.log")
	tmuxPath := filepath.Join(dir, "tmux")
	tmuxScript := `#!/bin/sh
printf '%s\n' "$*" >>"$HMUX_TEST_TMUX_LOG"
case "$1" in
has-session) exit 1 ;;
new-session) printf '$42 1700000000\n' ;;
display-message) printf '$42 1700000000\n' ;;
set-option) exit 7 ;;
*) exit 99 ;;
esac
`
	if err := os.WriteFile(tmuxPath, []byte(tmuxScript), 0o700); err != nil {
		t.Fatal(err)
	}
	commandPath := filepath.Join(dir, "hmux-e2e-command")
	if err := os.WriteFile(commandPath, []byte("#!/bin/sh\nexit 0\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", dir)
	t.Setenv("HMUX_TEST_TMUX_LOG", logPath)
	inventory := model.Inventory{Profiles: []model.Profile{{
		ID: "shell", Label: "Shell", DefaultDirectory: dir,
		Command: []string{"hmux-e2e-command"},
	}}}
	stateDir := filepath.Join(dir, "invalid-state")
	if err := os.WriteFile(stateDir, []byte("not a directory"), 0o600); err != nil {
		t.Fatal(err)
	}
	_, err := Create(t.Context(), inventory, "shell", "hmux-e2e-create-no-kill", stateDir)
	if err == nil || !strings.Contains(err.Error(), "was left running") {
		t.Fatalf("error=%v", err)
	}
	log, readErr := os.ReadFile(logPath)
	if readErr != nil {
		t.Fatal(readErr)
	}
	if strings.Contains(string(log), "kill-session") {
		t.Fatalf("hmux attempted to kill a tmux session:\n%s", log)
	}
}

func TestCreateSessionReturnsAuthoritativeIdentityForNewAndReusedSessions(t *testing.T) {
	for _, reused := range []bool{false, true} {
		t.Run(map[bool]string{false: "new", true: "reused"}[reused], func(t *testing.T) {
			dir := t.TempDir()
			logPath := filepath.Join(dir, "tmux.log")
			tmuxPath := filepath.Join(dir, "tmux")
			tmuxScript := `#!/bin/sh
set -eu
printf '%s\n' "$*" >>"$HMUX_TEST_TMUX_LOG"
sep='|:hmux-sep-v1:|'
case "$1" in
has-session)
	[ "${HMUX_TEST_REUSED:-0}" = 1 ]
	;;
new-session) printf '$42 1700000000\n' ;;
display-message)
	[ "$4" = "=$HMUX_TEST_SESSION_NAME:" ]
	printf '$42 1700000000\n'
	;;
list-sessions)
	printf '$42%s%s%s1700000000%s1700000001%s0%s1%s%s\n' "$sep" "$HMUX_TEST_SESSION_NAME" "$sep" "$sep" "$sep" "$sep" "$sep" "$sep"
	;;
list-windows)
	printf '$42%smain%s1%s%s%shmux-e2e-command%s120%s40%s1\n' "$sep" "$sep" "$sep" "$HMUX_TEST_DIRECTORY" "$sep" "$sep" "$sep" "$sep"
	;;
*) exit 99 ;;
esac
`
			if err := os.WriteFile(tmuxPath, []byte(tmuxScript), 0o700); err != nil {
				t.Fatal(err)
			}
			commandPath := filepath.Join(dir, "hmux-e2e-command")
			if err := os.WriteFile(commandPath, []byte("#!/bin/sh\nexit 0\n"), 0o700); err != nil {
				t.Fatal(err)
			}
			t.Setenv("PATH", dir)
			t.Setenv("HMUX_TEST_TMUX_LOG", logPath)
			t.Setenv("HMUX_TEST_DIRECTORY", dir)
			t.Setenv("HMUX_TEST_SESSION_NAME", "hmux-e2e-authoritative")
			if reused {
				t.Setenv("HMUX_TEST_REUSED", "1")
			}
			inventory := model.Inventory{Profiles: []model.Profile{{
				ID: "shell", Label: "Shell", DefaultDirectory: dir,
				Command: []string{"hmux-e2e-command"}, Tags: []string{"local"},
			}}}
			stateDir := filepath.Join(dir, "state")
			result, err := CreateSession(
				t.Context(), inventory, "shell", "hmux-e2e-authoritative", stateDir,
			)
			if err != nil {
				t.Fatal(err)
			}
			if result.ID != "$42" || result.CreatedAt != 1700000000 || result.Reused != reused {
				t.Fatalf("result=%+v", result)
			}
			creationLog, err := os.ReadFile(logPath)
			if err != nil {
				t.Fatal(err)
			}
			if strings.Contains(string(creationLog), "list-sessions") || (!reused && strings.Contains(string(creationLog), "display-message")) {
				t.Fatalf("create reconstructed identity after creation: %s", creationLog)
			}
			value, err := CatalogAt(t.Context(), stateDir)
			if err != nil {
				t.Fatal(err)
			}
			expectedProfile := "shell"
			if reused {
				expectedProfile = ""
			}
			if len(value.Sessions) != 1 || value.Sessions[0].Profile != expectedProfile {
				t.Fatalf("profile metadata was not authoritative: %+v", value.Sessions)
			}
			logData, err := os.ReadFile(logPath)
			if err != nil {
				t.Fatal(err)
			}
			created := strings.Contains(string(logData), "new-session")
			if created == reused {
				t.Fatalf("reused=%t tmux log=%s", reused, logData)
			}
		})
	}
}
