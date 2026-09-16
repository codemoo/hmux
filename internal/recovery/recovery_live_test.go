package recovery

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/sessionstate"
)

type isolatedRecoveryRunner struct {
	runner catalog.TmuxRunner
	socket string
}

func (r isolatedRecoveryRunner) Output(ctx context.Context, args ...string) ([]byte, error) {
	args = append([]string(nil), args...)
	if len(args) > 0 && args[0] == "new-session" {
		for i := 0; i+1 < len(args); i++ {
			if args[i] == "-s" && args[i+1] == "hmux" {
				args[i+1] = "hmux-e2e-default"
			}
		}
	}

	return r.runner.Output(ctx, append([]string{"-L", r.socket, "-f", "/dev/null"}, args...)...)
}

func TestRecoveryWithIsolatedTmuxAndFakeProviders(t *testing.T) {
	if os.Getenv("HMUX_RUN_RECOVERY_TMUX_TEST") != "1" {
		t.Skip("isolated tmux recovery acceptance is opt-in")
	}
	tmux, err := catalog.TmuxPath()
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	defer cancel()
	runner := isolatedRecoveryRunner{catalog.TmuxRunner{Path: tmux}, fmt.Sprintf("hmux-e2e-recovery-%d", os.Getpid())}
	t.Cleanup(func() {
		bounded, c := context.WithTimeout(context.Background(), 3*time.Second)
		defer c()
		_, _ = runner.Output(bounded, "kill-server")
	})
	command := func(args ...string) string {
		t.Helper()
		raw, err := runner.Output(ctx, args...)
		if err != nil {
			t.Fatalf("isolated tmux %s failed: %v", args[0], err)
		}
		return strings.TrimSpace(string(raw))
	}
	logs := filepath.Join(dir, "launches")
	for _, provider := range []string{"codex", "claude"} {
		script := "#!/bin/sh\nprintf '%s|%s|%s\\n' '" + provider + "' \"$1\" \"$2\" >> '" + logs + "'\nexec /bin/sleep 300\n"
		if err := os.WriteFile(filepath.Join(dir, provider), []byte(script), 0700); err != nil {
			t.Fatal(err)
		}
	}
	t.Setenv("PATH", dir+":/usr/bin:/bin:/usr/sbin:/sbin")
	configDir := filepath.Join(dir, "provider config")
	if err := os.Mkdir(configDir, 0700); err != nil {
		t.Fatal(err)
	}
	command("new-session", "-d", "-s", "hmux-e2e-recover", "-n", "one", "-c", dir, "/bin/sleep", "300")
	command("split-window", "-d", "-t", "hmux-e2e-recover:one", "-c", dir, "/bin/sleep", "300")
	command("new-window", "-d", "-t", "hmux-e2e-recover:4", "-n", "two", "-c", dir, "/bin/sleep", "300")
	command("select-window", "-t", "hmux-e2e-recover:4")
	boot := "boot-a"
	store := Store{StateDir: filepath.Join(dir, "state"), Runner: runner, BootID: func(context.Context) (string, error) { return boot, nil }, Bind: func(_ context.Context, pids []int) (map[int]catalog.ResumeReference, error) {
		refs := map[int]catalog.ResumeReference{}
		for index, pid := range pids {
			provider := "codex"
			if index%2 == 1 {
				provider = "claude"
			}
			refs[pid] = catalog.ResumeReference{Provider: provider, SessionID: fmt.Sprintf("session-%d", index), ConfigDir: configDir}
		}
		return refs, nil
	}}
	old, err := catalog.ReadBasic(ctx, runner)
	if err != nil || len(old.Sessions) != 1 {
		t.Fatal("initial isolated catalog failed")
	}
	if err := (sessionstate.Store{StateDir: store.StateDir}).SetAlias(old.Sessions[0], "Restored alias"); err != nil {
		t.Fatal(err)
	}
	if err := store.Sync(ctx); err != nil {
		t.Fatal(err)
	}
	before, err := store.readState()
	if err != nil {
		t.Fatal(err)
	}
	if len(before.Checkpoint.Sessions) != 1 || len(before.Checkpoint.Sessions[0].Windows) != 2 {
		t.Fatal("topology checkpoint incomplete")
	}
	command("kill-server") // this dedicated socket contains only this test's resources
	time.Sleep(1100 * time.Millisecond)
	boot = "boot-b"
	store.Bind = func(context.Context, []int) (map[int]catalog.ResumeReference, error) {
		return map[int]catalog.ResumeReference{}, nil
	}
	// Simulate process death after tmux committed creation but before the
	// caller could persist its returned identity. No live provider has started.
	store.Runner = crashAfterNewSession{runner}
	func() {
		defer func() {
			if recover() == nil {
				t.Fatal("crash point not reached")
			}
		}()
		_ = store.Sync(ctx)
	}()
	store.Runner = runner
	if err := store.Sync(ctx); err != nil {
		t.Fatal(err)
	}
	var log []byte
	for end := time.Now().Add(5 * time.Second); time.Now().Before(end); {
		log, _ = os.ReadFile(logs)
		if len(strings.Fields(string(log))) == 3 {
			break
		}
		time.Sleep(50 * time.Millisecond)
	}
	if len(strings.Fields(string(log))) != 3 || !strings.Contains(string(log), "codex|resume|session-") || !strings.Contains(string(log), "claude|--resume|session-") {
		t.Fatalf("fake providers did not resume exactly once: %q", log)
	}
	current, err := catalog.ReadBasic(ctx, runner)
	if err != nil || len(current.Sessions) != 1 || current.Sessions[0].WindowCount != 2 || current.Sessions[0].ActiveWindow != "two" {
		t.Fatal("recovered topology differs")
	}
	if err := store.Apply(&current); err != nil {
		t.Fatal(err)
	}
	if current.Sessions[0].RestoredFrom == nil || current.Sessions[0].RestoredFrom.ID != old.Sessions[0].ID {
		t.Fatal("verified lineage missing")
	}
	if err := (sessionstate.Store{StateDir: store.StateDir}).Apply(&current); err != nil {
		t.Fatal(err)
	}
	if current.Sessions[0].Alias != "Restored alias" {
		t.Fatal("alias not restored")
	}
	if err := store.Sync(ctx); err != nil {
		t.Fatal(err)
	}
	if err := store.Restore(ctx); err != nil {
		t.Fatal(err)
	}
	if err := store.Restore(ctx); err != nil {
		t.Fatal(err)
	}
	mapped, err := catalog.ReadBasic(ctx, runner)
	if err != nil {
		t.Fatal(err)
	}
	if err := store.Apply(&mapped); err != nil || mapped.Sessions[0].RestoredFrom == nil {
		t.Fatal("repeated manual restore erased lineage")
	}
	after, _ := os.ReadFile(logs)
	if string(after) != string(log) {
		t.Fatal("same-boot sync relaunched providers")
	}
	// A deliberately deleted final session is not resurrected on the next boot.
	command("kill-server")
	if err := store.Save(ctx); err != nil {
		t.Fatal(err)
	}
	if err := store.Sync(ctx); err != nil {
		t.Fatal(err)
	}
	sameBoot, err := catalog.ReadBasic(ctx, runner)
	if err != nil || len(sameBoot.Sessions) != 0 {
		t.Fatal("same-boot deliberate deletion restarted tmux")
	}
	boot = "boot-c"
	if err := store.Sync(ctx); err != nil {
		t.Fatal(err)
	}
	empty, err := catalog.ReadBasic(ctx, runner)
	if err != nil || len(empty.Sessions) != 1 || empty.Sessions[0].Name != "hmux-e2e-default" {
		t.Fatal("empty reboot did not start the default shell")
	}
	t.Log("isolated reboot recovery: windows/panes, fixed Codex+Claude resumes, alias, lineage, idempotence, delete-all passed")
}

type crashAfterNewSession struct{ catalog.Runner }

func (r crashAfterNewSession) Output(ctx context.Context, args ...string) ([]byte, error) {
	raw, err := r.Runner.Output(ctx, args...)
	if err == nil && len(args) > 0 && args[0] == "new-session" {
		panic("synthetic process death after tmux creation")
	}
	return raw, err
}
