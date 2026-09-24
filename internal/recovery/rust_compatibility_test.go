package recovery

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/model"
)

// The actual Go serializer/validator and lock owner exchange current state with
// a separately built Rust process. All paths, tmux output and provider IDs are synthetic.
func TestRustCurrentRecoveryState(t *testing.T) {
	binary := os.Getenv("HMUX_RUST_RECOVERY_ORACLE")
	if binary == "" {
		t.Skip("requires separately built Rust recovery_oracle")
	}
	root, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(root, 0700); err != nil {
		t.Fatal(err)
	}
	tmux := filepath.Join(root, "fake-tmux")
	script := "#!/bin/sh\nroot=${0%/*}\nif [ -f \"$root/slow\" ]; then printf x > \"$root/entered\"; /bin/sleep 1; fi\ncase \"$1\" in\nlist-sessions|list-windows|list-panes) /bin/cat \"$root/$1.out\" ;;\n*) exit 91 ;;\nesac\n"
	if err := os.WriteFile(tmux, []byte(script), 0700); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"list-sessions", "list-windows", "list-panes"} {
		if err := os.WriteFile(filepath.Join(root, name+".out"), nil, 0600); err != nil {
			t.Fatal(err)
		}
	}
	runner := &scriptedRunner{t: t, steps: []runnerStep{{command: "list-sessions"}, {command: "list-windows"}}}
	store := Store{StateDir: root, Runner: runner, BootID: func(context.Context) (string, error) { return "synthetic-boot", nil }, Bind: func(context.Context, []int) (map[int]catalog.ResumeReference, error) {
		return map[int]catalog.ResumeReference{}, nil
	}}
	var reference *catalog.ResumeReference
	run := func(operation string, value model.Catalog) (model.Catalog, error) {
		q := map[string]any{"root": root, "tmux": tmux, "boot_id": "synthetic-boot", "operation": operation, "catalog": value, "reference": reference}
		raw, _ := json.Marshal(q)
		ctx, cancel := context.WithTimeout(t.Context(), 6*time.Second)
		defer cancel()
		cmd := exec.CommandContext(ctx, binary)
		cmd.Stdin = bytes.NewReader(raw)
		out, err := cmd.Output()
		if err != nil {
			return model.Catalog{}, err
		}
		var result model.Catalog
		err = json.Unmarshal(out, &result)
		return result, err
	}
	if err := store.Sync(t.Context()); err != nil {
		t.Fatal(err)
	}
	runner.done()
	if _, err := run("save", model.Catalog{}); err != nil {
		t.Fatal("Rust rejected Go empty array checkpoint:", err)
	}
	// Nil slices are also valid current Go state (for example after cloning an
	// empty pending snapshot), so exercise the actual Go serializer for both.
	if err := store.writeState(diskState{BootID: "synthetic-boot"}); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(root, "recovery", "state.json")
	raw, _ := os.ReadFile(path)
	if !bytes.Contains(raw, []byte(`"sessions":null`)) {
		t.Fatal("Go empty checkpoint no longer exercises null")
	}
	if _, err := run("save", model.Catalog{}); err != nil {
		t.Fatal("Rust rejected Go empty checkpoint:", err)
	}
	if current, err := store.readState(); err != nil || len(current.Checkpoint.Sessions) != 0 {
		t.Fatalf("Go rejected Rust empty checkpoint: %v", err)
	}

	steps := captureSteps(101, "$1", "synthetic", 100, "%3")
	for _, step := range steps[:3] {
		if err := os.WriteFile(filepath.Join(root, step.command+".out"), []byte(step.output), 0600); err != nil {
			t.Fatal(err)
		}
	}
	runner.steps = steps
	reference = &catalog.ResumeReference{Provider: "codex", SessionID: "synthetic-123", ConfigDir: "/synthetic/codex"}
	store.Bind = func(context.Context, []int) (map[int]catalog.ResumeReference, error) {
		return map[int]catalog.ResumeReference{101: *reference}, nil
	}
	if err := store.Save(t.Context()); err != nil {
		t.Fatal(err)
	}
	before, err := store.readState()
	if err != nil {
		t.Fatal(err)
	}
	if _, err := run("save", model.Catalog{}); err != nil {
		t.Fatal("Rust rejected populated Go checkpoint:", err)
	}
	after, err := store.readState()
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(before.Checkpoint, after.Checkpoint) {
		t.Fatalf("checkpoint mismatch\nGo %#v\nRust %#v", before.Checkpoint, after.Checkpoint)
	}
	// Each implementation excludes the other using the same persistent lock inode.
	if err := store.withLock(t.Context(), func() error {
		if _, err := run("save", model.Catalog{}); err == nil {
			t.Fatal("Rust bypassed the Go recovery lock")
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "slow"), nil, 0600); err != nil {
		t.Fatal(err)
	}
	finished := make(chan error, 1)
	go func() { _, err := run("save", model.Catalog{}); finished <- err }()
	deadline := time.Now().Add(2 * time.Second)
	for {
		if _, err := os.Stat(filepath.Join(root, "entered")); err == nil {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("Rust capture did not start")
		}
		time.Sleep(10 * time.Millisecond)
	}
	bounded, cancel := context.WithTimeout(t.Context(), 100*time.Millisecond)
	err = store.Save(bounded)
	cancel()
	if err == nil {
		t.Fatal("Go bypassed the Rust recovery lock")
	}
	if err := os.Remove(filepath.Join(root, "slow")); err != nil {
		t.Fatal(err)
	}
	if err := <-finished; err != nil {
		t.Fatal("Rust capture failed after lock check:", err)
	}

	// Go can persist null maps during an interrupted restore. Rust must read them,
	// retain the pending work, and apply lineage only to the exact target lifetime.
	after.Pending = &pendingRestore{BootID: "synthetic-boot", Snapshot: after.Checkpoint}
	after.Mappings = []restoredIdentity{{From: model.SessionIdentity{ID: "$7", CreatedAt: 70}, To: model.SessionIdentity{ID: "$1", CreatedAt: 100}, Name: "synthetic"}}
	if err := store.writeState(after); err != nil {
		t.Fatal(err)
	}
	value := model.Catalog{Sessions: []model.Session{{ID: "$1", CreatedAt: 100, Name: "synthetic"}, {ID: "$1", CreatedAt: 101, Name: "synthetic"}, {ID: "$1", CreatedAt: 100, Name: "renamed"}}}
	rust, err := run("apply", value)
	if err != nil {
		t.Fatal("Rust rejected pending Go state:", err)
	}
	if err := store.Apply(&value); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(rust.Sessions, value.Sessions) {
		t.Fatal("restored lifetime projection differs")
	}
	raw, err = os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := run("save", model.Catalog{}); err == nil {
		t.Fatal("Rust saved over pending restore")
	}
	unchanged, _ := os.ReadFile(path)
	if !bytes.Equal(raw, unchanged) {
		t.Fatal("rejected save changed pending state")
	}
}

func TestRustRecoveryWithIsolatedTmuxAndFakeProviders(t *testing.T) {
	binary, tmux := os.Getenv("HMUX_RUST_RECOVERY_ORACLE"), os.Getenv("HMUX_TEST_TMUX")
	if binary == "" || tmux == "" {
		t.Skip("requires Rust recovery_oracle and explicit HMUX_TEST_TMUX; isolated sockets only")
	}
	for _, provider := range []string{"codex", "claude"} {
		t.Run(provider, func(t *testing.T) {
			// macOS Unix sockets have a short pathname limit; Go's normal test
			// temporary directory includes the long test name.
			temp, err := os.MkdirTemp("/tmp", "hmux-e2e-rec-")
			if err != nil {
				t.Fatal(err)
			}
			t.Cleanup(func() { _ = os.RemoveAll(temp) })
			root, err := filepath.EvalSymlinks(temp)
			if err != nil {
				t.Fatal(err)
			}
			if err := os.Chmod(root, 0700); err != nil {
				t.Fatal(err)
			}
			bin := filepath.Join(root, "bin")
			if err := os.Mkdir(bin, 0700); err != nil {
				t.Fatal(err)
			}
			socket := fmt.Sprintf("hmux-e2e-rust-recovery-%s-%d", provider, os.Getpid())
			env := []string{"HOME=" + root, "PATH=" + bin + ":/usr/bin:/bin", "SHELL=/bin/sh", "TERM=xterm-256color", "TMUX_TMPDIR=" + root, "HMUX_TEST_REAL_TMUX=" + tmux}
			query := func(args ...string) ([]byte, error) {
				ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
				defer cancel()
				cmd := exec.CommandContext(ctx, tmux, append([]string{"-L", socket, "-f", "/dev/null"}, args...)...)
				cmd.Env = env
				return cmd.CombinedOutput()
			}
			t.Cleanup(func() {
				ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
				defer cancel()
				cmd := exec.CommandContext(ctx, tmux, "-L", socket, "kill-server")
				cmd.Env = env
				_ = cmd.Run()
			})
			// Fail one topology operation before gate release, then retry using the
			// persisted pending intent. Only this test's isolated tmux server exists.
			wrapper := filepath.Join(bin, "fake-tmux")
			script := "#!/bin/sh\nfor arg do\nif [ \"$arg\" = select-layout ] && [ -f \"$HOME/fail-once\" ]; then /bin/rm \"$HOME/fail-once\"; exit 71; fi\ndone\nexec \"$HMUX_TEST_REAL_TMUX\" -f /dev/null \"$@\"\n"
			if err := os.WriteFile(wrapper, []byte(script), 0700); err != nil {
				t.Fatal(err)
			}
			script = "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HOME/provider-args\"\nprintf '%s\\n' \"${CODEX_HOME:-${CLAUDE_CONFIG_DIR:-}}\" > \"$HOME/provider-home\"\n/bin/cp \"$HOME/state/recovery/state.json\" \"$HOME/provider-state.json\"\nprintf x >> \"$HOME/provider-starts\"\nexit 7\n"
			if err := os.WriteFile(filepath.Join(bin, provider), []byte(script), 0700); err != nil {
				t.Fatal(err)
			}
			if out, err := query("new-session", "-d", "-s", "hmux-e2e-original", "-c", root, "/bin/sh", "-i"); err != nil {
				t.Fatalf("isolated tmux: %s %v", out, err)
			}
			stateRoot := filepath.Join(root, "state")
			ref := catalog.ResumeReference{Provider: provider, SessionID: "synthetic-record-123", ConfigDir: filepath.Join(root, "provider-config")}
			if err := os.Mkdir(ref.ConfigDir, 0700); err != nil {
				t.Fatal(err)
			}
			run := func(boot string) error {
				raw, _ := json.Marshal(map[string]any{"root": stateRoot, "tmux": wrapper, "socket_name": socket, "boot_id": boot, "operation": "sync", "reference": ref})
				ctx, cancel := context.WithTimeout(t.Context(), 35*time.Second)
				defer cancel()
				cmd := exec.CommandContext(ctx, binary)
				cmd.Env = env
				cmd.Stdin = bytes.NewReader(raw)
				out, err := cmd.CombinedOutput()
				if err != nil {
					return fmt.Errorf("%v: %s", err, out)
				}
				return nil
			}
			if err := run("synthetic-boot-a"); err != nil {
				t.Fatal(err)
			}
			store := Store{StateDir: stateRoot}
			before, err := store.readState()
			if err != nil {
				t.Fatal(err)
			}
			if out, err := query("kill-server"); err != nil {
				t.Fatalf("stop isolated server: %s %v", out, err)
			}
			deadline := time.Now().Add(2 * time.Second)
			for {
				if _, err := query("list-sessions"); err != nil {
					break
				}
				if time.Now().After(deadline) {
					t.Fatal("isolated server did not stop")
				}
				time.Sleep(10 * time.Millisecond)
			}
			if err := os.WriteFile(filepath.Join(root, "fail-once"), nil, 0600); err != nil {
				t.Fatal(err)
			}
			if err := run("synthetic-boot-b"); err == nil {
				t.Fatal("injected interrupted restore was not reported")
			}
			pending, err := store.readState()
			if err != nil || pending.Pending == nil {
				t.Fatalf("pending state lost: %v", err)
			}
			if _, err := os.Stat(filepath.Join(root, "provider-starts")); !os.IsNotExist(err) {
				t.Fatal("provider launched before restore commit")
			}
			if err := run("synthetic-boot-b"); err != nil {
				t.Fatal("retry failed:", err)
			}
			deadline = time.Now().Add(3 * time.Second)
			for {
				if _, err := os.Stat(filepath.Join(root, "provider-starts")); err == nil {
					break
				}
				if time.Now().After(deadline) {
					t.Fatal("fake provider did not launch")
				}
				time.Sleep(10 * time.Millisecond)
			}
			after, err := store.readState()
			if err != nil {
				t.Fatal(err)
			}
			if after.Pending != nil || len(after.Mappings) != 1 || after.Mappings[0].From != before.Checkpoint.Sessions[0].Identity {
				t.Fatal("exact recovery lineage missing")
			}
			observedRaw, err := os.ReadFile(filepath.Join(root, "provider-state.json"))
			if err != nil {
				t.Fatal(err)
			}
			var observed diskState
			if err := json.Unmarshal(observedRaw, &observed); err != nil {
				t.Fatal(err)
			}
			if len(observed.Mappings) == 0 && (observed.Pending == nil || len(observed.Pending.Completed) == 0) {
				t.Fatal("provider ran before mapping was persisted")
			}
			args, err := os.ReadFile(filepath.Join(root, "provider-args"))
			if err != nil {
				t.Fatal(err)
			}
			flag := "resume"
			if provider == "claude" {
				flag = "--resume"
			}
			if string(args) != flag+"\n"+ref.SessionID+"\n" {
				t.Fatalf("wrong resume arguments: %q", args)
			}
			home, _ := os.ReadFile(filepath.Join(root, "provider-home"))
			if strings.TrimSpace(string(home)) != ref.ConfigDir {
				t.Fatal("provider configuration not preserved")
			}
			deadline = time.Now().Add(3 * time.Second)
			for {
				out, err := query("display-message", "-p", "-t", after.Mappings[0].To.ID+":", "#{pane_dead}|#{pane_current_command}")
				// /bin/sh reports its implementation name (bash on macOS, dash
				// on some Linux hosts) through pane_current_command.
				state := strings.TrimSpace(string(out))
				if err == nil && (state == "0|sh" || state == "0|bash" || state == "0|dash") {
					break
				}
				if time.Now().After(deadline) {
					t.Fatalf("shell did not survive provider exit: %s %v", out, err)
				}
				time.Sleep(10 * time.Millisecond)
			}
			if err := run("synthetic-boot-b"); err != nil {
				t.Fatal(err)
			}
			starts, _ := os.ReadFile(filepath.Join(root, "provider-starts"))
			if string(starts) != "x" {
				t.Fatal("same-boot sync duplicated provider launch")
			}
		})
	}
}
