package recovery

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/model"
)

// tmux emits a four-hex-digit layout checksum followed by its geometry. Keep
// this fixture shaped like that output so validation tests exercise the format
// stored by capture rather than an invented layout representation.
const recoveryTestLayout = "b1e2,80x24,0,0,0"

func recoveryTestSnapshot() snapshot {
	return snapshot{Sessions: []savedSession{{
		Identity: model.SessionIdentity{ID: "$1", CreatedAt: 1},
		Name:     "session-one",
		Windows: []savedWindow{{
			Index:  0,
			Name:   "window-one",
			Layout: recoveryTestLayout,
			Active: true,
			Panes: []savedPane{{
				Index:  0,
				Cwd:    "/tmp/hmux-recovery-fixture",
				Active: true,
			}},
		}},
	}}}
}

func recoveryTestState() diskState {
	return diskState{
		Version:    stateVersion,
		BootID:     "test-boot-identity",
		Checkpoint: recoveryTestSnapshot(),
		UpdatedAt:  time.Date(2026, time.January, 2, 3, 4, 5, 0, time.UTC).Format(time.RFC3339Nano),
	}
}

func requireValidRecoverySnapshot(t *testing.T, value snapshot) {
	t.Helper()
	if err := validateSnapshot(value); err != nil {
		t.Fatalf("recovery test fixture must be valid: %v", err)
	}
}

func requireValidRecoveryState(t *testing.T, value diskState) {
	t.Helper()
	if err := validateState(value); err != nil {
		t.Fatalf("recovery state test fixture must be valid: %v", err)
	}
}

func TestValidateSnapshotRejectsDuplicateTmuxTopology(t *testing.T) {
	t.Run("session lifetime", func(t *testing.T) {
		value := recoveryTestSnapshot()
		requireValidRecoverySnapshot(t, value)
		duplicate := recoveryTestSnapshot().Sessions[0]
		duplicate.Name = "session-two"
		value.Sessions = append(value.Sessions, duplicate)
		if err := validateSnapshot(value); err == nil {
			t.Fatal("duplicate tmux session lifetime was accepted")
		}
	})

	t.Run("window index", func(t *testing.T) {
		value := recoveryTestSnapshot()
		requireValidRecoverySnapshot(t, value)
		duplicate := value.Sessions[0].Windows[0]
		duplicate.Name = "window-two"
		value.Sessions[0].Windows = append(value.Sessions[0].Windows, duplicate)
		if err := validateSnapshot(value); err == nil {
			t.Fatal("duplicate tmux window index was accepted")
		}
	})

	t.Run("pane index", func(t *testing.T) {
		value := recoveryTestSnapshot()
		requireValidRecoverySnapshot(t, value)
		duplicate := value.Sessions[0].Windows[0].Panes[0]
		duplicate.Cwd = "/tmp/hmux-recovery-fixture-two"
		value.Sessions[0].Windows[0].Panes = append(value.Sessions[0].Windows[0].Panes, duplicate)
		if err := validateSnapshot(value); err == nil {
			t.Fatal("duplicate tmux pane index was accepted")
		}
	})
}

func TestValidateSnapshotRejectsBoundedTopologyOverflow(t *testing.T) {
	t.Run("sessions", func(t *testing.T) {
		value := snapshot{Sessions: make([]savedSession, 0, maximumSessions+1)}
		for index := 0; index <= maximumSessions; index++ {
			session := recoveryTestSnapshot().Sessions[0]
			session.Identity = model.SessionIdentity{ID: fmt.Sprintf("$%d", index+1), CreatedAt: int64(index + 1)}
			session.Name = fmt.Sprintf("session-%d", index+1)
			value.Sessions = append(value.Sessions, session)
		}
		if err := validateSnapshot(value); err == nil {
			t.Fatal("snapshot above the session limit was accepted")
		}
	})

	t.Run("windows per session", func(t *testing.T) {
		value := recoveryTestSnapshot()
		value.Sessions[0].Windows = make([]savedWindow, 0, maximumWindowsSession+1)
		for index := 0; index <= maximumWindowsSession; index++ {
			window := recoveryTestSnapshot().Sessions[0].Windows[0]
			window.Index = index
			window.Name = fmt.Sprintf("window-%d", index)
			window.Active = index == 0
			value.Sessions[0].Windows = append(value.Sessions[0].Windows, window)
		}
		if err := validateSnapshot(value); err == nil {
			t.Fatal("snapshot above the per-session window limit was accepted")
		}
	})

	t.Run("panes per window", func(t *testing.T) {
		value := recoveryTestSnapshot()
		value.Sessions[0].Windows[0].Panes = make([]savedPane, 0, maximumPanesWindow+1)
		for index := 0; index <= maximumPanesWindow; index++ {
			pane := recoveryTestSnapshot().Sessions[0].Windows[0].Panes[0]
			pane.Index = index
			pane.Cwd = fmt.Sprintf("/tmp/hmux-recovery-pane-%d", index)
			pane.Active = index == 0
			value.Sessions[0].Windows[0].Panes = append(value.Sessions[0].Windows[0].Panes, pane)
		}
		if err := validateSnapshot(value); err == nil {
			t.Fatal("snapshot above the per-window pane limit was accepted")
		}
	})

	t.Run("total windows", func(t *testing.T) {
		value := snapshot{Sessions: make([]savedSession, 0, 9)}
		for sessionIndex := 0; sessionIndex < 9; sessionIndex++ {
			session := recoveryTestSnapshot().Sessions[0]
			session.Identity = model.SessionIdentity{ID: fmt.Sprintf("$%d", sessionIndex+1), CreatedAt: int64(sessionIndex + 1)}
			session.Name = fmt.Sprintf("session-%d", sessionIndex+1)
			session.Windows = make([]savedWindow, 0, maximumWindowsSession)
			for windowIndex := 0; windowIndex < maximumWindowsSession; windowIndex++ {
				window := recoveryTestSnapshot().Sessions[0].Windows[0]
				window.Index = windowIndex
				window.Name = fmt.Sprintf("window-%d", windowIndex)
				window.Active = windowIndex == 0
				session.Windows = append(session.Windows, window)
			}
			value.Sessions = append(value.Sessions, session)
		}
		if err := validateSnapshot(value); err == nil {
			t.Fatal("snapshot above the global window limit was accepted")
		}
	})

	t.Run("total panes", func(t *testing.T) {
		value := recoveryTestSnapshot()
		value.Sessions[0].Windows = make([]savedWindow, 0, 17)
		for windowIndex := 0; windowIndex < 17; windowIndex++ {
			window := recoveryTestSnapshot().Sessions[0].Windows[0]
			window.Index = windowIndex
			window.Name = fmt.Sprintf("window-%d", windowIndex)
			window.Active = windowIndex == 0
			window.Panes = make([]savedPane, 0, maximumPanesWindow)
			for paneIndex := 0; paneIndex < maximumPanesWindow; paneIndex++ {
				pane := recoveryTestSnapshot().Sessions[0].Windows[0].Panes[0]
				pane.Index = paneIndex
				pane.Cwd = fmt.Sprintf("/tmp/hmux-recovery-%d-%d", windowIndex, paneIndex)
				pane.Active = paneIndex == 0
				window.Panes = append(window.Panes, pane)
			}
			value.Sessions[0].Windows = append(value.Sessions[0].Windows, window)
		}
		if err := validateSnapshot(value); err == nil {
			t.Fatal("snapshot above the global pane limit was accepted")
		}
	})
}

func TestValidateSnapshotRejectsUnsafeStoredValues(t *testing.T) {
	invalidUTF8Path := string([]byte{'/', 't', 'm', 'p', '/', 0xff})
	cases := []struct {
		name   string
		mutate func(*snapshot)
	}{
		{
			name: "control character in working directory",
			mutate: func(value *snapshot) {
				value.Sessions[0].Windows[0].Panes[0].Cwd = "/tmp/hmux\nrecovery"
			},
		},
		{
			name: "control character in session name",
			mutate: func(value *snapshot) {
				value.Sessions[0].Name = "session\x00one"
			},
		},
		{
			name: "malformed tmux layout",
			mutate: func(value *snapshot) {
				value.Sessions[0].Windows[0].Layout = "b1e2,80x24,0,0,"
			},
		},
		{
			name: "resume control character",
			mutate: func(value *snapshot) {
				value.Sessions[0].Windows[0].Panes[0].Resume = &catalog.ResumeReference{Provider: "codex", SessionID: "safe-session", ConfigDir: "/tmp/hmux\tconfig"}
			},
		},
		{
			name: "resume invalid utf8 configuration directory",
			mutate: func(value *snapshot) {
				value.Sessions[0].Windows[0].Panes[0].Resume = &catalog.ResumeReference{Provider: "claude", SessionID: "safe-session", ConfigDir: invalidUTF8Path}
			},
		},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			value := recoveryTestSnapshot()
			requireValidRecoverySnapshot(t, value)
			test.mutate(&value)
			if err := validateSnapshot(value); err == nil {
				t.Fatal("unsafe snapshot value was accepted")
			}
		})
	}
}

func TestValidateStateRejectsAmbiguousOrUnboundedMappings(t *testing.T) {
	t.Run("duplicate restored source lifetime", func(t *testing.T) {
		value := recoveryTestState()
		requireValidRecoveryState(t, value)
		from := model.SessionIdentity{ID: "$10", CreatedAt: 10}
		value.Mappings = []restoredIdentity{
			{From: from, To: model.SessionIdentity{ID: "$20", CreatedAt: 20}, Name: "restored-one"},
			{From: from, To: model.SessionIdentity{ID: "$30", CreatedAt: 30}, Name: "restored-two"},
		}
		if err := validateState(value); err == nil {
			t.Fatal("ambiguous restored source lifetime was accepted")
		}
	})

	t.Run("mapping count", func(t *testing.T) {
		value := recoveryTestState()
		requireValidRecoveryState(t, value)
		value.Mappings = make([]restoredIdentity, 0, maximumSessions+1)
		for index := 0; index <= maximumSessions; index++ {
			value.Mappings = append(value.Mappings, restoredIdentity{
				From: model.SessionIdentity{ID: fmt.Sprintf("$%d", index+1), CreatedAt: int64(index + 1)},
				To:   model.SessionIdentity{ID: fmt.Sprintf("$%d", index+1001), CreatedAt: int64(index + 1001)},
				Name: fmt.Sprintf("restored-%d", index),
			})
		}
		if err := validateState(value); err == nil {
			t.Fatal("state above the mapping limit was accepted")
		}
	})
}

func TestReadStateRejectsLinkAndNonPrivateFile(t *testing.T) {
	t.Run("symbolic link", func(t *testing.T) {
		base := t.TempDir()
		root := filepath.Join(base, "recovery")
		if err := os.Mkdir(root, 0o700); err != nil {
			t.Fatal(err)
		}
		target := filepath.Join(base, "other-state.json")
		if err := os.WriteFile(target, []byte("{}\n"), 0o600); err != nil {
			t.Fatal(err)
		}
		if err := os.Symlink(target, filepath.Join(root, "state.json")); err != nil {
			t.Fatal(err)
		}
		if _, err := (Store{StateDir: base}).readState(); err == nil {
			t.Fatal("symbolic-link recovery state was accepted")
		}
	})

	t.Run("group readable file", func(t *testing.T) {
		base := t.TempDir()
		root := filepath.Join(base, "recovery")
		if err := os.Mkdir(root, 0o700); err != nil {
			t.Fatal(err)
		}
		path := filepath.Join(root, "state.json")
		if err := os.WriteFile(path, []byte("{}\n"), 0o600); err != nil {
			t.Fatal(err)
		}
		if err := os.Chmod(path, 0o644); err != nil {
			t.Fatal(err)
		}
		if _, err := (Store{StateDir: base}).readState(); err == nil {
			t.Fatal("non-private recovery state was accepted")
		}
	})
}

func TestReplacementPaneCannotReleaseProviderGate(t *testing.T) {
	runner := &scriptedRunner{t: t, steps: []runnerStep{
		{command: "display-message", output: "$2" + recoverySeparator + "2\n"},
		{command: "list-panes", output: "%99" + recoverySeparator + "0" + recoverySeparator + "0\n"},
	}}
	store := Store{Runner: runner}
	mapping := restoredIdentity{To: model.SessionIdentity{ID: "$2", CreatedAt: 2}, Panes: map[string]string{"0/0": "%7"}}
	if err := store.verifyRestoredPanes(t.Context(), mapping); err == nil {
		t.Fatal("replacement pane accepted")
	}
	runner.done()
}
