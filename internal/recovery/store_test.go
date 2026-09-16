package recovery

import (
	"context"
	"fmt"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"github.com/codemoo/hmux/internal/catalog"
)

type runnerStep struct {
	command string
	check   func(*testing.T, []string)
	output  string
	err     error
}

type scriptedRunner struct {
	t     *testing.T
	steps []runnerStep
	calls [][]string
}

func (r *scriptedRunner) Output(_ context.Context, args ...string) ([]byte, error) {
	r.t.Helper()
	r.calls = append(r.calls, append([]string(nil), args...))
	if len(r.steps) == 0 {
		r.t.Fatalf("unexpected tmux call: %q", args)
	}
	step := r.steps[0]
	r.steps = r.steps[1:]
	if len(args) == 0 || args[0] != step.command {
		r.t.Fatalf("tmux command = %q, want %q", args, step.command)
	}
	if step.check != nil {
		step.check(r.t, args)
	}
	return []byte(step.output), step.err
}

func (r *scriptedRunner) done() {
	r.t.Helper()
	if len(r.steps) != 0 {
		r.t.Fatalf("%d expected tmux calls were not made", len(r.steps))
	}
}

func captureSteps(pid int, sessionID, name string, created int64, paneID string) []runnerStep {
	sep := "|:hmux-sep-v1:|"
	session := strings.Join([]string{sessionID, name, fmt.Sprint(created), fmt.Sprint(created), "0", "1", "", "0"}, sep) + "\n"
	window := strings.Join([]string{sessionID, "editor", "1", "/tmp", "zsh", "80", "24", fmt.Sprint(pid)}, sep) + "\n"
	pane := strings.Join([]string{sessionID, "@2", "0", "editor", "b1e2,80x24,0,0,2", "1", paneID, "0", "1", "/tmp", fmt.Sprint(pid)}, recoverySeparator) + "\n"
	return []runnerStep{
		{command: "list-sessions", output: session},
		{command: "list-windows", output: window},
		{command: "list-panes", output: pane},
		{command: "list-sessions", output: session},
		{command: "list-windows", output: window},
		{command: "list-panes", output: pane},
	}
}

func TestSyncAndSaveRefreshResumeReference(t *testing.T) {
	stateDir := filepath.Join(t.TempDir(), "state")
	runner := &scriptedRunner{t: t, steps: captureSteps(101, "$1", "work", 100, "%3")}
	reference := catalog.ResumeReference{Provider: "codex", SessionID: "018e1234-abcd", ConfigDir: "/tmp/codex-a"}
	store := Store{
		StateDir: stateDir, Runner: runner,
		BootID: func(context.Context) (string, error) { return "boot-a", nil },
		Bind: func(_ context.Context, pids []int) (map[int]catalog.ResumeReference, error) {
			if !reflect.DeepEqual(pids, []int{101}) {
				t.Fatalf("binding pids = %v", pids)
			}
			return map[int]catalog.ResumeReference{101: reference}, nil
		},
	}
	if err := store.Sync(context.Background()); err != nil {
		t.Fatal(err)
	}
	runner.done()
	current, err := store.readState()
	if err != nil {
		t.Fatal(err)
	}
	if got := current.Checkpoint.Sessions[0].Windows[0].Panes[0].Resume; got == nil || *got != reference {
		t.Fatalf("initial resume reference = %#v", got)
	}

	// A new stable provider ID replaces the old one.
	reference.SessionID = "018e5678-efab"
	runner.steps = captureSteps(101, "$1", "work", 100, "%3")
	if err := store.Save(context.Background()); err != nil {
		t.Fatal(err)
	}
	current, _ = store.readState()
	if got := current.Checkpoint.Sessions[0].Windows[0].Panes[0].Resume; got == nil || got.SessionID != reference.SessionID {
		t.Fatalf("refreshed resume reference = %#v", got)
	}

	// Missing or ambiguous resolution is authoritative and never carries the
	// previous ID forward during an ordinary save.
	store.Bind = func(context.Context, []int) (map[int]catalog.ResumeReference, error) {
		return map[int]catalog.ResumeReference{}, nil
	}
	runner.steps = captureSteps(101, "$1", "work", 100, "%3")
	if err := store.Save(context.Background()); err != nil {
		t.Fatal(err)
	}
	current, _ = store.readState()
	if got := current.Checkpoint.Sessions[0].Windows[0].Panes[0].Resume; got != nil {
		t.Fatalf("stale resume reference retained: %#v", got)
	}
}

func TestSaveCannotAdvanceAcrossBoot(t *testing.T) {
	stateDir := filepath.Join(t.TempDir(), "state")
	runner := &scriptedRunner{t: t, steps: captureSteps(101, "$1", "work", 100, "%3")}
	boot := "boot-a"
	store := Store{
		StateDir: stateDir, Runner: runner,
		BootID: func(context.Context) (string, error) { return boot, nil },
		Bind: func(context.Context, []int) (map[int]catalog.ResumeReference, error) {
			return map[int]catalog.ResumeReference{}, nil
		},
	}
	if err := store.Sync(context.Background()); err != nil {
		t.Fatal(err)
	}
	boot = "boot-b"
	if err := store.Save(context.Background()); err == nil || !strings.Contains(err.Error(), "boot synchronization") {
		t.Fatalf("Save across reboot error = %v", err)
	}
	if len(runner.steps) != 0 {
		t.Fatal("Save queried tmux before rejecting an unsynchronized boot")
	}
	current, _ := store.readState()
	if current.BootID != "boot-a" || len(current.Checkpoint.Sessions) != 1 {
		t.Fatalf("checkpoint was erased across boot: %+v", current)
	}
}

func TestProviderCommandsAreFixedArgumentVectors(t *testing.T) {
	codex := savedPane{Cwd: "/tmp", Resume: &catalog.ResumeReference{
		Provider: "codex", SessionID: "018e1234-abcd", ConfigDir: "/tmp/codex config",
	}}
	got := appendPaneCommand([]string{"respawn-pane", "-t", "%7"}, codex, map[string]string{"codex": "/trusted/codex"})
	want := []string{"respawn-pane", "-t", "%7", "-e", "CODEX_HOME=/tmp/codex config", "/trusted/codex", "resume", "018e1234-abcd"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("codex resume argv = %#v, want %#v", got, want)
	}
	claude := savedPane{Cwd: "/tmp", Resume: &catalog.ResumeReference{
		Provider: "claude", SessionID: "018e5678-efab", ConfigDir: "/tmp/claude config",
	}}
	got = appendPaneCommand([]string{"respawn-pane", "-t", "%8"}, claude, map[string]string{"claude": "/trusted/claude"})
	want = []string{"respawn-pane", "-t", "%8", "-e", "CLAUDE_CONFIG_DIR=/tmp/claude config", "/trusted/claude", "--resume", "018e5678-efab"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("claude resume argv = %#v, want %#v", got, want)
	}
}

func TestStartEmptyServerLeavesExistingSessionsUntouched(t *testing.T) {
	steps := captureSteps(101, "$1", "existing", 100, "%3")
	runner := &scriptedRunner{t: t, steps: steps[:2]}
	if err := (Store{Runner: runner}).startEmptyServer(t.Context()); err != nil {
		t.Fatal(err)
	}
	runner.done()
}

func TestStartEmptyServerPropagatesReadFailure(t *testing.T) {
	runner := &scriptedRunner{t: t, steps: []runnerStep{{command: "list-sessions", err: fmt.Errorf("synthetic access denied")}}}
	if err := (Store{Runner: runner}).startEmptyServer(t.Context()); err == nil {
		t.Fatal("read failure triggered start")
	}
	runner.done()
}

func TestStartEmptyServerCreatesDetachedShell(t *testing.T) {
	runner := &scriptedRunner{t: t, steps: []runnerStep{
		{command: "list-sessions"}, {command: "list-windows"},
		{command: "new-session", check: func(t *testing.T, args []string) {
			if len(args) != 8 || !reflect.DeepEqual(args[:5], []string{"new-session", "-d", "-s", "hmux", "-c"}) || args[7] != "-l" {
				t.Fatalf("unexpected startup argv: %v", args)
			}
		}},
	}}
	if err := (Store{Runner: runner}).startEmptyServer(t.Context()); err != nil {
		t.Fatal(err)
	}
	runner.done()
}
