package agent

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/model"
)

func TestCreateSessionWithIsolatedTmux(t *testing.T) {
	if os.Getenv("HMUX_RUN_TMUX_CREATE_TEST") != "1" {
		t.Skip("isolated tmux creation integration is opt-in")
	}
	tmuxPath, err := exec.LookPath("tmux")
	if err != nil {
		t.Skip("tmux is unavailable")
	}
	root, err := os.MkdirTemp("/tmp", "hmux-e2e-create-")
	if err != nil {
		t.Fatal(err)
	}
	socket := filepath.Join(root, "tmux.sock")
	t.Cleanup(func() {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = exec.CommandContext(ctx, tmuxPath, "-S", socket, "kill-server").Run()
		_ = os.RemoveAll(root)
	})
	// Every command, including the production lookup through PATH, is bound
	// to this test's private socket and an empty tmux configuration.
	wrapper := filepath.Join(root, "tmux")
	script := "#!/bin/sh\nexec " + shellCommand([]string{tmuxPath, "-S", socket, "-f", "/dev/null"}) + " \"$@\"\n"
	if err := os.WriteFile(wrapper, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", root+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("TMUX", "")
	t.Setenv("TMUX_PANE", "")
	ctx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
	defer cancel()
	inventory := model.Inventory{Profiles: []model.Profile{{
		ID: "hmux-e2e-shell", Label: "Test shell", DefaultDirectory: root,
		Command: []string{"sleep", "120"},
	}}}
	stateDir := filepath.Join(root, "state")
	const name = "hmux-e2e-reuse 한글"
	first, err := CreateSession(ctx, inventory, "hmux-e2e-shell", name, stateDir)
	if err != nil {
		t.Fatal(err)
	}
	reused, err := CreateSession(ctx, inventory, "hmux-e2e-shell", name, stateDir)
	if err != nil {
		t.Fatal(err)
	}
	if first.Reused || reused.Reused || first.ID == reused.ID {
		t.Fatalf("named creation reused work: first=%+v second=%+v", first, reused)
	}
	automaticIDs := map[string]bool{first.ID: true, reused.ID: true}
	for range 2 {
		created, err := CreateSession(ctx, inventory, "hmux-e2e-shell", "", stateDir)
		if err != nil {
			t.Fatal(err)
		}
		if created.Reused || automaticIDs[created.ID] {
			t.Fatalf("automatic creation reused an existing session: %+v", created)
		}
		automaticIDs[created.ID] = true
	}
	value, err := catalog.ReadBasic(ctx, catalog.TmuxRunner{Path: wrapper})
	if err != nil {
		t.Fatal(err)
	}
	if len(value.Sessions) != 4 {
		t.Fatalf("expected four separate sessions, got %d", len(value.Sessions))
	}
	for _, session := range value.Sessions {
		if !strings.HasPrefix(session.Name, "hmux-e2e-") || !automaticIDs[session.ID] || session.Attached != 0 {
			t.Fatalf("unexpected session in the private test server: %+v", session)
		}
	}
}

func TestProvidersExitToShellWithIsolatedTmux(t *testing.T) {
	if os.Getenv("HMUX_RUN_TMUX_CREATE_TEST") != "1" {
		t.Skip("isolated tmux creation integration is opt-in")
	}
	tmux, err := exec.LookPath("tmux")
	if err != nil {
		t.Skip("tmux unavailable")
	}
	root, err := os.MkdirTemp("/tmp", "hmux-e2e-exit-")
	if err != nil {
		t.Fatal(err)
	}
	socket := filepath.Join(root, "tmux.sock")
	t.Cleanup(func() { _ = exec.Command(tmux, "-S", socket, "kill-server").Run(); _ = os.RemoveAll(root) })
	script := "#!/bin/sh\nexec " + shellCommand([]string{tmux, "-S", socket, "-f", "/dev/null"}) + " \"$@\"\n"
	if err := os.WriteFile(filepath.Join(root, "tmux"), []byte(script), 0700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", root+":"+os.Getenv("PATH"))
	t.Setenv("SHELL", "/bin/sh")
	t.Setenv("TMUX", "")
	t.Setenv("TMUX_PANE", "")
	ctx, cancel := context.WithTimeout(t.Context(), 20*time.Second)
	defer cancel()
	for _, provider := range []string{"codex", "claude"} {
		for _, status := range []int{0, 7, 130, -1, -2} {
			// Fake provider validates literal argv, writes CWD, then exits with each status.
			body := fmt.Sprintf("#!/bin/sh\n[ \"$1\" = 'has space; $(false)' ] || exit 99\npwd > provider-cwd\nexit %d\n", status)
			if status == -1 {
				body = "#!/bin/sh\npwd > provider-cwd\nexec /bin/sleep 300\n"
			}
			if status == -2 {
				body = "#!/bin/sh\ntrap 'printf signal > interrupted' INT\npwd > provider-cwd\nwhile [ ! -f exit-provider ]; do /bin/sleep 0.05; done\nexit 0\n"
			}
			if err := os.WriteFile(filepath.Join(root, provider), []byte(body), 0700); err != nil {
				t.Fatal(err)
			}
			inventory := model.Inventory{Profiles: []model.Profile{{ID: provider, DefaultDirectory: root, Command: []string{provider, "has space; $(false)"}}}}
			result, err := CreateSession(ctx, inventory, provider, "hmux-e2e-한글 / project", filepath.Join(root, "state"))
			if err != nil {
				t.Fatal(err)
			}
			var current string
			for deadline := time.Now().Add(3 * time.Second); time.Now().Before(deadline); {
				raw, err := exec.CommandContext(ctx, tmux, "-S", socket, "display-message", "-p", "-t", result.ID+":", "#{pane_current_path}").Output()
				if err != nil {
					t.Fatal(err)
				}
				current = strings.TrimSpace(string(raw))
				if _, err := os.Stat(filepath.Join(current, "provider-cwd")); err == nil {
					break
				}
				time.Sleep(20 * time.Millisecond)
			}
			if status < 0 {
				if err := exec.CommandContext(ctx, tmux, "-S", socket, "send-keys", "-t", result.ID+":", "C-c").Run(); err != nil {
					t.Fatal(err)
				}
			}
			// Send a command to the resulting interactive shell, never to a real provider.
			if err := exec.CommandContext(ctx, tmux, "-S", socket, "send-keys", "-t", result.ID+":", "printf shell-ready > shell-ready", "Enter").Run(); err != nil {
				t.Fatal(err)
			}
			ready := filepath.Join(current, "shell-ready")
			if status == -2 {
				for deadline := time.Now().Add(3 * time.Second); time.Now().Before(deadline); {
					if _, err := os.Stat(filepath.Join(current, "interrupted")); err == nil {
						break
					}
					time.Sleep(20 * time.Millisecond)
				}
				if _, err := os.Stat(filepath.Join(current, "interrupted")); err != nil {
					t.Fatal("provider did not handle Ctrl+C")
				}
				if _, err := os.Stat(ready); !os.IsNotExist(err) {
					t.Fatal("shell started while provider was still running")
				}
				if err := os.WriteFile(filepath.Join(current, "exit-provider"), []byte("exit"), 0600); err != nil {
					t.Fatal(err)
				}
			}
			for deadline := time.Now().Add(3 * time.Second); time.Now().Before(deadline); {
				if _, err := os.Stat(ready); err == nil {
					break
				}
				time.Sleep(20 * time.Millisecond)
			}
			data, err := os.ReadFile(ready)
			if err != nil || string(data) != "shell-ready" {
				t.Fatalf("%s exit %d did not leave a usable shell: %v", provider, status, err)
			}
			physicalRoot, _ := filepath.EvalSymlinks(root)
			if filepath.Dir(current) != physicalRoot || !strings.HasPrefix(filepath.Base(current), "hmux-e2e-한글-project") {
				t.Fatalf("unexpected cwd %q", current)
			}
		}
	}
}
