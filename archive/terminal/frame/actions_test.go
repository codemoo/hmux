package frame

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/codemoo/hmux/internal/tabstate"
)

func TestOuterFrameTabsSwitchAndCloseWithoutTargetDetach(t *testing.T) {
	root := t.TempDir()
	binDir := filepath.Join(root, "bin")
	if err := os.Mkdir(binDir, 0o700); err != nil {
		t.Fatal(err)
	}
	tmuxPath := filepath.Join(binDir, "tmux")
	script := `#!/bin/sh
printf '%s\n' "$*" >>"$HMUX_TEST_TMUX_LOG"
case "$1" in
list-sessions)
  printf '$1|:hmux-sep-v1:|one|:hmux-sep-v1:|1700000000|:hmux-sep-v1:|1700000100|:hmux-sep-v1:|1|:hmux-sep-v1:|1|:hmux-sep-v1:||:hmux-sep-v1:|\n'
  printf '$2|:hmux-sep-v1:|two|:hmux-sep-v1:|1700000001|:hmux-sep-v1:|1700000101|:hmux-sep-v1:|1|:hmux-sep-v1:|1|:hmux-sep-v1:||:hmux-sep-v1:|\n'
  ;;
list-windows)
  printf '$1|:hmux-sep-v1:|shell|:hmux-sep-v1:|1|:hmux-sep-v1:|/tmp|:hmux-sep-v1:|zsh|:hmux-sep-v1:|100|:hmux-sep-v1:|30|:hmux-sep-v1:|\n'
  printf '$2|:hmux-sep-v1:|shell|:hmux-sep-v1:|1|:hmux-sep-v1:|/tmp|:hmux-sep-v1:|zsh|:hmux-sep-v1:|100|:hmux-sep-v1:|30|:hmux-sep-v1:|\n'
  ;;
refresh-client) ;;
set-option) ;;
switch-client) ;;
*) exit 91 ;;
esac
`
	if err := os.WriteFile(tmuxPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	logPath := filepath.Join(root, "tmux.log")
	t.Setenv("PATH", binDir+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_TEST_TMUX_LOG", logPath)
	t.Setenv("TMUX", "/tmp/disposable-outer,1,0")
	t.Setenv("TMUX_PANE", "%1")

	stateDir := filepath.Join(root, "state")
	store := tabstate.Store{StateDir: stateDir}
	const launcher = "0123456789abcdef0123456789abcdef"
	const clientName = "/dev/ttys505"
	const controlName = "/dev/ttys606"
	for _, id := range []string{"$1", "$2"} {
		if err := store.OpenFrame(launcher, clientName, controlName, id); err != nil {
			t.Fatal(err)
		}
	}
	workflowDir := filepath.Join(stateDir, "workflows")
	if err := os.MkdirAll(workflowDir, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(workflowDir, "state.json"), []byte("{not-json\n"), 0o600); err != nil {
		t.Fatal(err)
	}

	status, err := Status(t.Context(), stateDir, launcher)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(status, " 1 one ") || !strings.Contains(status, " 2 two ") {
		t.Fatalf("tabs=%q", status)
	}
	if result, err := Action(t.Context(), stateDir, launcher, "3"); err != nil {
		t.Fatal(err)
	} else if result != actionStay {
		t.Fatalf("missing tab action=%d", result)
	}
	statusFile := filepath.Join(
		stateDir, "frames", launcher+"-123.status",
	)
	if err := Click(
		t.Context(), stateDir, launcher, statusFile, "tab1",
	); err != nil {
		t.Fatal(err)
	}
	frameState, err := store.Frame(launcher)
	if err != nil {
		t.Fatal(err)
	}
	if frameState.CurrentID != "$1" {
		t.Fatalf("current=%q", frameState.CurrentID)
	}
	if result, err := Action(t.Context(), stateDir, launcher, "close"); err != nil {
		t.Fatal(err)
	} else if result != actionStay {
		t.Fatalf("multi-tab close action=%d", result)
	}
	frameState, err = store.Frame(launcher)
	if err != nil {
		t.Fatal(err)
	}
	if frameState.CurrentID != "$2" || len(frameState.Sessions) != 1 {
		t.Fatalf("frame=%#v", frameState)
	}
	status, err = Status(t.Context(), stateDir, launcher)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(status, " one ") || !strings.Contains(status, " 1 two ") {
		t.Fatalf("closed tab remained in status=%q", status)
	}
	if err := Click(
		t.Context(), stateDir, launcher, statusFile, "list",
	); err != nil {
		t.Fatal(err)
	}
	if status, ready := readReadyStatus(statusFile); !ready || status != 0 {
		t.Fatalf("click list status=%d ready=%t", status, ready)
	}
	if err := Click(
		t.Context(), stateDir, launcher,
		filepath.Join(stateDir, "outside", launcher+"-123.status"),
		"list",
	); err == nil {
		t.Fatal("click accepted a status file outside the private frames directory")
	}
	if result, err := Action(t.Context(), stateDir, launcher, "list"); err != nil {
		t.Fatal(err)
	} else if result != actionLeave {
		t.Fatalf("list action=%d", result)
	}
	if result, err := Action(t.Context(), stateDir, launcher, "quit"); err != nil {
		t.Fatal(err)
	} else if result != actionQuit {
		t.Fatalf("quit action=%d", result)
	}
	if result, err := Action(t.Context(), stateDir, launcher, "close"); err != nil {
		t.Fatal(err)
	} else if result != actionClose {
		t.Fatalf("last-tab close action=%d", result)
	}
	if _, err := store.Frame(launcher); err == nil {
		t.Fatal("last-tab close left an active frame")
	}
	log, err := os.ReadFile(logPath)
	if err != nil {
		t.Fatal(err)
	}
	text := string(log)
	if !strings.Contains(text, "switch-client -c "+clientName+" -t $1") ||
		!strings.Contains(text, "switch-client -c "+clientName+" -t $2") {
		t.Fatalf("switch calls missing:\n%s", text)
	}
	refresh := strings.Index(text, "refresh-client -t "+controlName+" -S")
	firstSwitch := strings.Index(text, "switch-client -c "+clientName+" -t $1")
	if refresh < 0 || firstSwitch < 0 || refresh > firstSwitch {
		t.Fatalf("visible header was not refreshed before target switch:\n%s", text)
	}
	if strings.Contains(text, "detach-client") || strings.Contains(text, "kill-") ||
		strings.Contains(text, "set-option -t") {
		t.Fatalf("frame action mutated target tmux state:\n%s", text)
	}
	for _, line := range strings.Split(text, "\n") {
		if strings.HasPrefix(line, "set-option ") &&
			!strings.HasPrefix(line, "set-option -g @hmux_frame_tab1 ") {
			t.Fatalf("unexpected disposable frame option update:\n%s", text)
		}
	}
}
