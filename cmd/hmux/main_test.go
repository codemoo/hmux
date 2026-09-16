package main

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/hmux/archive/terminal/ui"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
)

func TestTimedOutUpdateDoesNotCancelRequestedCommand(t *testing.T) {
	home := t.TempDir()
	binDir := filepath.Join(home, "bin")
	if err := os.MkdirAll(binDir, 0o700); err != nil {
		t.Fatal(err)
	}
	ssh := filepath.Join(binDir, "ssh")
	script := `#!/bin/sh
printf '%s\n' "$*" >>"$HMUX_TEST_LOG"
case "$*" in
  *" catalog" | *" catalog --launcher "*) sleep 0.05; printf '%s\n' '{"protocol_version":1,"generated_at":"2026-07-28T00:00:00Z","sessions":[{"id":"$1","name":"ok"}]}' ;;
  *) exit 1 ;;
esac
`
	if err := os.WriteFile(ssh, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	configDir := filepath.Join(home, ".config", "hmux")
	if err := os.MkdirAll(configDir, 0o700); err != nil {
		t.Fatal(err)
	}
	configPath := filepath.Join(configDir, "client.toml")
	configData := `schema_version = 1
client_id = "office-mac"
role = "remote"
timeout_seconds = 5
update_check = true
`
	if err := os.WriteFile(configPath, []byte(configData), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HOME", home)
	t.Setenv("PATH", binDir+string(os.PathListSeparator)+os.Getenv("PATH"))
	logPath := filepath.Join(home, "ssh.log")
	t.Setenv("HMUX_TEST_LOG", logPath)
	originalAutoUpdateCheck := autoUpdateCheck
	autoUpdateCheck = func(ctx context.Context, _ config.ClientConfig) (bool, error) {
		if _, ok := ctx.Deadline(); !ok {
			t.Fatal("automatic update context has no deadline")
		}
		return false, context.DeadlineExceeded
	}
	defer func() { autoUpdateCheck = originalAutoUpdateCheck }()
	devNull, err := os.OpenFile(os.DevNull, os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	originalStdout := os.Stdout
	os.Stdout = devNull
	defer func() {
		os.Stdout = originalStdout
		_ = devNull.Close()
	}()
	started := time.Now()
	if err := run([]string{"--config", configPath, "ls", "--json"}); err != nil {
		log, _ := os.ReadFile(logPath)
		t.Fatalf("requested command inherited expired update context: %v; calls=%q", err, log)
	}
	if elapsed := time.Since(started); elapsed > 10*time.Second {
		t.Fatalf("timeout isolation took too long: %v", elapsed)
	}
}

func TestDoctorToolPathFindsRegularExecutableFromPATH(t *testing.T) {
	dir := t.TempDir()
	tool := filepath.Join(dir, "hmux-e2e-doctor-tool")
	if err := os.WriteFile(tool, []byte("#!/bin/sh\nexit 0\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", dir)
	got, err := doctorToolPath("hmux-e2e-doctor-tool")
	if err != nil || got != tool {
		t.Fatalf("path=%q err=%v", got, err)
	}
	if _, err := doctorToolPath("hmux-e2e-missing-tool"); err == nil {
		t.Fatal("missing tool was reported as installed")
	}
}

func TestLauncherReturnsToSelectorWithoutAProcessRestart(t *testing.T) {
	root := t.TempDir()
	binDir := filepath.Join(root, "bin")
	if err := os.MkdirAll(binDir, 0o700); err != nil {
		t.Fatal(err)
	}
	sshLog := filepath.Join(root, "ssh.log")
	fzfCount := filepath.Join(root, "fzf.count")
	ssh := filepath.Join(binDir, "ssh")
	sshScript := `#!/bin/sh
set -eu
case "$*" in
  *" catalog" | *" catalog --launcher "*)
    printf 'catalog\n' >>"$HMUX_TEST_SSH_LOG"
    printf '%s\n' '{"protocol_version":1,"generated_at":"2026-07-30T00:00:00Z","sessions":[{"id":"$1","name":"main"}]}'
    ;;
  *" attach --launcher "*)
    printf 'attach\n' >>"$HMUX_TEST_SSH_LOG"
    ;;
  *)
    exit 91
    ;;
esac
`
	if err := os.WriteFile(ssh, []byte(sshScript), 0o700); err != nil {
		t.Fatal(err)
	}
	fzf := filepath.Join(binDir, "fzf")
	fzfScript := `#!/bin/sh
set -eu
count=0
if [ -f "$HMUX_TEST_FZF_COUNT" ]; then
  count=$(cat "$HMUX_TEST_FZF_COUNT")
fi
count=$((count + 1))
printf '%s\n' "$count" >"$HMUX_TEST_FZF_COUNT"
if [ "$count" -eq 1 ]; then
  sleep 0.05
  printf '$1\tselected\n'
  exit 0
fi
exit 130
`
	if err := os.WriteFile(fzf, []byte(fzfScript), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", binDir+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_TEST_SSH_LOG", sshLog)
	t.Setenv("HMUX_TEST_FZF_COUNT", fzfCount)
	t.Setenv("HMUX_LAUNCHER", "1")
	t.Setenv("HMUX_LAUNCHER_ID", "0123456789abcdef0123456789abcdef")
	t.Setenv("NO_COLOR", "1")
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	cfg.Timeout = 1
	cfg.StateDir = filepath.Join(root, "state")

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	err := selectAndAttach(ctx, cfg, filepath.Join(root, "client.toml"), "", false, false)
	if !errors.Is(err, ui.ErrCancelled) {
		t.Fatalf("launcher loop=%v", err)
	}
	log, readErr := os.ReadFile(sshLog)
	if readErr != nil {
		t.Fatal(readErr)
	}
	if got := string(log); got != "catalog\nattach\ncatalog\n" {
		t.Fatalf("launcher calls=%q", got)
	}
	count, readErr := os.ReadFile(fzfCount)
	if readErr != nil {
		t.Fatal(readErr)
	}
	if strings.TrimSpace(string(count)) != "2" {
		t.Fatalf("selector process count=%q", count)
	}
}

func TestResolveOpenTabsPreservesLauncherOrderAndDropsMissingSessions(t *testing.T) {
	value := model.Catalog{
		Sessions: []model.Session{
			{ID: "$1", Name: "one"},
			{ID: "$2", Name: "two"},
		},
		OpenTabs: []string{"$2", "$999", "$1"},
	}
	got := resolveOpenTabs(value)
	if len(got) != 2 || got[0].ID != "$2" || got[1].ID != "$1" {
		t.Fatalf("tabs=%#v", got)
	}
}

func TestParseWorkflowArgsAllowsDocumentedOrder(t *testing.T) {
	filter, jsonOutput, err := parseWorkflowArgs([]string{"session-name", "--json"})
	if err != nil || filter != "session-name" || !jsonOutput {
		t.Fatalf("filter=%q json=%t err=%v", filter, jsonOutput, err)
	}
	if _, _, err := parseWorkflowArgs([]string{"one", "two"}); err == nil {
		t.Fatal("multiple workflow session filters were accepted")
	}
}
