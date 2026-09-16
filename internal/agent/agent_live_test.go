package agent

import (
	"context"
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
	ctx, cancel := context.WithTimeout(t.Context(), 15*time.Second)
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
	if first.Reused || !reused.Reused || first.ID != reused.ID || first.CreatedAt != reused.CreatedAt {
		t.Fatalf("detached named reuse changed identity: first=%+v reused=%+v", first, reused)
	}
	automaticIDs := map[string]bool{first.ID: true}
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
	if len(value.Sessions) != 3 {
		t.Fatalf("expected three separate sessions, got %d", len(value.Sessions))
	}
	for _, session := range value.Sessions {
		if !strings.HasPrefix(session.Name, "hmux-e2e-") || !automaticIDs[session.ID] || session.Attached != 0 {
			t.Fatalf("unexpected session in the private test server: %+v", session)
		}
	}
}
