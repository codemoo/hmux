package agent

import (
	"github.com/codemoo/hmux/internal/sessionstate"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"unicode/utf8"

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

func TestWorkspaceSlugAndConcurrentAllocation(t *testing.T) {
	for input, want := range map[string]string{
		"한글 세션-01": "한글-세션-01", "../My Project/a:1": "My-Project-a-1",
		"../../": "session", "🚀": "session", "  --hello__  ": "hello", "hello;$(touch nope)": "hello-touch-nope",
	} {
		if got := workspaceSlug(input, 36); got != want {
			t.Errorf("%q: got %q, want %q", input, got, want)
		}
	}
	root := t.TempDir()
	// Existing files, folders and links are never reused, followed or overwritten.
	outside := t.TempDir()
	if err := os.Symlink(outside, filepath.Join(root, "project")); err != nil {
		t.Fatal(err)
	}
	var wg sync.WaitGroup
	results := make(chan string, 12)
	for range 12 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			dir, name, err := allocateWorkspace(root, "project", strings.Repeat("p", 63))
			if err != nil {
				t.Error(err)
				return
			}
			if filepath.Dir(dir) != root || dir == filepath.Join(root, "project") || utf8.RuneCountInString(name) > 80 {
				t.Errorf("invalid allocation %q %q", dir, name)
			}
			if !strings.HasPrefix(name, filepath.Base(dir)+"-") {
				t.Error("tmux name missing directory prefix")
			}
			results <- dir
		}()
	}
	wg.Wait()
	close(results)
	seen := map[string]bool{}
	for dir := range results {
		if seen[dir] {
			t.Error("duplicate directory")
		}
		seen[dir] = true
	}
	if len(seen) != 12 {
		t.Fatalf("allocated %d directories", len(seen))
	}
	entries, _ := os.ReadDir(outside)
	if len(entries) != 0 {
		t.Fatal("followed existing child link")
	}
}

func TestCreateValidationIsSideEffectFree(t *testing.T) {
	base := filepath.Join(t.TempDir(), "not-created")
	inventory := model.Inventory{Profiles: []model.Profile{{ID: "shell", DefaultDirectory: base, Command: []string{"sh"}}}}
	for _, input := range []string{"", "한글 project/one", strings.Repeat("가", 80)} {
		name, err := ValidateCreate(inventory, "shell", input)
		if err != nil || name == "" || len(name) > 144 {
			t.Fatalf("name=%q err=%v", name, err)
		}
	}
	if _, err := os.Stat(base); !os.IsNotExist(err) {
		t.Fatal("validation created a directory")
	}
	for _, input := range []string{"bad\nname", strings.Repeat("a", 81), string([]byte{255})} {
		if _, err := ValidateCreate(inventory, "shell", input); err == nil {
			t.Errorf("accepted %q", input)
		}
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

	if _, err := CatalogAt(t.Context(), stateDir); err != nil {
		t.Fatal(err)
	}
	value, err := BasicCatalogAt(t.Context(), stateDir)
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

func TestCreateSessionReturnsAuthoritativeIdentity(t *testing.T) {
	dir := t.TempDir()
	logPath := filepath.Join(dir, "tmux.log")
	script := `#!/bin/sh
printf '%s\n' "$*" >>"$HMUX_TEST_TMUX_LOG"
[ "$1" = new-session ] || exit 99
printf '$42 1700000000\n'
`
	if err := os.WriteFile(filepath.Join(dir, "tmux"), []byte(script), 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "hmux-e2e-command"), []byte("#!/bin/sh\nexit 0\n"), 0700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", dir)
	t.Setenv("HMUX_TEST_TMUX_LOG", logPath)
	inventory := model.Inventory{Profiles: []model.Profile{{ID: "shell", DefaultDirectory: dir, Command: []string{"hmux-e2e-command"}}}}
	stateDir := filepath.Join(dir, "state")
	result, err := CreateSession(t.Context(), inventory, "shell", "hmux-e2e-authoritative", stateDir)
	if err != nil {
		t.Fatal(err)
	}
	if result.ID != "$42" || result.CreatedAt != 1700000000 || result.Reused {
		t.Fatalf("result=%+v", result)
	}
	data, _ := os.ReadFile(logPath)
	if strings.Count(string(data), "new-session") != 1 || strings.Contains(string(data), "display-message") {
		t.Fatalf("non-authoritative lookup: %s", data)
	}
	fields := strings.Fields(string(data))
	name := ""
	for index, field := range fields {
		if field == "-s" && index+1 < len(fields) {
			name = fields[index+1]
		}
	}
	value := model.Catalog{Sessions: []model.Session{{ID: result.ID, Name: name, CreatedAt: result.CreatedAt}}}
	if err := (sessionstate.Store{StateDir: stateDir}).Apply(&value); err != nil {
		t.Fatal(err)
	}
	if value.Sessions[0].Profile != "shell" {
		t.Fatalf("missing metadata: %+v", value)
	}
}
